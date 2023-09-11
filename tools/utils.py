import itertools
import logging
import kubernetes.client
import kubernetes.config
import os
import subprocess
import threading
import yaml

from rich.console import Console
from rich.logging import RichHandler

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
    for i, r in enumerate(config["regions"]):
        namespaces[r] = {"region": r, "id": i + 1}
    return namespaces


def get_context(base_path, region: str) -> str:
    contexts, _ = kubernetes.config.list_kube_config_contexts()
    context_names = [c["name"] for c in contexts]

    context = None
    with open(base_path / "main.yaml", "r") as yaml_file:
        regions_info = yaml.safe_load(yaml_file)
        if region in regions_info["regions"]:
            regions = regions_info["regions"]
            if region in regions and regions[region] and "context" in regions[region]:
                context = regions[region]["context"]

    if context is None:
        for ctx in context_names:
            if region in ctx:
                context = ctx
                break

    if context is None:
        raise Exception(f"Could not find context for region: {region}")

    return context


def get_kube_config(base_path, region: str):
    context = get_context(base_path, region)
    config = kubernetes.client.Configuration()
    kubernetes.config.load_kube_config(
        context=context, client_configuration=config, persist_config=False
    )

    return config


def get_running_pods(kube_config, namespace, selector):
    with kubernetes.client.ApiClient(kube_config) as api_client:
        corev1 = kubernetes.client.CoreV1Api(api_client)
        pods = []
        pod_list = corev1.list_namespaced_pod(
            namespace=namespace,
            label_selector=",".join([f"{k}={v}" for k, v in selector.items()]),
        )
        for pod in pod_list.items:
            if pod.status.phase == "Running":
                pods.append(pod.metadata.name)

        return pods


def print_kube_logs_streams(logs_streams, follow=False, console=None):
    if not console:
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
        t = threading.Thread(target=print_log, args=(log_stream, color), daemon=True)
        t.start()
        threads.append(t)

    if not follow:
        # Wait for all threads to finish
        for t in threads:
            t.join()
