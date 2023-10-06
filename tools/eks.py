import argparse
import itertools
import time
import subprocess
import yaml

from collections import namedtuple
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from rich.console import Console
from utils import get_main_config, Kube, COLORS


BASE_PATH = Path(__file__).parent.resolve() / "deploy"

CONSOLE = Console()

RegionInfo = namedtuple("RegionInfo", ["name", "color", "is_global"])


def run_subprocess_and_print_log(cmd: list[str], info: RegionInfo, dry_run: bool):
    CONSOLE.log(f"Running: {' '.join(cmd)}")

    if dry_run:
        return

    with subprocess.Popen(cmd, stdout=subprocess.PIPE) as proc:
        assert proc.stdout is not None
        for line in proc.stdout:
            decoded = line.decode("utf-8").rstrip("\n")
            CONSOLE.print(
                f"[bold]\[{info.name}][/bold] {decoded}",
                style=info.color,
                highlight=False,
            )


def create_eks_cluster(info: RegionInfo, dry_run: bool):
    eks_config_file = BASE_PATH / f"eks/{info.name}.yaml"

    # Create the cluster
    run_subprocess_and_print_log(
        [
            "eksctl",
            "create",
            "cluster",
            "--config-file",
            eks_config_file.as_posix(),
        ],
        info,
        dry_run,
    )

    if not dry_run:
        CONSOLE.log(
            f"EKS cluster in {info.name} is up. "
            f"Context: {Kube.get_context(BASE_PATH, info.name)}."
        )


def delete_eks_cluster(info: RegionInfo, dry_run: bool):
    eks_config_file = BASE_PATH / f"eks/{info.name}.yaml"

    if info.is_global:
        # The cluster deletion will be stuck if the EBS CSI driver
        # is not deleted first.
        with open(eks_config_file, "r") as yaml_file:
            eks_config = yaml.safe_load(yaml_file)
        run_subprocess_and_print_log(
            [
                "eksctl",
                "delete",
                "addon",
                "--name",
                "aws-ebs-csi-driver",
                "--cluster",
                eks_config["metadata"]["name"],
                "--region",
                info.name,
            ],
            info,
            dry_run,
        )

    # Delete the cluster
    run_subprocess_and_print_log(
        [
            "eksctl",
            "delete",
            "cluster",
            "--force",
            "--config-file",
            eks_config_file.as_posix(),
        ],
        info,
        dry_run,
    )

    CONSOLE.log(f"EKS cluster in {info.name} is deleted.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "action",
        choices=["create", "delete"],
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the commands that would be executed without actually executing them.",
    )
    args = parser.parse_args()

    if args.action == "create":
        action_fn = create_eks_cluster
    elif args.action == "delete":
        action_fn = delete_eks_cluster
    else:
        raise ValueError(f"Unknown action: {args.action}")

    config = get_main_config(BASE_PATH)
    global_region = config["global_region"]
    regions = set(config["regions"])
    regions.add(global_region)

    colors = itertools.cycle(COLORS)

    with ThreadPoolExecutor() as executor:
        infos = [
            RegionInfo(region, next(colors), region == global_region)
            for region in regions
        ]
        tasks = [executor.submit(action_fn, info, args.dry_run) for info in infos]
        with CONSOLE.status("[bold green]Waiting..."):
            while any(task.running() for task in tasks):
                time.sleep(1)

        # Consume the exceptions if any
        for task in tasks:
            task.result()
