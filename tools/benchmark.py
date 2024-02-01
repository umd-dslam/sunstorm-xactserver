import argparse
import json
import re
import time
import threading

import kubernetes.client

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from tempfile import TemporaryFile
from typing import TypedDict, NamedTuple

from utils import (
    Kube,
    MainConfig,
    NamespaceInfo,
    get_main_config,
    get_logger,
    get_namespaces,
    run_subprocess,
)

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
            # Per-region values are in the form of key=!region1:value1;region2=value2;region3=value3
            if "=!" in s:
                key, value = s.split("=!")
                per_region_values = value.split(";")
                for per_region_value in per_region_values:
                    if ":" not in per_region_value:
                        raise ValueError(
                            f"Invalid per-region value: {per_region_value}"
                        )
                    target_region, val = per_region_value.split(":")
                    if target_region.strip() == region:
                        helm_cmd += ["--set", f"{key}={val.strip()}"]
            else:
                helm_cmd += ["--set", s]
        if dry_run:
            helm_cmd += ["--debug"]
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


def get_job_logs(namespace: str, region: str, job: str, dry_run: bool):
    if dry_run:
        return [f'[dry-run] logs for "{job}" in region "{region}"'.encode()]

    kube_config = Kube.get_config(BASE_PATH, region)
    with kubernetes.client.ApiClient(kube_config) as api_client:  # type: ignore
        batchv1 = kubernetes.client.BatchV1Api(api_client)

        job_info = batchv1.read_namespaced_job(name=job, namespace=namespace)
        assert (
            job_info.spec
            and job_info.spec.selector
            and job_info.spec.selector.match_labels
        )
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
        except kubernetes.client.rest.ApiException as e:  # type: ignore
            body = json.loads(e.body)
            LOG.warning(
                f'Getting pods "{job}" in namespace "{namespace}": {body["message"]}.'
            )
            time.sleep(1)

    if not pods:
        raise Exception(f'No pods found for job "{job}" in namespace "{namespace}"')

    if len(pods) > 1:
        LOG.warning(f'More than one pod found for job "{job}". Picking only one arbitrarily')

    attempt = 10
    while attempt > 0:
        try:
            return Kube.get_logs(
                kube_config,
                namespace,
                pods[0],
                follow=True,
            )
        except kubernetes.client.rest.ApiException as e:  # type: ignore
            body = json.loads(e.body)
            LOG.warning(
                f'Getting logs for "{job}" in namespace "{namespace}": {body["message"]}.'
            )
            time.sleep(6)
            attempt -= 1

    raise Exception(f'Could not get logs for job "{job}" in namespace "{namespace}"')


class Namespace(NamedTuple):
    name: str
    info: NamespaceInfo


class BenchmarkResult(TypedDict):
    throughput: float
    goodput: float


class Operation:
    config: MainConfig
    namespaces: list[Namespace]
    settings: list[str]
    dry_run: bool
    logs_per_sec: int

    @classmethod
    def run(cls, args) -> BenchmarkResult | None:
        cls.config = get_main_config(BASE_PATH)
        namespaces = get_namespaces(cls.config)

        # Namespace sorted by id
        cls.namespaces = [Namespace(*item) for item in namespaces.items()]
        cls.namespaces.sort(key=lambda ns: ns.info["id"])

        # Settings for the values.yaml file in helm-benchbase
        cls.settings = args.set or []
        ordered_namespaces = ",".join([ns for ns, _ in cls.namespaces])
        cls.settings.append(f"ordered_namespaces={{{ordered_namespaces}}}")
        for ns, ns_info in cls.namespaces:
            for k, v in ns_info.items():
                cls.settings.append(f"namespaces.{ns}.{k}={v}")

        # Other settings
        cls.dry_run = args.dry_run
        cls.logs_per_sec = args.logs_per_sec

        # Run the operation
        if not args.logs_only:
            cls.do(args.regions)

        # Fetch the log of the operation
        return cls.log(args.regions)

    @classmethod
    def do(cls, regions: list[str]):
        raise NotImplementedError()

    @classmethod
    def log(cls, regions: list[str]) -> BenchmarkResult | None:
        return None


