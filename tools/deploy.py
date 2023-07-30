import argparse
import os
import tempfile
import time
import subprocess

import boto3
import dns.resolver
import yaml

from pathlib import Path
from typing import List
from utils import get_logger, spin_while

LOG = get_logger(
    __name__,
    level=os.environ.get("LOG_LEVEL", "INFO").upper(),
)

REGIONS_YAML = "regions.yaml"
BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def try_with_timeout(fn, timeout: int):
    start_time = time.time()
    while True:
        result = fn()
        if result is not None:
            return result

        if time.time() - start_time >= timeout:
            raise TimeoutError(
                f"Timeout: {fn.__name__} did not return within {timeout} seconds."
            )

        # Wait for 5 seconds before trying again
        spin_while(lambda elapsed: elapsed < 5)


def get_context(region: str) -> str:
    kube_context = "-"
    kube_config_file = Path.home() / ".kube" / "config"
    with open(kube_config_file, "r") as kube_config_file:
        kube_config = yaml.safe_load(kube_config_file)
        for ctx in kube_config["contexts"]:
            if region in ctx["name"]:
                kube_context = ctx["name"]
                break

    return kube_context


def set_up_load_balancer_for_coredns(regions: List[str], dry_run: bool) -> str:
    for region in regions:
        LOG.info(f"Creating load balancer for CoreDNS in region {region}.")
        cmd = [
            "kubectl",
            "apply",
            "--context",
            get_context(region),
            "-f",
            (BASE_PATH / "eks" / "dns-lb-eks.yaml").as_posix(),
        ]

        LOG.debug(f"Executing: {' '.join(cmd)}")
        if not dry_run:
            subprocess.run(
                cmd,
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.STDOUT,
            )
        LOG.info(f"Load balancer for CoreDNS in region {region} created.")


def install_dns_configmap(regions: List[str], global_region: str, dry_run: bool):
    TIMEOUT = 300

    region_lb_ip_addresses = {"addresses": {}}

    for region in regions:
        LOG.info(f"Fetching CoreDNS load balancer ip addresses for region: {region}")

        if dry_run:
            continue

        elb_client = boto3.client("elbv2", region_name=region)

        def get_load_balancer_public_addresses():
            dns_name = []

            response = elb_client.describe_load_balancers()
            dns_name = [lb["DNSName"] for lb in response["LoadBalancers"]]

            if not dns_name:
                return None

            return dns_name[0]

        lb_dns_name = try_with_timeout(get_load_balancer_public_addresses, TIMEOUT)

        def get_load_balancer_ip_addresses():
            ip_addresses = []
            try:
                answers = dns.resolver.resolve(lb_dns_name, "A")
                ip_addresses = [rdata.address for rdata in answers]
            except Exception:
                return None

            return ip_addresses

        lb_ip_addresses = try_with_timeout(get_load_balancer_ip_addresses, TIMEOUT)

        region_lb_ip_addresses["addresses"][region] = lb_ip_addresses

    dns_config = tempfile.NamedTemporaryFile(
        mode="w", delete=False, prefix="eks-lb-dns-", suffix=".yaml"
    )
    yaml.dump(region_lb_ip_addresses, dns_config)
    LOG.info(f"Helm config for DNS configmap written to: {dns_config.name}")

    for region in regions:
        context = get_context(region)
        helm_name = f"dns-{region}"
        cmd = [
            "helm",
            "uninstall",
            helm_name,
            "--kube-context",
            context,
        ]
        LOG.info(
            f"Uninstalling possibly existing CoreDNS configmap in region: {region}"
        )
        LOG.debug(f"Executing: {' '.join(cmd)}")
        if not dry_run:
            subprocess.run(cmd, check=False)

        cmd = [
            "kubectl",
            "delete",
            "configmap",
            "coredns",
            "--context",
            context,
            "--namespace",
            "kube-system",
        ]
        LOG.info(f"Deleting possibly existing CoreDNS configmap in region: {region}")
        LOG.debug(f"Executing: {' '.join(cmd)}")
        if not dry_run:
            subprocess.run(cmd, check=False)

        cmd = [
            "helm",
            "install",
            helm_name,
            "--kube-context",
            context,
            "-f",
            dns_config.name,
            "--set",
            f"region={region},global_region={global_region}",
            (BASE_PATH / "helm-dns").as_posix(),
        ]
        LOG.info(f"Installing new CoreDNS configmap in region: {region}")
        LOG.debug(f"Executing: {' '.join(cmd)}")
        if not dry_run:
            subprocess.run(cmd, check=True)


