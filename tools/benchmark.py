import argparse
import json
import time
import threading
import signal

import kubernetes.client

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from utils import (
    Kube,
    get_main_config,
    get_logger,
    get_namespaces,
    run_subprocess,
)
from tempfile import TemporaryFile

LOG = get_logger(__name__)
BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def run_benchmark(namespace, region, sets, dry_run):
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


def get_job_logs(namespace, region, job):
    kube_config = Kube.get_config(BASE_PATH, region)
    with kubernetes.client.ApiClient(kube_config) as api_client:
        batchv1 = kubernetes.client.BatchV1Api(api_client)

        job_info = batchv1.read_namespaced_job(name=job, namespace=namespace)
        selector = job_info.spec.selector.match_labels

    pods = []
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


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "operation",
        choices=["create", "load", "sload", "execute"],
        help="""The operation to run.
        (create: run the DDL script, 
        load: load the data in parallel,
        sload: load the data sequentially,
        execute: execute the benchmark)
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

    args = parser.parse_args()

    config = get_main_config(BASE_PATH)
    regions = config["regions"]

    sets = args.set or []

    namespaces = get_namespaces(config)
    ordered_namespaces = [
        item[0] for item in sorted(namespaces.items(), key=lambda x: x[1]["id"])
    ]
    for ns, ns_info in namespaces.items():
        for k, v in ns_info.items():
            sets.append(f"namespaces.{ns}.{k}={v}")

    sets.append(f"ordered_namespaces={{{','.join(ordered_namespaces)}}}")

    exit_event = threading.Event()

    global_region = config["global_region"]

    if args.operation == "create" or args.operation == "sload":
        if not args.logs_only:
            run_benchmark(
                "global",
                global_region,
                [f"operation={args.operation}", "parallel_load=false"] + sets,
                args.dry_run,
            )

        if not args.dry_run:
            logs = get_job_logs("global", global_region, "create-load")
            Kube.print_logs(
                Kube.NamedLogs(
                    namespace="global",
                    name=args.operation,
                    stream=logs,
                ),
                follow=True,
                exit_event=exit_event,
            )

    elif args.operation == "load":
        if not args.logs_only:
            with ThreadPoolExecutor(max_workers=len(regions)) as executor:
                for region in regions:
                    namespace_id = ordered_namespaces.index(region)
                    executor.submit(
                        run_benchmark,
                        region,
                        region,
                        [
                            "operation=load",
                            "parallel_load=true",
                            f"namespace_id={namespace_id}",
                        ]
                        + sets,
                        args.dry_run,
                    )

        if not args.dry_run:
            with ThreadPoolExecutor(max_workers=len(regions)) as executor:
                named_logs = executor.map(
                    lambda region: Kube.NamedLogs(
                        namespace=region,
                        name="load",
                        stream=get_job_logs(region, region, "create-load"),
                    ),
                    regions,
                )

            Kube.print_logs(named_logs, follow=True, exit_event=exit_event)

    elif args.operation == "execute":
        if not args.logs_only:
            timestamp = time.strftime("%Y-%m-%d_%H-%M-%S")
            with ThreadPoolExecutor(max_workers=len(regions)) as executor:
                for region in regions:
                    namespace_id = ordered_namespaces.index(region)
                    executor.submit(
                        run_benchmark,
                        region,
                        region,
                        [
                            "operation=execute",
                            f"timestamp={timestamp}",
                            f"namespace_id={namespace_id}",
                        ]
                        + sets,
                        args.dry_run,
                    )

        if not args.dry_run:
            with ThreadPoolExecutor(max_workers=len(regions)) as executor:
                named_logs = executor.map(
                    lambda region: Kube.NamedLogs(
                        namespace=region,
                        name="execute",
                        stream=get_job_logs(region, region, "execute"),
                    ),
                    regions,
                )

            Kube.print_logs(named_logs, follow=True, exit_event=exit_event)
    else:
        raise ValueError(f"Unknown operation: {args.operation}")

    if not args.dry_run:
        # Wait for Ctrl-C
        signal.signal(signal.SIGINT, lambda *args: exit_event.set())
        exit_event.wait()
