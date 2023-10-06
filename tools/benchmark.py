import argparse
import json
import time
import threading

import kubernetes.client

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from utils import (
    Kube,
    MainConfig,
    get_main_config,
    get_logger,
    get_namespaces,
    run_subprocess,
)
from tempfile import TemporaryFile

LOG = get_logger(__name__)
BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def run_benchmark(
    namespace: str, region: str, sets: list[str], delete_only: bool, dry_run: bool
):
    with TemporaryFile() as helm_output:
        helm_cmd = [
            "helm",
            "template",
            (BASE_PATH / "helm-benchbase").as_posix(),
            "--namespace",
            namespace,
        ]
        for s in sets:
            helm_cmd += ["--set", s]
        run_subprocess(
            helm_cmd,
            (LOG, f"[{namespace}] Generating Kubernetes configs from Helm templates"),
            dry_run=False,
            check=True,
            stdout=helm_output,
        )
        helm_output.flush()

        helm_output.seek(0)
        LOG.debug(f"Helm output: {helm_output.read().decode()}")

        context = Kube.get_context(BASE_PATH, region)

        # Delete the existing deployment, if any
        helm_output.seek(0)
        run_subprocess(
            [
                "kubectl",
                "delete",
                "--namespace",
                namespace,
                "-f",
                "-",
                "--context",
                context,
            ],
            (LOG, f"[{namespace}] Deleting existing benchmark"),
            dry_run,
            stdin=helm_output,
            check=False,
        )

        if delete_only:
            return

        # Apply the new deployment
        helm_output.seek(0)
        run_subprocess(
            [
                "kubectl",
                "apply",
                "--namespace",
                namespace,
                "-f",
                "-",
                "--context",
                context,
            ],
            (LOG, f"[{namespace}] Creating new benchmark"),
            dry_run,
            stdin=helm_output,
            check=True,
        )


def get_job_logs(namespace: str, region: str, job: str):
    kube_config = Kube.get_config(BASE_PATH, region)
    with kubernetes.client.ApiClient(kube_config) as api_client:
        batchv1 = kubernetes.client.BatchV1Api(api_client)

        job_info = batchv1.read_namespaced_job(name=job, namespace=namespace)
        selector = job_info.spec.selector.match_labels

    pods: list[str] = []
    while len(pods) == 0:
        try:
            pods = Kube.get_pods(
                kube_config,
                namespace,
                selector,
                phases=["Running", "Pending", "Succeeded"],
            )
            if len(pods) > 1:
                raise Exception(f'More than one pod found for job "{job}"')
        except kubernetes.client.rest.ApiException as e:
            body = json.loads(e.body)
            LOG.warning(
                f'Getting pods "{job}" in namespace "{namespace}": {body["message"]}.'
            )
            time.sleep(1)

    if not pods:
        raise Exception(f'No pods found for job "{job}" in namespace "{namespace}"')

    attempt = 10
    while attempt > 0:
        try:
            return Kube.get_logs(
                kube_config,
                namespace,
                pods[0],
                follow=True,
            )
        except kubernetes.client.rest.ApiException as e:
            body = json.loads(e.body)
            LOG.warning(
                f'Getting logs for "{job}" in namespace "{namespace}": {body["message"]}.'
            )
            time.sleep(6)
            attempt -= 1

    raise Exception(f'Could not get logs for job "{job}" in namespace "{namespace}"')


class Operation:
    config: MainConfig
    namespaces: list[tuple[str, dict[str, str | int]]]
    settings: list[str]
    dry_run: bool
    exit_event: threading.Event

    @classmethod
    def run(cls, args, exit_event: threading.Event):
        cls.config = get_main_config(BASE_PATH)
        cls.namespaces = list(get_namespaces(cls.config).items())
        cls.namespaces.sort(key=lambda x: x[1]["id"])

        cls.settings = args.set or []
        ordered_namespaces = ",".join([ns for ns, _ in cls.namespaces])
        cls.settings.append(f"ordered_namespaces={{{ordered_namespaces}}}")
        for ns, ns_info in cls.namespaces:
            for k, v in ns_info.items():
                cls.settings.append(f"namespaces.{ns}.{k}={v}")

        cls.dry_run = args.dry_run
        cls.exit_event = exit_event

        if not args.logs_only:
            cls.do()

        if not args.dry_run:
            cls.log()

    @classmethod
    def do(cls):
        raise NotImplementedError()

    @classmethod
    def log(cls):
        raise NotImplementedError()


class Create(Operation):
    @classmethod
    def do(cls):
        run_benchmark(
            "global",
            cls.config["global_region"],
            cls.settings + ["operation=create"],
            delete_only=False,
            dry_run=cls.dry_run,
        )

    @classmethod
    def log(cls):
        logs = get_job_logs("global", cls.config["global_region"], "create-load")
        Kube.print_logs(
            Kube.NamedLogs(
                namespace="global",
                name="create",
                stream=logs,
            ),
            follow=True,
            exit_event=cls.exit_event,
        )