class Create(Operation):
    @classmethod
    def do(cls, regions: list[str]):
        region = cls.config["global_region"]
        if regions and region not in regions:
            return

        additional_settings = ["operation=create"]
        target = cls.config.get("global_region_target_address_and_database")
        if target:
            additional_settings.append(f"target_address_and_database={target}")

        run_benchmark(
            "global",
            region,
            cls.settings + additional_settings,
            delete_only=False,
            dry_run=cls.dry_run,
        )

    @classmethod
    def log(cls, regions) -> BenchmarkResult | None:
        region = cls.config["global_region"]
        if regions and region not in regions:
            return None

        named_logs = Kube.NamedLogs(
            namespace="global",
            name="create",
            stream=get_job_logs("global", region, "create-load", cls.dry_run),
        )

        exit_event = threading.Event()
        Kube.print_logs(named_logs, follow=True, exit_event=exit_event)
        exit_event.wait()

        return None


class Load(Operation):
    @classmethod
    def do(cls, regions: list[str]):
        max_workers = len(cls.namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            for id, (namespace, ns_info) in enumerate(cls.namespaces):
                region = ns_info["region"]
                if regions and region not in regions:
                    continue

                per_region_settings = [
                    "operation=load",
                    "loadall=false",
                    f"namespace_id={id}",
                ]
                target = ns_info["target_address_and_database"]
                if target:
                    per_region_settings.append(f"target_address_and_database={target}")
                executor.submit(
                    run_benchmark,
                    namespace,
                    region,
                    cls.settings + per_region_settings,
                    delete_only=False,
                    dry_run=cls.dry_run,
                )

    @classmethod
    def log(cls, regions: list[str]) -> BenchmarkResult | None:
        max_workers = len(cls.namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            filtered_namespaces = [
                ns
                for ns in cls.namespaces
                if not regions or ns.info["region"] in regions
            ]
            named_logs = executor.map(
                lambda ns: Kube.NamedLogs(
                    namespace=ns.name,
                    name="load",
                    stream=get_job_logs(
                        ns.name, ns.info["region"], "create-load", cls.dry_run
                    ),
                ),
                filtered_namespaces,
            )

        exit_event = threading.Event()
        Kube.print_logs(named_logs, follow=True, exit_event=exit_event)
        exit_event.wait()

        return None


class SLoad(Operation):
    @classmethod
    def do(cls, regions: list[str]):
        region = cls.config["global_region"]
        if regions and region not in regions:
            return

        additional_settings = ["operation=load", "loadall=true"]
        target = cls.config["global_region_target_address_and_database"]
        if target:
            additional_settings.append(f"target_address_and_database={target}")
        run_benchmark(
            "global",
            region,
            cls.settings + additional_settings,
            delete_only=False,
            dry_run=cls.dry_run,
        )

    @classmethod
    def log(cls, regions: list[str]) -> BenchmarkResult | None:
        region = cls.config["global_region"]
        if regions and region not in regions:
            return

        named_logs = Kube.NamedLogs(
            namespace="global",
            name="sload",
            stream=get_job_logs("global", region, "create-load", cls.dry_run),
        )

        exit_event = threading.Event()
        Kube.print_logs(
            named_logs,
            follow=True,
            exit_event=exit_event,
        )
        exit_event.wait()

        return None


class Execute(Operation):
    @classmethod
    def namespaces_without_global(cls, regions: list[str]):
        return [
            ns
            for ns in cls.namespaces
            if ns.name != "global" and (not regions or ns.info["region"] in regions)
        ]

    @classmethod
    def do(cls, regions: list[str]):
        timestamp = time.strftime("%Y-%m-%d_%H-%M-%S")
        namespaces = cls.namespaces_without_global(regions)
        max_workers = len(namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            for ns in namespaces:
                region = ns.info["region"]
                if regions and region not in regions:
                    continue

                namespace_id = ns.info["id"]
                per_region_settings = [
                    "operation=execute",
                    f"timestamp={timestamp}",
                    f"namespace_id={namespace_id}",
                ]
                target = ns.info["target_address_and_database"]
                if target:
                    per_region_settings.append(f"target_address_and_database={target}")
                warmup = ns.info["warmup"]
                if warmup:
                    per_region_settings.append(f"warmup={warmup}")

                executor.submit(
                    run_benchmark,
                    ns.name,
                    region,
                    cls.settings + per_region_settings,
                    delete_only=False,
                    dry_run=cls.dry_run,
                )

    @classmethod
    def log(cls, regions: list[str]) -> BenchmarkResult | None:
        max_workers = len(cls.namespaces)
        exit_event = threading.Event()
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            named_logs = executor.map(
                lambda ns: Kube.NamedLogs(
                    namespace=ns.name,
                    name="execute",
                    stream=get_job_logs(
                        ns.name, ns.info["region"], "execute", cls.dry_run
                    ),
                ),
                cls.namespaces_without_global(regions),
            )

        lock = threading.Lock()
        matched_logs = []
        throughput = 0.0
        goodput = 0.0

        pattern = re.compile(
            r"Rate limited reqs/s: Results\(.+\) = (\d+\.\d+|\d+) requests/sec \(throughput\), "
            r"(\d+\.\d+|\d+) requests/sec \(goodput\)"
        )

        def parse_and_add(name: str, log: str):
            nonlocal throughput
            nonlocal goodput
            nonlocal lock

            match = pattern.search(log)
            if match:
                with lock:
                    matched_logs.append(f"[{name}] {log}")
                    throughput += float(match.group(1))
                    goodput += float(match.group(2))

        Kube.print_logs(
            named_logs,
            follow=True,
            callback=parse_and_add,
            logs_per_sec=cls.logs_per_sec,
            exit_event=exit_event,
        )

        exit_event.wait()
        LOG.info("Throughput: %.2f req/s. Goodput: %.2f req/s ", throughput, goodput)
        LOG.info("\n".join(sorted(matched_logs)))

        return {"throughput": throughput, "goodput": goodput}


class Delete(Operation):
    @classmethod
    def do(cls, regions: list[str]):
        global_region = cls.config["global_region"]
        if not regions or global_region in regions:
            run_benchmark(
                "global",
                global_region,
                cls.settings + ["operation=create"],
                delete_only=True,
                dry_run=cls.dry_run,
            )

        max_workers = len(cls.namespaces)
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            for ns in cls.namespaces:
                region = ns.info["region"]
                if regions and region not in regions:
                    continue

                executor.submit(
                    run_benchmark,
                    ns.name,
                    region,
                    cls.settings + ["operation=load"],
                    delete_only=True,
                    dry_run=cls.dry_run,
                )
                executor.submit(
                    run_benchmark,
                    ns.name,
                    region,
                    cls.settings + ["operation=execute"],
                    delete_only=True,
                    dry_run=cls.dry_run,
                )


def main(cmd_args: list[str]) -> BenchmarkResult:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "operation",
        choices=["create", "load", "sload", "execute", "delete"],
        help="""The operation to run.
        (create: run the DDL script, 
        load: load the data in parallel,
        sload: load the data sequentially,
        execute: execute the benchmark,
        delete: terminate all running benchmarks)
        """,
    )
    parser.add_argument(
        "--set",
        "-s",
        action="append",
        help="Override the values in the config file. Each argument should be in the form of key=value.",
    )
    parser.add_argument(
        "--regions",
        "-r",
        nargs="*",
        help="The regions to run the benchmark in. If not specified, run in all regions.",
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
    parser.add_argument(
        "--logs-per-sec",
        default=0,
        type=int,
        help="The maximum number of log lines to print per second.",
    )

    args = parser.parse_args(cmd_args)

    result = None
    if args.operation == "create":
        result = Create.run(args)
    elif args.operation == "sload":
        result = SLoad.run(args)
    elif args.operation == "load":
        result = Load.run(args)
    elif args.operation == "execute":
        result = Execute.run(args)
    elif args.operation == "delete":
        result = Delete.run(args)
    else:
        raise ValueError(f"Unknown operation: {args.operation}")

    return result or {"throughput": 0, "goodput": 0}


if __name__ == "__main__":
    import sys

    main(sys.argv[1:])
