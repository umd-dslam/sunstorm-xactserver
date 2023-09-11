import argparse
import time

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from utils import (
    get_main_config,
    get_logger,
    get_context,
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

        context = get_context(BASE_PATH, region)

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


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("operation", choices=["create", "load", "execute"])
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

    if args.operation == "create":
        run_benchmark(
            "global", config["global_region"], ["operation=create"] + sets, args.dry_run
        )
    elif args.operation == "load":
        run_benchmark(
            "global", config["global_region"], ["operation=load"] + sets, args.dry_run
        )
    elif args.operation == "execute":
        timestamp = time.strftime("%Y-%m-%d_%H-%M-%S")
        with ThreadPoolExecutor(max_workers=len(regions)) as executor:
            for region in regions:
                executor.submit(
                    run_benchmark,
                    region,
                    region,
                    ["operation=execute", f"timestamp={timestamp}"] + sets,
                    args.dry_run,
                )
