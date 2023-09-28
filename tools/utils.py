from typing import Iterator
import yaml

from collections import namedtuple

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


def get_logger(name: str):
    import logging
    import os
    from rich.logging import RichHandler

    logger = logging.getLogger(name)
    logger.addHandler(RichHandler())
    logger.setLevel(os.environ.get("LOG_LEVEL", "INFO").upper())
    return logger


class Command:
    """Base class for a command"""

    NAME = "<not_implemented>"
    HELP = ""
    DESCRIPTION = ""

    def create_subparser(self, subparsers):
        parser = subparsers.add_parser(
            self.NAME, description=self.DESCRIPTION, help=self.HELP
        )
        parser.set_defaults(run=self.do_command)
        self.add_arguments(parser)
        return parser

    def add_arguments(self, parser):
        pass

    def do_command(self, args):
        pass


def initialize_and_run_commands(parser, commands, args=None):
    subparsers = parser.add_subparsers(dest="command name")
    subparsers.required = True

    for command in commands:
        command().create_subparser(subparsers)

    parsed_args = parser.parse_args(args)
    parsed_args.run(parsed_args)


def run_subprocess(cmd, logger_and_info_log, dry_run, **kwargs):
    import subprocess

    logger, info_log = logger_and_info_log
    logger.info(info_log)
    logger.debug(f"Executing: {' '.join(cmd)}")
    if not dry_run:
        subprocess.run(cmd, **kwargs)


def get_main_config(base_path):
    with open(base_path / "main.yaml", "r") as yaml_file:
        return yaml.safe_load(yaml_file)


def get_namespaces(config):
    namespaces = {"global": {"region": config["global_region"], "id": 0}}
    for region, region_info in config["regions"].items():
        namespaces[region] = {"region": region, "id": region_info["id"]}
    return namespaces


import kubernetes.client
import kubernetes.config


class Kube:
    @staticmethod
    def get_context(base_path, region: str) -> str:
        contexts, _ = kubernetes.config.list_kube_config_contexts()
        context_names = [c["name"] for c in contexts]

        context = None

        # Look to see if the context is specified with the region
        with open(base_path / "main.yaml", "r") as yaml_file:
            regions_info = yaml.safe_load(yaml_file)
            if region in regions_info["regions"]:
                regions = regions_info["regions"]
                if (
                    region in regions
                    and regions[region]
                    and "context" in regions[region]
                ):
                    context = regions[region]["context"]

            # Context is still not found see if this is a global region
            # and global region context is specified
            if context is None:
                if (
                    region == regions_info["global_region"]
                    and "global_region_context" in regions_info
                ):
                    region = regions_info["global_region_context"]

        # Context is still not found, choose the one with the region
        # name in it
        if context is None:
            for ctx in context_names:
                if region in ctx:
                    context = ctx
                    break

        # Cannot find the context, give up
        if context is None:
            raise Exception(f"Could not find context for region: {region}")

        return context

    @staticmethod
    def get_config(base_path, region: str):
        context = Kube.get_context(base_path, region)
        config = kubernetes.client.Configuration()
        kubernetes.config.load_kube_config(
            context=context, client_configuration=config, persist_config=False
        )

        return config

    @staticmethod
    def get_pods(kube_config, namespace, selector, phases=None):
        with kubernetes.client.ApiClient(kube_config) as api_client:
            corev1 = kubernetes.client.CoreV1Api(api_client)
            pods = []
            pod_list = corev1.list_namespaced_pod(
                namespace=namespace,
                label_selector=",".join([f"{k}={v}" for k, v in selector.items()]),
            )
            for pod in pod_list.items:
                if not phases or pod.status.phase in phases:
                    pods.append(pod.metadata.name)

            return pods

    @staticmethod
    def get_logs(kube_config, namespace, pod, follow, lines=None, container=None):
        with kubernetes.client.ApiClient(kube_config) as api_client:
            corev1 = kubernetes.client.CoreV1Api(api_client)
            return corev1.read_namespaced_pod_log(
                name=pod,
                namespace=namespace,
                container=container,
                follow=follow,
                tail_lines=lines,
                _preload_content=False,
            )

    NamedLogs = namedtuple("NamedLogs", ["namespace", "name", "stream"])

    @staticmethod
    def print_logs(named_logs: NamedLogs, follow, console=None, exit_event=None):
        import itertools
        import threading

        from rich.console import Console
        from rich.markup import escape

        if isinstance(named_logs, Iterator):
            named_logs = list(named_logs)
        elif isinstance(named_logs, Kube.NamedLogs):
            named_logs = [named_logs]

        if not console:
            console = Console()

        alive_threads = len(named_logs)
        lock = threading.Lock()

        def print_log(logs, color):
            name = f"{logs.namespace}|{logs.name}"
            for line in logs.stream:
                decoded = line.decode("utf-8").rstrip("\n")
                console.print(
                    f"[bold]\[{name}][/bold] {escape(decoded)}",
                    style=color,
                    highlight=False,
                )

            nonlocal alive_threads
            with lock:
                alive_threads -= 1
                if alive_threads == 0 and exit_event:
                    exit_event.set()

        colors = itertools.cycle(COLORS)
        threads = []
        for logs in named_logs:
            color = next(colors)
            t = threading.Thread(target=print_log, args=(logs, color), daemon=True)
            t.start()
            threads.append(t)

        if not follow:
            # Wait for all threads to finish
            for t in threads:
                t.join()
