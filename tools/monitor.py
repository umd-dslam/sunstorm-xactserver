import argparse
import kubernetes.client
import itertools
import os
import threading
import signal

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from rich.console import Console
from rich.prompt import Confirm
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

# List of 20 distinct colors
# https://sashat.me/2017/01/11/list-of-20-simple-distinct-colors/
COLORS = [
    "#e6194b",
    "#3cb44b",
    "#ffe119",
    "#4363d8",
    "#f58231",
    "#911eb4",
    "#46f0f0",
    "#f032e6",
    "#bcf60c",
    "#fabebe",
    "#008080",
    "#e6beff",
    "#9a6324",
    "#fffac8",
    "#800000",
    "#aaffc3",
    "#808000",
    "#ffd8b1",
    "#000075",
    "#808080",
    "#ffffff",
    "#000000",
]


def get_running_pods_in_deployment(region, namespace, deployment_name):
    config = get_kube_config(BASE_PATH, region)
    with kubernetes.client.ApiClient(config) as api_client:
        appsv1 = kubernetes.client.AppsV1Api(api_client)
        corev1 = kubernetes.client.CoreV1Api(api_client)

        pods = []
        deployment = appsv1.read_namespaced_deployment(
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


def get_region_namespaces(args) -> list[tuple[str, str]]:
    regions, global_region = get_regions(BASE_PATH)

    chosen_regions = []
    if args.regions:
        for r in args.regions:
            if r == "global":
                chosen_regions.append((global_region, r))
            elif r in regions:
                chosen_regions.append((r, r))
            else:
                raise ValueError(f"Region {r} is not defined in the config file.")
    else:
        chosen_regions = [(r, r) for r in regions]
        chosen_regions.append((global_region, "global"))

    return chosen_regions


class MonitorCommand(Command):
    def add_arguments(self, parser):
        parser.add_argument(
            "--regions",
            "-r",
            nargs="*",
            help="The regions to monitor. If not specified, all regions will be monitored.",
        )


class LogsCommand(MonitorCommand):
    NAME = "logs"
    HELP = "Watch the logs from multiple regions."

    def add_arguments(self, parser):
        super().add_arguments(parser)
        parser.add_argument(
            "--deployment",
            "-d",
            nargs="*",
            choices=["compute", "xactserver", "pageserver"],
            help="The deployment to watch. If not specified, "
            "only 'compute' and 'xactserver' will be watched.",
        )
        parser.add_argument(
            "--follow",
            "-f",
            action="store_true",
            help="Follow the logs.",
        )
        lines = parser.add_mutually_exclusive_group()
        lines.add_argument(
            "--all",
            "-a",
            action="store_true",
            help="Show all logs from the beginning.",
        )
        lines.add_argument(
            "--lines",
            "-l",
            type=int,
            default=50,
            help="The number of lines from the tail to show.",
        )

    def do_command(self, args):
        deployments = args.deployment or ["compute", "xactserver"]

        if args.all:
            LOG.info("Showing all logs from the beginning.")
        else:
            LOG.info(f"Showing {args.lines} lines from the tail.")

        if args.follow:
            LOG.info("Following logs after that.")

        logs_streams = []
        for region, namespace in get_region_namespaces(args):
            config = get_kube_config(BASE_PATH, region)

            for deploy in deployments:
                pod = get_running_pods_in_deployment(region, namespace, deploy)
                if not pod:
                    LOG.warning(
                        f'Cannot find any running pods in deployment "{deploy}" in region "{region}".'
                    )
                    continue
                elif len(pod) > 1:
                    LOG.warning(
                        f'Found more than one running pods in deployment "{deploy}" in region "{region}".'
                        f' Only watch the first pod "{pod[0]}".'
                    )
                LOG.info(
                    f'Showing logs from pod "{pod[0]}" in "{namespace}" ("{region}").'
                )

                with kubernetes.client.ApiClient(config) as api_client:
                    corev1 = kubernetes.client.CoreV1Api(api_client)
                    logs = corev1.read_namespaced_pod_log(
                        name=pod[0],
                        namespace=namespace,
                        container=deploy,
                        follow=args.follow,
                        tail_lines=None if args.all else args.lines,
                        _preload_content=False,
                    )

                    logs_streams.append(
                        {
                            "region": region,
                            "namespace": namespace,
                            "deployment": deploy,
                            "logs": logs,
                        }
                    )

        if not Confirm.ask("Start watching logs?", default=True):
            return

        console = Console()

        def print_log(log_stream, color):
            name = f"{log_stream['namespace']}|{log_stream['deployment']}"
            for line in log_stream["logs"]:
                decoded = line.decode("utf-8").strip()
                console.print(
                    f"[bold]\[{name}][/bold] {decoded}", style=color, highlight=False
                )

        colors = itertools.cycle(COLORS)
        threads = []
        for log_stream in logs_streams:
            color = next(colors)
            t = threading.Thread(
                target=print_log, args=(log_stream, color), daemon=True
            )
            t.start()
            threads.append(t)

        if args.follow:
            # Wait for Ctrl-C
            exit_event = threading.Event()
            signal.signal(signal.SIGINT, lambda *args: exit_event.set())
            exit_event.wait()
        else:
            # Wait for all threads to finish
            for t in threads:
                t.join()


class StatusCommand(MonitorCommand):
    NAME = "status"
    HELP = "Show the status across multiple regions."

    def add_arguments(self, parser):
        super().add_arguments(parser)
        pass

    def do_command(self, args):
        pass


if __name__ == "__main__":
    parser = argparse.ArgumentParser()

    initialize_and_run_commands(
        parser,
        [LogsCommand, StatusCommand],
    )
