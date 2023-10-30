import argparse
import itertools
import subprocess
import yaml

from collections import namedtuple
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from rich.console import Console
from rich.prompt import Confirm
from utils import get_main_config, Kube, COLORS


BASE_PATH = Path(__file__).parent.resolve()
WORKSPACE_PATH = BASE_PATH / "workspace"
DEPLOY_PATH = BASE_PATH / "deploy"

CONSOLE = Console()

RegionInfo = namedtuple("RegionInfo", ["name", "color", "is_global"])


def run_subprocess(
    cmd: list[str], info: RegionInfo, print_log: bool = True, dry_run: bool = False
):
    CONSOLE.log(f"Running: {' '.join(cmd)}")

    if dry_run:
        return []

    stdout = []
    with subprocess.Popen(cmd, stdout=subprocess.PIPE) as proc:
        assert proc.stdout is not None
        for line in proc.stdout:
            decoded = line.decode("utf-8").rstrip("\n")
            if print_log:
                CONSOLE.print(
                    f"[bold]\\[{info.name}][/bold] {decoded}",
                    style=info.color,
                    highlight=False,
                )
            stdout.append(decoded)

    return stdout


def generate_eks_configs(regions: list[RegionInfo]):
    WORKSPACE_PATH.mkdir(parents=True, exist_ok=True)

    global_region_name = next((region.name for region in regions if region.is_global), None)
    for region in regions:
        stdout = run_subprocess(
            [
                "helm",
                "template",
                (BASE_PATH / "deploy" / "helm-eks").as_posix(),
                "--set",
                f"region={region.name}",
                "--set",
                f"global_region={global_region_name}",
            ],
            region,
            print_log=False,
        )
        with open(WORKSPACE_PATH / f"eks-{region.name}.yaml", "w") as yaml_file:
            yaml_file.write("\n".join(stdout))


def create_eks_cluster(info: RegionInfo, dry_run: bool):
    eks_config_file = WORKSPACE_PATH / f"eks-{info.name}.yaml"
    dry_run_arg = ["--dry-run"] if dry_run else []

    # Create the cluster
    run_subprocess(
        [
            "eksctl",
            "create",
            "cluster",
            "--config-file",
            eks_config_file.as_posix(),
        ]
        + dry_run_arg,
        info,
    )

    if not dry_run:
        CONSOLE.log(
            f"EKS cluster in {info.name} is up. "
            f"Context: {Kube.get_context(DEPLOY_PATH, info.name)}."
        )


def delete_eks_cluster(info: RegionInfo, dry_run: bool):
    eks_config_file = WORKSPACE_PATH / f"eks-{info.name}.yaml"

    if info.is_global:
        # The cluster deletion will be stuck if the EBS CSI driver
        # is not deleted first.
        with open(eks_config_file, "r") as yaml_file:
            eks_config = yaml.safe_load(yaml_file)
        run_subprocess(
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
            dry_run=dry_run,
        )

    # Delete the cluster
    run_subprocess(
        [
            "eksctl",
            "delete",
            "cluster",
            "--force",
            "--config-file",
            eks_config_file.as_posix(),
        ],
        info,
        dry_run=dry_run,
    )

    CONSOLE.log(f"EKS cluster in {info.name} is deleted.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "action",
        choices=["create", "delete", "generate"],
    )
    parser.add_argument(
        "--base-path",
        action="store", 
        type=str,
        help="Base path containing the cluster configs."
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the commands that would be executed without actually executing them.",
    )
    args = parser.parse_args()

    if args.base_path:
        BASE_PATH = Path(__file__).parent.resolve() / args.base_path
        WORKSPACE_PATH = BASE_PATH / "workspace"
        DEPLOY_PATH = BASE_PATH / "deploy"
    
    config = get_main_config(BASE_PATH / "deploy")

    regions = set(config["regions"] or [])
    if "global_region" in config:
        global_region = config["global_region"]
        regions.add(global_region)
    else:
        global_region = None

    generate_only = False
    action_fn = lambda info, dry_run: None

    if args.action == "create":
        action_fn = create_eks_cluster
    elif args.action == "delete":
        region_names = ", ".join(regions)
        if not Confirm.ask(
            f"Do you want to delete the EKS clusters in regions: {region_names}?",
            default=False,
        ):
            exit(0)
        action_fn = delete_eks_cluster
    elif args.action == "generate":
        generate_only = True
    else:
        raise ValueError(f"Unknown action: {args.action}")

    colors = itertools.cycle(COLORS)
    infos = [
        RegionInfo(region, next(colors), region == global_region) for region in regions
    ]

    generate_eks_configs(infos)

    if generate_only:
        exit(0)

    with ThreadPoolExecutor() as executor:
        results = executor.map(lambda info: action_fn(info, args.dry_run), infos)

        with CONSOLE.status("[bold green]Waiting..."):
            list(results)
