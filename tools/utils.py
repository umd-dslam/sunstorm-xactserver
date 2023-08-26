import logging
import subprocess
import yaml

from rich.logging import RichHandler
from pathlib import Path


def get_logger(
    name: str,
    level: int = logging.INFO,
    fmt: str = "%(asctime)s %(levelname)5s - %(message)s",
):
    handler = RichHandler()
    handler.setLevel(level)
    logger = logging.getLogger(name)
    logger.addHandler(RichHandler())
    logger.setLevel(level)
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


def get_regions(base_path):
    with open(base_path / "regions.yaml", "r") as yaml_file:
        return yaml.safe_load(yaml_file)


def get_context(region: str) -> str:
    kube_context = "-"
    kube_config_file = Path.home() / ".kube" / "config"
    with open(kube_config_file, "r") as kube_config_file:
        kube_config = yaml.safe_load(kube_config_file)
        for ctx in kube_config["contexts"]:
            if region in ctx["name"]:
                kube_context = ctx["name"]
                break

    return kube_context


def run_command(cmd, logger_and_info_log, dry_run, **kwargs):
    logger, info_log = logger_and_info_log
    logger.info(info_log)
    logger.debug(f"Executing: {' '.join(cmd)}")
    if not dry_run:
        subprocess.run(cmd, **kwargs)