def create_namespaces(regions: List[str], global_region: str, dry_run: bool):
    def create_namespace(region, namespace):
        cmd = [
            "kubectl",
            "create",
            "namespace",
            "--context",
            get_context(region),
            namespace,
        ]
        LOG.info(f'Creating namespace "{namespace}" in {region}')
        LOG.debug(f"Executing: {' '.join(cmd)}")
        if not dry_run:
            subprocess.run(cmd, check=False)

    create_namespace(global_region, "global")
    for region in regions:
        create_namespace(region, region)


def deploy_neon(
    regions: List[str], global_region: str, cleanup_only: bool, dry_run: bool
):
    def clean_up_neon_one_namespace(region, namespace):
        cmd = [
            "helm",
            "uninstall",
            f"neon-{namespace}",
            "--kube-context",
            get_context(region),
            "--namespace",
            namespace,
        ]
        LOG.info(
            f'Uninstalling possibly existing Neon in namespace "{namespace}" in region "{region}"'
        )
        LOG.debug(f"Executing: {' '.join(cmd)}")
        if not dry_run:
            subprocess.run(cmd, check=False)

    def deploy_neon_one_namespace(region, namespace):
        cmd = [
            "helm",
            "install",
            f"neon-{namespace}",
            "--kube-context",
            get_context(region),
            "--namespace",
            namespace,
            "--set",
            f"regions={{global,{','.join(regions)}}}",
            (BASE_PATH / "helm-neon").as_posix(),
        ]
        LOG.info(f'Installing Neon in namespace "{namespace}" in region "{region}"')
        LOG.debug(f"Executing: {' '.join(cmd)}")
        if not dry_run:
            subprocess.run(cmd, check=True)

    for region in regions:
        clean_up_neon_one_namespace(region, region)
    clean_up_neon_one_namespace(global_region, "global")

    if cleanup_only:
        return

    deploy_neon_one_namespace(global_region, "global")
    for region in regions:
        deploy_neon_one_namespace(region, region)


STAGES = [
    "load-balancer",
    "dns-config",
    "namespace",
    "neon",
]

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--skip-before",
        "--from",
        "-f",
        choices=STAGES,
        help="Skip all stages before the specified stage.",
    )
    parser.add_argument(
        "--skip-after",
        "--to",
        "-t",
        choices=STAGES,
        help="Skip all stages after the specified stage.",
    )
    parser.add_argument(
        "--clean-up-neon",
        action="store_true",
        help='Only do the cleaning up in the "neon" stage.',
    )
    parser.add_argument(
        "--stages",
        action="store_true",
        help="Print the available stages and exit.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the commands that would be executed without actually executing them.",
    )
    args = parser.parse_args()

    if args.stages:
        print("\n".join(STAGES))
        exit(0)

    skip_before_index = STAGES.index(args.skip_before) if args.skip_before else 0
    skip_after_index = (
        STAGES.index(args.skip_after) if args.skip_after else len(STAGES) - 1
    )

    unskipped_stages = STAGES[skip_before_index : skip_after_index + 1]

    with open(BASE_PATH / REGIONS_YAML, "r") as yaml_file:
        info = yaml.safe_load(yaml_file)

    if "load-balancer" in unskipped_stages:
        LOG.info(
            f"======================== Setting up load balancer for CoreDNS ========================"
        )
        set_up_load_balancer_for_coredns(info["regions"], args.dry_run)

    if "dns-config" in unskipped_stages:
        LOG.info(
            f"======================== Installing DNS configmap ========================"
        )
        install_dns_configmap(info["regions"], info["global_region"], args.dry_run)

    if "namespace" in unskipped_stages:
        LOG.info(
            f"======================== Creating namespaces ========================"
        )
        create_namespaces(info["regions"], info["global_region"], args.dry_run)

    if "neon" in unskipped_stages:
        LOG.info(f"======================== Deploying Neon ========================")
        deploy_neon(
            info["regions"], info["global_region"], args.clean_up_neon, args.dry_run
        )
