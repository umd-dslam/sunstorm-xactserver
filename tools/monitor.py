import argparse
import kubernetes.client
import itertools
import threading
import traceback
import signal

from concurrent.futures import ThreadPoolExecutor
from collections import defaultdict
from pathlib import Path
from typing import TypedDict

from rich.console import Group
from rich.live import Live
from rich.table import Table
from rich.prompt import Confirm
from rich.layout import Layout
from utils import (
    COLORS,
    Command,
    get_main_config,
    get_namespaces,
    get_logger,
    initialize_and_run_commands,
    Kube,
    NamespaceInfo,
)

LOG = get_logger(__name__)
BASE_PATH = Path(__file__).parent.resolve() / "deploy"


def get_chosen_namespaces(args: argparse.Namespace):
    config = get_main_config(BASE_PATH)
    namespaces = get_namespaces(config)

    chosen = {
        ns: info
        for ns, info in namespaces.items()
        if not args.namespaces or ns in args.namespaces
    }

    if args.namespaces:
        for ns in args.namespaces:
            if ns not in chosen:
                LOG.warning(f'Namespace "{ns}" not found in config.')

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

    def add_arguments(self, parser: argparse.ArgumentParser):
        super().add_arguments(parser)
        parser.add_argument(
            "--deployment",
            "-d",
            nargs="*",
            choices=["compute", "xactserver", "pageserver"],
            help="The deployment to watch. If no pod or deployment is specified, "
            "the 'compute' and 'xactserver' deployment will be watched.",
        )
        parser.add_argument(
            "--pod",
            "-p",
            nargs="*",
            help="The pod to watch.",
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
        if args.all:
            LOG.info("Showing all logs from the beginning.")
        else:
            LOG.info(f"Showing {args.lines} lines from the tail.")

        if args.follow:
            LOG.info("Following logs after that.")

        tasks = []
        with ThreadPoolExecutor() as executor:
            for ns, ns_info in get_chosen_namespaces(args).items():
                region = ns_info["region"]
                if args.deployment is None and args.pod is None:
                    args.deployment = ["compute", "xactserver"]
                for d in args.deployment or []:
                    tasks.append(
                        executor.submit(
                            LogsCommand._get_named_logs, args, ns, region, deployment=d
                        )
                    )
                for p in args.pod or []:
                    tasks.append(
                        executor.submit(
                            LogsCommand._get_named_logs, args, ns, region, pod=p
                        )
                    )

        named_logs = [task.result() for task in tasks if task.result()]

        if not Confirm.ask("Start watching logs?", default=True):
            return

        exit_event = threading.Event()
        Kube.print_logs(named_logs, args.follow, exit_event=exit_event)

        if args.follow:
            # Wait for Ctrl-C
            signal.signal(signal.SIGINT, lambda *args: exit_event.set())
            exit_event.wait()

    @staticmethod
    def _get_named_logs(
        args,
        namespace: str,
        region: str,
        deployment: str | None = None,
        pod: str | None = None,
    ):
        kube_config = Kube.get_config(BASE_PATH, region)

        if deployment:
            with kubernetes.client.ApiClient(kube_config) as api_client:  # type: ignore
                appsv1 = kubernetes.client.AppsV1Api(api_client)

                deployment_info = appsv1.read_namespaced_deployment(
                    name=deployment, namespace=namespace
                )
                selector = (
                    deployment_info.spec.selector.match_labels
                    if deployment_info.spec
                    else None
                ) or {}

            pods = Kube.get_pods(kube_config, namespace, selector)
        elif pod:
            pods = [pod]
        else:
            raise Exception("Either deployment or pod must be specified.")

        if not pods:
            LOG.warning(
                f'Cannot find any running pods in deployment "{deployment}" in region "{region}".'
            )
            return None
        elif len(pods) > 1:
            LOG.warning(
                f'Found more than one running pods in deployment "{deployment}" in region "{region}".'
                f' Only watch the first pod "{pods[0]}".'
            )

        try:
            logs = Kube.get_logs(
                kube_config,
                namespace,
                pods[0],
                follow=args.follow,
                lines=None if args.all else args.lines,
            )

            LOG.info(
                f'Showing logs from pod "{pods[0]}" in namespace "{namespace}" (region "{region}").'
            )

            return Kube.NamedLogs(
                namespace=namespace,
                name=deployment if deployment else pods[0],
                stream=logs,
            )
        except kubernetes.client.rest.ApiException as e:  # type: ignore
            LOG.error(
                f'Pod "{pods[0]}" in namespace "{namespace}" (region "{region}"): {e}'
            )
            return None


class Node(TypedDict):
    capacity: dict[str, str]
    status: bool
    pods: list[dict[str, str]]


class StatusCommand(MonitorCommand):
    NAME = "status"
    HELP = "Show the status across multiple regions."

    def add_arguments(self, parser: argparse.ArgumentParser):
        super().add_arguments(parser)
        parser.add_argument(
            "--refresh",
            "-n",
            type=int,
            default=1,
            help="The refresh rate in seconds.",
        )

    def do_command(self, args):
        exit_event = threading.Event()
        signal.signal(signal.SIGINT, lambda *args: exit_event.set())

        try:
            initial_tables = StatusCommand._generate_tables(args)
        except Exception as e:
            initial_tables = str(e)

        layout = Layout()
        layout.split_column(
            Layout(f"Refreshing every {args.refresh} seconds", size=1),
            Layout(initial_tables, name="table"),
        )

        with Live(layout, screen=True):
            while True:
                try:
                    layout["table"].update(StatusCommand._generate_tables(args))
                except InterruptedError:
                    break
                except Exception:
                    layout["table"].update(traceback.format_exc())

                if exit_event.wait(timeout=args.refresh):
                    break

    @staticmethod
    def _generate_tables(args: argparse.Namespace):
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

        for region, namespaces in sorted(data.items(), key=lambda x: x[0]):
            table = Table(
                "Namespace",
                "Node",
                "Capacity",
                "Pod",
                "Status",
                title=f"[bold]{region}[/bold]",
            )

            for namespace, nodes in sorted(namespaces.items(), key=lambda x: x[0]):
                table.add_section()
                namespace_cell = namespace
                for node_name, node in nodes.items():
                    capacity_cell = ""
                    if node["capacity"]:
                        cpu = node["capacity"]["cpu"]
                        memory = node["capacity"]["memory"]
                        capacity_cell = f"cpu: {cpu}, mem: {memory}"

                    # Choose a color for the node
                    if node_name not in node_to_color:
                        node_to_color[node_name] = next(colors)
                    color = node_to_color[node_name]

                    node_cell = node_name
                    if node_name and not node.get("status", False):
                        node_cell = f"[s]{node_cell}[/s] (not ready)"

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
    def _get_data(args: argparse.Namespace):
        namespaces = get_chosen_namespaces(args)

        data: defaultdict[str, dict[str, defaultdict[str, Node]]] = defaultdict(dict)
        lock = threading.Lock()

        def get_namespaced_data(ns: str, ns_info: NamespaceInfo):
            region = ns_info["region"]
            kube_config = Kube.get_config(BASE_PATH, region)

            nodes: defaultdict[str, Node] = defaultdict(
                lambda: {"pods": [], "capacity": {}, "status": False}
            )

            with kubernetes.client.ApiClient(kube_config) as api_client:  # type: ignore
                corev1 = kubernetes.client.CoreV1Api(api_client)
                pods = corev1.list_namespaced_pod(
                    namespace=ns,
                ).items

                for pod in pods:
                    node = pod.spec.node_name if pod.spec else None
                    # Populate node capacity if not already populated
                    if node and not nodes[node]["capacity"]:
                        try:
                            node_obj = corev1.read_node(node)
                            nodes[node]["capacity"] = (
                                (node_obj.status.capacity or {})
                                if node_obj.status
                                else {}
                            )
                            conditions = (
                                node_obj.status.conditions
                                if node_obj and node_obj.status
                                else []
                            )
                            for cond in conditions or []:
                                if cond.type == "Ready":
                                    nodes[node]["status"] = cond.status == "True"
                                    break
                        except kubernetes.client.rest.ApiException:  # type: ignore
                            pass

                    # Add pod to node
                    nodes[node or "unknown"]["pods"].append(
                        {
                            "name": (pod.metadata.name if pod.metadata else "") or "",
                            "status": StatusCommand._compute_status(pod),
                        }
                    )

            with lock:
                data[region].update({ns: nodes})

        with ThreadPoolExecutor() as executor:
            list(
                executor.map(
                    get_namespaced_data, namespaces.keys(), namespaces.values()
                )
            )

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