class Load(Operation):
    @classmethod
    def do(cls):
        max_workers = len(cls.namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            for id, (namespace, ns_info) in enumerate(cls.namespaces):
                executor.submit(
                    run_benchmark,
                    namespace,
                    ns_info["region"],
                    cls.settings
                    + [
                        "operation=load",
                        "loadall=false",
                        f"namespace_id={id}",
                    ],
                    delete_only=False,
                    dry_run=cls.dry_run,
                )

    @classmethod
    def log(cls):
        max_workers = len(cls.namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            named_logs = executor.map(
                lambda item: Kube.NamedLogs(
                    namespace=item[0],
                    name="load",
                    stream=get_job_logs(item[0], item[1]["region"], "create-load"),
                ),
                cls.namespaces,
            )

        Kube.print_logs(named_logs, follow=True, exit_event=cls.exit_event)


class SLoad(Operation):
    @classmethod
    def do(cls):
        run_benchmark(
            "global",
            cls.config["global_region"],
            cls.settings + ["operation=load", "loadall=true"],
            delete_only=False,
            dry_run=cls.dry_run,
        )

    @classmethod
    def log(cls):
        logs = get_job_logs("global", cls.config["global_region"], "create-load")
        Kube.print_logs(
            Kube.NamedLogs(
                namespace="global",
                name="sload",
                stream=logs,
            ),
            follow=True,
            exit_event=cls.exit_event,
        )


class Execute(Operation):
    @classmethod
    def do(cls):
        timestamp = time.strftime("%Y-%m-%d_%H-%M-%S")
        max_workers = len(cls.namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            for id, (namespace, ns_info) in enumerate(cls.namespaces):
                executor.submit(
                    run_benchmark,
                    namespace,
                    ns_info["region"],
                    cls.settings
                    + [
                        "operation=execute",
                        f"timestamp={timestamp}",
                        f"namespace_id={id}",
                    ],
                    delete_only=False,
                    dry_run=cls.dry_run,
                )

    @classmethod
    def log(cls):
        max_workers = len(cls.namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            named_logs = executor.map(
                lambda item: Kube.NamedLogs(
                    namespace=item[0],
                    name="execute",
                    stream=get_job_logs(item[0], item[1]["region"], "execute"),
                ),
                cls.namespaces,
            )

        Kube.print_logs(named_logs, follow=True, exit_event=cls.exit_event)


class Delete(Operation):
    @classmethod
    def do(cls):
        run_benchmark(
            "global",
            cls.config["global_region"],
            cls.settings + ["operation=create"],
            delete_only=True,
            dry_run=cls.dry_run,
        )

        max_workers = len(cls.namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            for namespace, ns_info in cls.namespaces:
                executor.submit(
                    run_benchmark,
                    namespace,
                    ns_info["region"],
                    cls.settings + ["operation=load"],
                    delete_only=True,
                    dry_run=cls.dry_run,
                )
                executor.submit(
                    run_benchmark,
                    namespace,
                    ns_info["region"],
                    cls.settings + ["operation=execute"],
                    delete_only=True,
                    dry_run=cls.dry_run,
                )

    @classmethod
    def log(cls):
        pass


def main(cmd_args: list[str]):
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "operation",
        choices=["create", "load", "sload", "execute", "delete"],
        help="""The operation to run.
        (create: run the DDL script, 
        load: load the data in parallel,
        sload: load the data sequentially,
        execute: execute the benchmark,
        delete: terminate all running benchmarks,
        """,
    )
    parser.add_argument(
        "--set",
        "-s",
        action="append",
        help="Override the values in the config file. Each argument should be in the form of key=value.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Do not actually run the benchmark.",
    )
    parser.add_argument(
        "--logs-only",
        "-l",
        action="store_true",
        help="Only print the logs of the benchmark.",
    )

    args = parser.parse_args(cmd_args)

    exit_event = threading.Event()

    if args.operation == "create":
        Create.run(args, exit_event)
    elif args.operation == "sload":
        SLoad.run(args, exit_event)
    elif args.operation == "load":
        Load.run(args, exit_event)
    elif args.operation == "execute":
        Execute.run(args, exit_event)
    elif args.operation == "delete":
        Delete.run(args, exit_event)
        exit_event.set()
    else:
        raise ValueError(f"Unknown operation: {args.operation}")

    if not args.dry_run:
        exit_event.wait()

    return 0


if __name__ == "__main__":
    import sys

    exit(main(sys.argv[1:]))
