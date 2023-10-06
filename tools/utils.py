import itertools
import logging
import threading

import kubernetes.client
import kubernetes.config
import yaml

from collections import namedtuple
from pathlib import Path
from typing import Iterator, TypedDict

from rich.console import Console
from rich.markup import escape


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


def run_subprocess(
    cmd: list[str],
    logger_and_info_log: tuple[logging.Logger, str],
    dry_run: bool,
    **kwargs,
):
    import subprocess

    logger, info_log = logger_and_info_log
    logger.info(info_log)
    logger.debug(f"Executing: {' '.join(cmd)}")
    if not dry_run:
        subprocess.run(cmd, **kwargs)


class MainConfig(TypedDict):
    global_region: str
    global_region_context: str
    regions: dict[str, dict[str, str | int]]


def get_main_config(base_path: Path) -> MainConfig:
    with open(base_path / "main.yaml", "r") as yaml_file:
        main_config: MainConfig = yaml.safe_load(yaml_file)

    contexts, _ = kubernetes.config.list_kube_config_contexts()
    context_names: list[str] = [c["name"] for c in contexts]

    def find_context(region: str):
        for ctx in context_names:
            if region in ctx:
                return ctx
        return None

    if main_config.get("regions") is None:
        main_config["regions"] = {}

    for region, region_info in main_config["regions"].items():
        if region_info.get("context") is None:
            region_info["context"] = find_context(region)

    if main_config.get("global_region_context") is None:
        main_config["global_region_context"] = find_context(
            main_config["global_region"]
        )
    return main_config


def get_namespaces(config: MainConfig):
    namespaces: dict[str, dict[str, str | int]] = {
        "global": {"region": config["global_region"], "id": 0}
    }
    regions = config.get("regions") or {}
    for region, region_info in regions.items():
        namespaces[region] = {"region": region, "id": region_info["id"]}
    return namespaces


class Kube:
    @staticmethod
    def get_context(base_path: Path, region: str) -> str:
        context = None

        main_config = get_main_config(base_path)
        if region == main_config["global_region"]:
            context = main_config.get("global_region_context", None)

        if context is None:
            context = str(
                main_config.get("regions", {}).get(region, {}).get("context", None)
            )

        if context is None:
            raise Exception(f"Could not find context for region: {region}")

        return context

    @staticmethod
    def get_config(base_path: Path, region: str):
        context = Kube.get_context(base_path, region)
        config = kubernetes.client.Configuration()
        kubernetes.config.load_kube_config(
            context=context, client_configuration=config, persist_config=False
        )

        return config

    @staticmethod
    def get_pods(
        kube_config: kubernetes.client.Configuration,
        namespace: str,
        selector: dict[str, str],
        phases: list[str] | None = None,
    ):
        with kubernetes.client.ApiClient(kube_config) as api_client:
            corev1 = kubernetes.client.CoreV1Api(api_client)
            pods: list[str] = []
            pod_list = corev1.list_namespaced_pod(
                namespace=namespace,
                label_selector=",".join([f"{k}={v}" for k, v in selector.items()]),
            )
            for pod in pod_list.items:
                if not phases or pod.status.phase in phases:
                    pods.append(pod.metadata.name)

            return pods

    @staticmethod
    def get_logs(
        kube_config: kubernetes.client.Configuration,
        namespace: str,
        pod: str,
        follow: bool,
        lines: int | None = None,
        container: str | None = None,
    ):
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
    def print_logs(
        named_logs: NamedLogs | list[NamedLogs],
        follow: bool,
        console: Console | None = None,
        exit_event: threading.Event | None = None,
    ):
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
        threads: list[threading.Thread] = []
        for logs in named_logs:
            color = next(colors)
            t = threading.Thread(target=print_log, args=(logs, color), daemon=True)
            t.start()
            threads.append(t)

        if not follow:
            # Wait for all threads to finish
            for t in threads:
                t.join()
