import argparse
import kubernetes.client
from kubernetes.client.rest import ApiException
import os

from multiprocessing.pool import ThreadPool
from pathlib import Path
from rich.console import Console
from utils import (
    get_regions,
    get_logger,
    get_kube_config,
    Command,
    initialize_and_run_commands,
)

LOG = get_logger(
    __name__,
    level=os.environ.get("LOG_LEVEL", "INFO").upper(),
)

BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def get_running_pods_in_deployment(region, namespace, deployment_name):
    config = get_kube_config(BASE_PATH, region)
    with kubernetes.client.ApiClient(config) as api_client:
        appv1 = kubernetes.client.AppsV1Api(api_client)
        corev1 = kubernetes.client.CoreV1Api(api_client)

        pods = []
        deployment = appv1.read_namespaced_deployment(
            name=deployment_name, namespace=namespace
        )

        selector = deployment.spec.selector.match_labels

        pod_list = corev1.list_namespaced_pod(
            namespace=namespace,
            label_selector=",".join([f"{k}={v}" for k, v in selector.items()]),
        )

        for pod in pod_list.items:
            if pod.status.phase == "Running":
                pods.append(pod.metadata.name)

        return pods


def read_pod_log(region, namespace, pod_name):
    console = Console()

    pool = ThreadPool(processes=2)

    config = get_kube_config(BASE_PATH, region)
    with kubernetes.client.ApiClient(config) as api_client:
        corev1 = kubernetes.client.CoreV1Api(api_client)

        pageserver = corev1.read_namespaced_pod_log(
            name=pod_name,
            namespace=namespace,
            container="pageserver",
            follow=True,
            _preload_content=False,
        )
        # xactserver = corev1.read_namespaced_pod_log(
        #     name="xactserver",
        #     namespace=namespace,
        #     container="xactserver",
        #     follow=True,
        #     _preload_content=False,
        # )

        # def log_stream():
        #     yield from pageserver.stream()
        #     yield from xactserver.stream()

        # pool.map()

        print(pageserver.stream())

        # for line in pageserver.stream():
        #     console.print(line.decode("utf-8").strip())


class LogsCommand:
    NAME = "logs"
    HELP = "Watch the logs from multiple regions."

    def add_arguments(self, parser):
        super().add_arguments(parser)
        pass

    def do_command(self, args):
        pass


class StatusCommand:
    NAME = "status"
    HELP = "Show the status across multiple regions."

    def add_arguments(self, parser):
        super().add_arguments(parser)
        pass

    def do_command(self, args):
        pass


if __name__ == "__main__":
    # parser = argparse.ArgumentParser()

    # parser.add_argument("operation", choices=["create", "load", "execute"])
    # parser.add_argument(
    #     "--set",
    #     "-s",
    #     action="append",
    #     help="Override the values in the config file. Each argument should be in the form of key=value.",
    # )

    # args = parser.parse_args()

    # regions_info = get_regions(BASE_PATH)
    # regions = regions_info["regions"]
    # global_region = regions_info["global_region"]

    # initialize_and_run_commands(
    #     parser,
    #     [LogsCommand, StatusCommand],
    # )
    # config.load_kube_config()

    # v1 = client.CoreV1Api()
    # print("Listing pods with their IPs:")
    # ret = v1.list_pod_for_all_namespaces(watch=False)
    # for i in ret.items:
    #     print("%s\t%s\t%s" % (i.status.pod_ip, i.metadata.namespace, i.metadata.name))

    pods = get_running_pods_in_deployment("us-east-1", "us-east-1", "pageserver")
    print(pods)
    read_pod_log("us-east-1", "us-east-1", pods[0])
    # for line in log:
    #     print(line)

    # namespaces = api_instance.list_namespace()
    # for ns in namespaces.items:
    #     print(ns.metadata.name)

    # import pprint

    # pprint.pprint(contexts)
    # if not contexts:
    #     print("Cannot find any context in kube-config file.")
