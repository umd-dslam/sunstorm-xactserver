import argparse
import os
import subprocess

import yaml

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import List
from utils import get_logger, spin_while, reset_spinner, get_regions, get_context

LOG = get_logger(
    __name__,
    level=os.environ.get("LOG_LEVEL", "INFO").upper(),
    fmt="%(asctime)s %(threadName)s %(levelname)5s - %(message)s",
)

BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def parallel_eksctl(action: str, regions: List[str], dry_run: bool) -> str:
    def create_eks_cluster(region: str):
        eks_config_file = f"eks/{region}.yaml"
        eks_log_file = f"eks/{region}.log"

        reset_spinner()
        LOG.info(
            f'Running "{action}" on EKS cluster in: {eks_config_file}. See logs in {eks_log_file}'
        )
        cmd = [
            "eksctl",
            action,
            "cluster",
            "-f",
            (BASE_PATH / eks_config_file).as_posix(),
        ]
        reset_spinner()
        LOG.debug(f"Executing: {' '.join(cmd)}")
        if not dry_run:
            with open(BASE_PATH / eks_log_file, "w") as log_file:
                subprocess.run(
                    cmd,
                    check=True,
                    stdout=log_file,
                    stderr=subprocess.STDOUT,
                )
            if action == "create":
                LOG.info(
                    f"EKS cluster in {region} is up. Context: {get_context(region)}."
                )

    with ThreadPoolExecutor() as executor:
        tasks = [executor.submit(create_eks_cluster, region) for region in regions]
        spin_while(lambda _: any(task.running() for task in tasks))
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
