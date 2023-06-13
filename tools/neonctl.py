#!/usr/bin/python3
# A tool to manage a local multi-region neon cluster
# Usage:
#   python3 tools/neonctl.py create <neon_src_dir> <data_dir> [--num-regions <num>] [--pg-version <version>] [--region-prefix <prefix>]
#   python3 tools/neonctl.py start <neon_src_dir> <data_dir>
#   python3 tools/neonctl.py stop <neon_src_dir> <data_dir>
#   python3 tools/neonctl.py destroy <neon_src_dir> <data_dir>
#
# Example:
#   python3 tools/neonctl.py create ~/src/neon ~/neon_data --num-regions 3 --pg-version 14
#

import os
import shutil
import subprocess
from typing import List, Dict

from utils import get_logger, Command, initialize_and_run_commands
from neonclone import clone_neon

logger = get_logger(__name__)


class Neon:
    def __init__(self, bin: str, env: Dict[str, str], dry_run: bool):
        self.bin = bin
        self.env = env
        self.dry_run = dry_run

    def run(self, args: List[str], check=True, **kwargs):
        cwd = kwargs.get("cwd", os.getcwd())
        logger.info(f"[{cwd}]: {self.bin} {' '.join(args)}")

        if self.dry_run:
            return

        try:
            subprocess.run(
                [self.bin] + args, env=self.env, check=check, **kwargs
            )
        except subprocess.CalledProcessError as e:
            logger.error(f"Failed to run {self.bin} {' '.join(args)}")
            raise e


class NeonCommand(Command):
    def add_arguments(self, parser):
        parser.add_argument(
            "neon_src_dir", type=str, help="The directory to neon codebase"
        )
        parser.add_argument("data_dir", type=str, help="The directory of neon data")
        parser.add_argument("--dry-run", action="store_true", help="Dry run")

    def get_neon(self, args):
        return Neon(
            os.path.join(args.neon_src_dir, "target/debug/neon_local"),
            {
                "POSTGRES_DISTRIB_DIR": os.path.join(args.neon_src_dir, "pg_install"),
            },
            args.dry_run,
        )

    def get_regions_in_root_dir(self, args):
        return sorted(
            [
                f
                for f in os.listdir(args.data_dir)
                if os.path.isdir(os.path.join(args.data_dir, f))
            ]
        )
    
    def start_all_regions(self, args):
        neon = self.get_neon(args)
        regions = self.get_regions_in_root_dir(args)
        for region in regions:
            region_dir = os.path.join(args.data_dir, region)
            neon.run(["start"], cwd=region_dir)
            neon.run(
                ["endpoint", "start", region],
                cwd=region_dir,
            )

        
    def stop_all_regions(self, args):
        neon = self.get_neon(args)
        regions = self.get_regions_in_root_dir(args)
        for region in regions:
            region_dir = os.path.join(args.data_dir, region)
            neon.run(["stop"], cwd=region_dir, check=False)


class CreateCommand(NeonCommand):
    NAME = "create"
    HELP = "Create a local multi-region neon cluster"

    def add_arguments(self, parser):
        super().add_arguments(parser)
        parser.add_argument("--num-regions", "-r", type=int, default=3)
        parser.add_argument("--pg-version", choices=["14", "15"], default="14")
        parser.add_argument("--region-prefix", "-p", type=str, default="r")

    def do_command(self, args):
        neon = self.get_neon(args)

        region_names = [f"{args.region_prefix}{i}" for i in range(args.num_regions + 1)]

        logger.info("============== INITIALIZING THE GLOBAL REGION ==============")
        global_region_dir = os.path.join(args.data_dir, region_names[0])
        if not args.dry_run:
            os.makedirs(global_region_dir, exist_ok=True)
        neon.run(["init", "--pg-version", args.pg_version], cwd=global_region_dir)
        neon.run(["start"], cwd=global_region_dir)
        neon.run(
            ["tenant", "create", "--set-default", "--pg-version", args.pg_version],
            cwd=global_region_dir,
        )
        neon.run(
            ["endpoint", "start", region_names[0], "--pg-version", args.pg_version],
            cwd=global_region_dir,
        )

        logger.info(f"============== CREATING TIMELINES FOR {args.num_regions} REGIONS ==============")
        for i, region in enumerate(region_names):
            if i == 0:
                continue
            neon.run(
                [
                    "timeline",
                    "branch",
                    "--branch-name",
                    region,
                    "--region-id",
                    str(i),
                ],
                cwd=global_region_dir,
            )

        neon.run(["stop"], cwd=global_region_dir)

        logger.info("============== CLONING THE GLOBAL REGION TO OTHER REGIONS ==============")
        for i, region in enumerate(region_names):
            if i == 0:
                continue
            region_dir = os.path.join(args.data_dir, region)
            logger.info(f'Cloning "{global_region_dir}" into "{region_dir}"')
            if not args.dry_run:
                os.makedirs(region_dir, exist_ok=True)
                clone_neon(global_region_dir, region_dir, i)

        logger.info("============== CREATING ENDPOINTS FOR EACH NON-GLOBAL REGION ==============")
        for i, region in enumerate(region_names):
            if i == 0:
                continue
            region_dir = os.path.join(args.data_dir, region)
            neon.run(
                [
                    "endpoint",
                    "create",
                    region,
                    "--branch-name",
                    region,
                    "--pg-port",
                    str(55432 + 2 * i),
                    "--http-port",
                    str(55432 + 2 * i + 1),
                    "--pg-version",
                    args.pg_version,
                ],
                cwd=region_dir,
            )


class StartCommand(NeonCommand):
    NAME = "start"
    HELP = "Start a local multi-region neon cluster"

    def do_command(self, args):
        self.start_all_regions(args)


class StopCommand(NeonCommand):
    NAME = "stop"
    HELP = "Stop a local multi-region neon cluster"

    def do_command(self, args):
        self.stop_all_regions(args)


class DestroyCommand(NeonCommand):
    NAME = "destroy"
    HELP = "Destroy a local multi-region neon cluster"

    def do_command(self, args):
        self.stop_all_regions(args)
        region_names = self.get_regions_in_root_dir(args)
        for region in region_names:
            region_dir = os.path.join(args.data_dir, region)
            logger.info("Removing %s", region_dir)
            if not args.dry_run:
                shutil.rmtree(region_dir)


if __name__ == "__main__":
    initialize_and_run_commands(
        "Manage a local multi-region neon cluster",
        [
            CreateCommand,
            StartCommand,
            StopCommand,
            DestroyCommand,
        ],
    )
