import argparse
import itertools
import logging
import threading

import kubernetes.client
import yaml

from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, TypedDict, Sequence

from kubernetes.client.configuration import Configuration as KubeConfiguration
from kubernetes.config.kube_config import load_kube_config
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

    def create_subparser(
        self, subparsers: "argparse._SubParsersAction[argparse.ArgumentParser]"
    ):
        parser = subparsers.add_parser(
            self.NAME, description=self.DESCRIPTION, help=self.HELP
        )
        parser.set_defaults(run=self.do_command)
        self.add_arguments(parser)
        return parser

    def add_arguments(self, parser: argparse.ArgumentParser):
        pass

    def do_command(self, args: argparse.Namespace):
        pass


def initialize_and_run_commands(
    parser: argparse.ArgumentParser,
    commands: list[type[Command]],
    args: Sequence[str] | None = None,
):
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


class RegionInfo(TypedDict):
    id: int
    context: str | None


class MainConfig(TypedDict):
    global_region: str
    global_region_context: str | None
    regions: dict[str, RegionInfo] | None


def get_main_config(base_path: Path) -> MainConfig:
    with open(base_path / "main.yaml", "r") as yaml_file:
        main_config: MainConfig = yaml.safe_load(yaml_file)

    contexts, _ = kubernetes.config.list_kube_config_contexts()  # type: ignore
    context_names: list[str] = [c["name"] for c in contexts]

    def find_context(region: str):
        for ctx in context_names:
            if region in ctx:
                return ctx
        return None

    regions: dict[str, RegionInfo] = main_config.get("regions") or {}

    for region, region_info in regions.items():
        if region_info.get("context") is None:
            region_info["context"] = find_context(region)

    if (main_config.get("global_region")) and (main_config.get("global_region_context") is None):
        main_config["global_region_context"] = find_context(
            main_config["global_region"]
        )
    return main_config


class NamespaceInfo(TypedDict):
    region: str
    id: int


def get_namespaces(config: MainConfig):
    namespaces: dict[str, NamespaceInfo] = {}
    if "global_region" in config:
        namespaces["global"] =  {"region": config["global_region"], "id": 0}
    regions = config.get("regions") or {}
    for region, region_info in regions.items():
        namespaces[region] = {"region": region, "id": region_info["id"]}
    return namespaces


class Kube:
    @staticmethod
    def get_context(base_path: Path, region: str) -> str:
        context = None

        main_config = get_main_config(base_path)
        if ("global_region" in main_config) and (region == main_config["global_region"]):
            context = main_config.get("global_region_context", None)

        if context is None:
            regions = main_config.get("regions") or {}
            context = regions[region]["context"]

        if context is None:
            raise Exception(f"Could not find context for region: {region}")

        return context

    @staticmethod
    def get_config(base_path: Path, region: str):
        context = Kube.get_context(base_path, region)
        config = KubeConfiguration()
        load_kube_config(
            context=context, client_configuration=config, persist_config=False
        )

        return config

    @staticmethod
    def get_pods(
        kube_config: KubeConfiguration,
        namespace: str,
        selector: dict[str, str],
        phases: list[str] | None = None,
    ):
        with kubernetes.client.ApiClient(kube_config) as api_client:  # type: ignore
            corev1 = kubernetes.client.CoreV1Api(api_client)
            pods: list[str] = []
            pod_list = corev1.list_namespaced_pod(
                namespace=namespace,
                label_selector=",".join([f"{k}={v}" for k, v in selector.items()]),
            )
            for pod in pod_list.items:
                if not phases or (pod.status and pod.status.phase in phases):
                    if pod.metadata and pod.metadata.name:
                        pods.append(pod.metadata.name)

            return pods

    @staticmethod
    def get_logs(
        kube_config: KubeConfiguration,
        namespace: str,
        pod: str,
        follow: bool,
        lines: int | None = None,
        container: str | None = None,
    ):
        with kubernetes.client.ApiClient(kube_config) as api_client:  # type: ignore
            corev1 = kubernetes.client.CoreV1Api(api_client)
            return corev1.read_namespaced_pod_log(  # type: ignore
                name=pod,
                namespace=namespace,
                container=container,
                follow=follow,
                tail_lines=lines,
                _preload_content=False,  # type: ignore
            )

    @dataclass
    class NamedLogs:
        namespace: str
        name: str
        stream: Iterable[bytes]

    @staticmethod
    def print_logs(
        named_logs: NamedLogs | Iterable[NamedLogs],
        follow: bool,
        console: Console | None = None,
        callback: Callable[[str, str], None] | None = None,
        logs_per_sec: int = 0,
        exit_event: threading.Event | None = None,
    ):
        if isinstance(named_logs, Kube.NamedLogs):
            named_logs = [named_logs]

        named_logs = list(named_logs)

        if not console:
            console = Console()

        alive_threads = len(named_logs)
        lock = threading.Lock()

        def print_log(logs: Kube.NamedLogs, color: str):
            from token_bucket import Limiter, MemoryStorage

            TOKEN_BUCKET_SIZE = 100

            limiter = None
            if logs_per_sec > 0:
                limiter = Limiter(logs_per_sec, TOKEN_BUCKET_SIZE, MemoryStorage())

            name = f"{logs.namespace}|{logs.name}"
            for line in logs.stream:
                decoded = line.decode("utf-8").rstrip("\n")
                if callback:
                    callback(name, decoded)

                if not limiter or limiter.consume("log"):
                    console.print(
                        f"[bold]\\[{name}][/bold] {escape(decoded)}",
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
