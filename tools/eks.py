import argparse
import os
import time
import subprocess

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import List
from rich.console import Console
from utils import get_logger, get_regions, get_context


BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def parallel_eksctl(action: str, regions: List[str], dry_run: bool) -> str:
    console = Console()

    def create_eks_cluster(region: str):
        eks_config_file = f"eks/{region}.yaml"
        eks_log_file = f"eks/{region}.log"

        cmd = [
            "eksctl",
            action,
            "cluster",
            "-f",
            (BASE_PATH / eks_config_file).as_posix(),
        ]

        console.log(f"Running: {' '.join(cmd)}. See logs in {eks_log_file}.")

        if not dry_run:
            with open(BASE_PATH / eks_log_file, "w") as log_file:
                subprocess.run(
                    cmd,
                    check=True,
                    stdout=log_file,
                    stderr=subprocess.STDOUT,
                )
            if action == "create":
                console.log(
                    f"EKS cluster in {region} is up. Context: {get_context(region)}."
                )

    with ThreadPoolExecutor() as executor:
        tasks = [executor.submit(create_eks_cluster, region) for region in regions]
        with console.status("[bold green]Waiting for the EKS clusters..."):
            while any(task.running() for task in tasks):
                time.sleep(1)

        # Consume the exceptions if any
        for task in tasks:
            task.result()


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

    info = get_regions(BASE_PATH)

    parallel_eksctl(args.action, info["regions"], args.dry_run)
