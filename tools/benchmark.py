import argparse
import os
import subprocess

from pathlib import Path
from utils import get_regions, get_logger, get_context
from tempfile import TemporaryFile

LOG = get_logger(__name__)
BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def run_benchmark(region, namespace, sets, args):
    sets_args = []
    for arg in sets:
        sets_args.extend(["--set", arg])

    helm_cmd = [
        "helm",
        "template",
        (BASE_PATH / "helm-benchbase").as_posix(),
        "--namespace",
        namespace,
    ] + sets_args

    LOG.debug(f"Executing: {' '.join(helm_cmd)}")
    with TemporaryFile() as helm_output:
        subprocess.run(
            helm_cmd,
            stdout=helm_output,
            check=True,
        )
        helm_output.flush()

        helm_output.seek(0)
        LOG.debug(f"Helm output: {helm_output.read().decode()}")

        context = get_context(BASE_PATH, region)

        # Delete the existing deployment, if any
        cmd = [
            "kubectl",
            "delete",
            "--namespace",
            namespace,
            "-f",
            "-",
            "--context",
            context,
        ]
        LOG.debug(f"Executing: {' '.join(cmd)}")
        helm_output.seek(0)
        if not args.dry_run:
            subprocess.run(
                cmd,
                stdin=helm_output,
                check=False,
            )

        # Apply the new deployment
        cmd = [
            "kubectl",
            "apply",
            "--namespace",
            namespace,
            "-f",
            "-",
            "--context",
            context,
        ]
        LOG.debug(f"Executing: {' '.join(cmd)}")
        helm_output.seek(0)
        if not args.dry_run:
            subprocess.run(
                cmd,
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

    regions_info = get_regions(BASE_PATH)
    regions = regions_info["regions"]
    global_region = regions_info["global_region"]

    sets = args.set or []
    sets.append(f"regions={{global,{','.join(regions)}}}")

    if args.operation == "create":
        run_benchmark(global_region, "global", ["operation=create"] + sets, args)
    elif args.operation == "load":
        run_benchmark(global_region, "global", ["operation=load"] + sets, args)
    elif args.operation == "execute":
        for region in regions_info["regions"]:
            LOG.info("Executing benchmark in region %s", region)
            run_benchmark(
                region,
                region,
                ["operation=execute"] + sets,
                args,
            )
