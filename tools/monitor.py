import argparse
import kubernetes.client
import itertools
import threading
import signal

from collections import defaultdict
from pathlib import Path
from rich.console import Console, Group
from rich.live import Live
from rich.table import Table
from rich.prompt import Confirm
from rich.layout import Layout
from utils import (
    get_main_config,
    get_logger,
    get_kube_config,
    Command,
    initialize_and_run_commands,
    COLORS,
)

LOG = get_logger(__name__)
BASE_PATH = Path(__file__).parent.resolve() / "deploy"


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
    config = get_main_config(BASE_PATH)
    regions = config["regions"]
    global_region = config["global_region"]

    chosen = []
    if args.namespaces:
        for ns in args.namespaces:
            if ns == "global":
                chosen.append((global_region, ns))
            elif ns in regions:
                chosen.append((ns, ns))
            else:
                raise ValueError(f"Namespace {ns} is not defined in the config file.")
    else:
        chosen = [(r, r) for r in regions]
        chosen.append((global_region, "global"))

    return chosen


class MonitorCommand(Command):
    def add_arguments(self, parser):
        parser.add_argument(
            "--namespaces",
            "-ns",
            nargs="*",
            help="The namespaces to monitor. If not specified, all namespaces will be monitored.",
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
                decoded = line.decode("utf-8").rstrip("\n")
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
        parser.add_argument(
            "--refresh",
            "-n",
            type=int,
            default=1,
            help="The refresh rate in seconds.",
        )

    def do_command(self, args):
        layout = Layout()
        layout.split_column(
            Layout(f"Refreshing every {args.refresh} seconds", size=1),
            Layout(StatusCommand._generate_tables(args), name="table"),
        )

        exit_event = threading.Event()
        signal.signal(signal.SIGINT, lambda *args: exit_event.set())

        with Live(layout, screen=True):
            while True:
                layout["table"].update(StatusCommand._generate_tables(args))
                if exit_event.wait(timeout=args.refresh):
                    break

    @staticmethod
    def _generate_tables(args):
        data = StatusCommand._get_data(args)

        colors = itertools.cycle(COLORS)
        node_to_color = {}
        tables = []

        def color_status(status):
            if status in ["Running", "Succeeded"]:
                return f"[b green]{status}[/b green]"
            elif status == "Pending":
                return "[b yellow]Pending[/b yellow]"
            elif status == "Terminating":
                return "[b]Terminating[/b]"
            else:
                return f"[b red]{status}[/b red]"

        for region, namespaces in data.items():
            table = Table(
                "Namespace",
                "Node",
                "Capacity",
                "Pod",
                "Status",
                title=f"[bold]{region}[/bold]",
            )

            for namespace, nodes in namespaces.items():
                table.add_section()
                namespace_cell = namespace
                for node_cell, node in nodes.items():
                    capacity_cell = ""
                    if "capacity" in node:
                        cpu = node["capacity"]["cpu"]
                        memory = node["capacity"]["memory"]
                        capacity_cell = f"cpu: {cpu}, mem: {memory}"

                    # Choose a color for the node
                    if node_cell not in node_to_color:
                        node_to_color[node_cell] = next(colors)
                    color = node_to_color[node_cell]

                    for pod in node["pods"]:
                        table.add_row(
                            namespace_cell,
                            f"[{color}]{node_cell}[/{color}]",
                            f"[{color}]{capacity_cell}[/{color}]",
                            f"[{color}]{pod['name']}[/{color}]",
                            color_status(pod["status"]),
                        )
                        node_cell = ""
                        capacity_cell = ""
                        namespace_cell = ""

            tables.append(table)

        return Group(*tables)

    @staticmethod
    def _get_data(args):
        data = defaultdict(dict)
        for region, namespace in get_region_namespaces(args):
            config = get_kube_config(BASE_PATH, region)

            nodes = defaultdict(lambda: {"pods": []})

            with kubernetes.client.ApiClient(config) as api_client:
                corev1 = kubernetes.client.CoreV1Api(api_client)
                pods = corev1.list_namespaced_pod(
                    namespace=namespace,
                ).items

                for pod in pods:
                    node = pod.spec.node_name
                    # Populate node capacity if not already done
                    if node and "capacity" not in nodes[node]:
                        node_obj = corev1.read_node(node)
                        nodes[node]["capacity"] = node_obj.status.capacity

                    # Add pod to node
                    nodes[node]["pods"].append(
                        {
                            "name": pod.metadata.name,
                            "status": StatusCommand._compute_status(pod),
                        }
                    )

            data[region].update({namespace: nodes})

        return data

    @staticmethod
    def _compute_status(pod):
        if pod.status.phase != "Running":
            return pod.status.phase

        is_disrupted = False
        containers_ready = False
        for cond in pod.status.conditions or []:
            if cond.type == "DisruptionTarget" and cond.status == "True":
                is_disrupted = True
                break
            if cond.type == "ContainersReady" and cond.status == "True":
                containers_ready = True

        if is_disrupted:
            return "Terminating"

        if not containers_ready:
            for container in pod.status.container_statuses or []:
                if not container.ready:
                    if container.state.waiting:
                        return container.state.waiting.reason
                    elif container.state.terminated:
                        return container.state.terminated.reason

        return "Running"


if __name__ == "__main__":
    parser = argparse.ArgumentParser()

    initialize_and_run_commands(
        parser,
        [LogsCommand, StatusCommand],
    )
