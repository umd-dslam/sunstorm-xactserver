import logging
import kubernetes.client
import kubernetes.config
import os
import subprocess
import yaml

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


def get_regions(base_path) -> (list, str):
    with open(base_path / "regions.yaml", "r") as yaml_file:
        regions_info = yaml.safe_load(yaml_file)
        regions = regions_info["regions"].keys()
        global_region = regions_info["global_region"]
        return regions, global_region


def get_context(base_path, region: str) -> str:
    contexts, _ = kubernetes.config.list_kube_config_contexts()
    context_names = [c["name"] for c in contexts]

    context = None
    with open(base_path / "regions.yaml", "r") as yaml_file:
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


def run_command(cmd, logger_and_info_log, dry_run, **kwargs):
    logger, info_log = logger_and_info_log
    logger.info(info_log)
    logger.debug(f"Executing: {' '.join(cmd)}")
    if not dry_run:
        subprocess.run(cmd, **kwargs)
