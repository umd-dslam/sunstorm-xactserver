import argparse
import json
import tempfile
import time
import subprocess

import boto3
import dns.resolver
import kubernetes.client
import yaml

from pathlib import Path
from kubernetes.client.rest import ApiException
from typing import List
from rich.console import Console
from utils import get_logger, get_main_config, get_context, run_command, get_kube_config

LOG = get_logger(__name__)
BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def context_flag(region, flag_name="--context"):
    try:
        context = get_context(BASE_PATH, region)
        return [flag_name, context]
    except:
        return []


def try_with_timeout(fn, timeout: int):
    start_time = time.time()
    console = Console()
    with console.status("[bold green]Waiting..."):
        while True:
            result = fn()
            if result is not None:
                return result

            if time.time() - start_time >= timeout:
                raise TimeoutError(
                    f"Timeout: {fn.__name__} did not return within {timeout} seconds."
                )


def set_up_load_balancer_for_coredns(config, dry_run: bool) -> str:
    regions = config["regions"]
    if len(regions) == 1:
        LOG.info(
            "Only one region is specified. Skipping load balancer for CoreDNS.",
        )
        return

    for region in regions:
        run_command(
            [
                "kubectl",
                "apply",
                "-f",
                (BASE_PATH / "eks" / "dns-lb-eks.yaml").as_posix(),
            ]
            + context_flag(region),
            (LOG, f"Creating load balancer for CoreDNS in region {region}."),
            dry_run,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.STDOUT,
        )
        LOG.info(f"Load balancer for CoreDNS in region {region} created.")


def install_dns_configmap(config, dry_run: bool):
    global_region = config["global_region"]
    regions = set(config["regions"]) | {config["global_region"]}
    if len(regions) == 1:
        LOG.info(
            "Only one region is specified. Skipping DNS configmap installation.",
        )
        return

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
        helm_name = f"dns-{region}"
        run_command(
            [
                "helm",
                "uninstall",
                helm_name,
            ]
            + context_flag(region, "--kube-context"),
            (
                LOG,
                f"Uninstalling possibly existing CoreDNS configmap in region: {region}",
            ),
            dry_run,
            check=False,
        )

        run_command(
            [
                "kubectl",
                "delete",
                "configmap",
                "coredns",
                "--namespace",
                "kube-system",
            ]
            + context_flag(region),
            (LOG, f"Deleting possibly existing CoreDNS configmap in region: {region}"),
            dry_run,
            check=False,
        )

        run_command(
            [
                "helm",
                "install",
                helm_name,
                "-f",
                dns_config.name,
                "--set",
                f"region={region},global_region={global_region}",
                (BASE_PATH / "helm-dns").as_posix(),
            ]
            + context_flag(region, "--kube-context"),
            (LOG, f"Installing new CoreDNS configmap in region: {region}"),
            dry_run,
            check=True,
        )


def create_namespaces(config, dry_run: bool):
    global_region = config["global_region"]
    regions = set(config["regions"]) | {global_region}

    def clean_up_namespace(region, namespace):
        config = get_kube_config(BASE_PATH, region)
        LOG.info(f'Deleting namespace "{namespace}" in region "{region}"')
        with kubernetes.client.ApiClient(config) as api_client:
            kube = kubernetes.client.CoreV1Api(api_client)
            try:
                kube.delete_namespace(
                    namespace, pretty="true", dry_run="All" if dry_run else None
                )
            except ApiException as e:
                if e.status != 404:
                    LOG.error(
                        "Exception when calling CoreV1Api->delete_namespace: %s" % e
                    )

    clean_up_neon_one_namespace(global_region, "global", dry_run)
    clean_up_namespace(global_region, "global")
    for region in regions:
        clean_up_neon_one_namespace(region, region, dry_run)
        clean_up_namespace(region, region)

    def create_namespace(region, namespace):
        config = get_kube_config(BASE_PATH, region)
        LOG.info(f'Creating namespace "{namespace}" in region "{region}"')
        with kubernetes.client.ApiClient(config) as api_client:
            kube = kubernetes.client.CoreV1Api(api_client)
            while True:
                try:
                    kube.create_namespace(
                        kubernetes.client.V1Namespace(
                            metadata=kubernetes.client.V1ObjectMeta(
                                name=namespace,
                                labels={
                                    "part-of": "neon",
                                },
                            )
                        ),
                        pretty="true",
                        dry_run="All" if dry_run else None,
                    )
                    break
                except ApiException as e:
                    body = json.loads(e.body)
                    if "object is being deleted" in body["message"]:
                        LOG.warning(
                            f'Namespace "{namespace}" in region "{region}" is being deleted. Retrying after 5 second.'
                        )
                        time.sleep(5)
                    else:
                        LOG.error(
                            "Exception when calling CoreV1Api->create_namespace: %s" % e
                        )
                        break

    create_namespace(global_region, "global")
    for region in regions:
        create_namespace(region, region)


def deploy_neon(config, cleanup_only: bool, dry_run: bool):
    global_region = config["global_region"]
    regions = set(config["regions"]) | {global_region}

    def deploy_neon_one_namespace(region, namespace):
        substitutions = f"regions={{global,{','.join(regions)}}}"
        hub_ebs_volume_id = config.get("hub_ebs_volume_id")
        if region == global_region and hub_ebs_volume_id:
            substitutions += f",hub_ebs_volume_id={hub_ebs_volume_id}"

        run_command(
            [
                "helm",
                "install",
                f"neon-{namespace}",
                "--namespace",
                namespace,
                "--set",
                substitutions,
                (BASE_PATH / "helm-neon").as_posix(),
            ]
            + context_flag(region, "--kube-context"),
            (LOG, f'Installing Neon in namespace "{namespace}" in region "{region}"'),
            dry_run,
            check=True,
        )

    for region in regions:
        clean_up_neon_one_namespace(region, region, dry_run)
    clean_up_neon_one_namespace(global_region, "global", dry_run)

    if cleanup_only:
        return

    deploy_neon_one_namespace(global_region, "global")
    for region in regions:
        deploy_neon_one_namespace(region, region)


def clean_up_neon_one_namespace(region, namespace, dry_run):
    run_command(
        [
            "helm",
            "uninstall",
            f"neon-{namespace}",
            "--namespace",
            namespace,
        ]
        + context_flag(region, "--kube-context"),
        (
            LOG,
            f'Uninstalling possibly existing Neon in namespace "{namespace}" in region "{region}"',
        ),
        dry_run,
        check=False,
    )


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

    config = get_main_config(BASE_PATH)

    log_tag = "bold yellow"

    if "load-balancer" in unskipped_stages:
        LOG.info(
            f"[{log_tag}]Setting up load balancer for CoreDNS[/{log_tag}]",
            extra={"markup": True},
        )
        set_up_load_balancer_for_coredns(config, args.dry_run)

    if "dns-config" in unskipped_stages:
        LOG.info(
            f"[{log_tag}]Installing DNS configmap[/{log_tag}]", extra={"markup": True}
        )
        install_dns_configmap(config, args.dry_run)

    if "namespace" in unskipped_stages:
        LOG.info(f"[{log_tag}]Creating namespaces[/{log_tag}]", extra={"markup": True})
        create_namespaces(config, args.dry_run)

    if "neon" in unskipped_stages:
        LOG.info(f"[{log_tag}]Deploying Neon[/{log_tag}]", extra={"markup": True})
        deploy_neon(
            config,
            args.clean_up_neon,
            args.dry_run,
        )
