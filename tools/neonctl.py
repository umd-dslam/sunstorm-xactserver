#!/usr/bin/python3
# A tool to manage a local multi-region neon cluster
#
# Example:
#   export NEON_DIR=~/src/neon
#   export XACTSERVER_DIR=~/src/xactserver
#   python3 tools/neonctl.py create ~/neon_data -r 3
#   python3 tools/neonctl.py start ~/neon_data
#   python3 tools/neonctl.py destroy ~/neon_data
#

import os
import signal
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
        logger.info(f"[{cwd}]: {os.path.basename(self.bin)} {' '.join(args)}")

        if self.dry_run:
            return

        try:
            subprocess.run([self.bin] + args, env=self.env, check=check, **kwargs)
        except subprocess.CalledProcessError as e:
            logger.error(f"Failed to run {self.bin} {' '.join(args)}")
            raise e


class XactServer:
    def __init__(self, bin: str, env: Dict[str, str], dry_run: bool):
        self.bin = bin
        self.env = env
        self.dry_run = dry_run

    def run(self, args: List[str], **kwargs):
        cwd = kwargs.get("cwd", os.getcwd())
        bin_name = self.bin if self.dry_run else os.path.basename(self.bin)
        logger.info(f"[{cwd}]: {bin_name} {' '.join(args)}")

        if self.dry_run:
            return

        with open(os.path.join(cwd, "xactserver.pid"), "w") as f:
            process = subprocess.Popen(
                [self.bin] + args,
                env=self.env,
                start_new_session=True,
                stderr=subprocess.STDOUT,
                stdout=subprocess.DEVNULL,
                **kwargs,
            )
            f.write(str(process.pid))

    def stop(self, cwd=None):
        cwd = os.getcwd() if cwd is None else cwd

        if self.dry_run:
            return

        pid_file = os.path.join(cwd, "xactserver.pid")

        if not os.path.isfile(pid_file):
            logger.info("No xactserver.pid file found. Skipping")
            return

        with open(pid_file, "r") as f:
            pid = int(f.read())
            logger.info(f"Stopping xactserver with pid {pid}")
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                logger.info(f"Process with pid {pid} does not exist. Skipping")


class NeonCommand(Command):
    def __init__(self):
        super().__init__()
        self.neon_bin = None
        self.xactserver_bin = None

    def add_arguments(self, parser):
        parser.add_argument("data_dir", type=str, help="The directory to neon data")
        parser.add_argument(
            "--neon-dir", type=str, help="The directory to neon codebase"
        )
        parser.add_argument(
            "--xactserver-dir", type=str, help="The directory to xactserver codebase"
        )
        parser.add_argument(
            "--no-xactserver",
            action="store_true",
            help="Print the command to start xactserver but don't actually start it",
        )
        parser.add_argument("--dry-run", action="store_true", help="Dry run")

    def get_neon(self, args):
        if self.neon_bin is None:
            neon_dir = args.neon_dir or os.environ.get("NEON_DIR", ".")
            neon_bin_candidates = [
                os.path.abspath(os.path.join(neon_dir, b))
                for b in [
                    "target/debug/neon_local",
                    "target/release/neon_local",
                ]
            ]

            neon_bin_path = None
            for b in neon_bin_candidates:
                if os.path.isfile(b):
                    neon_bin_path = b
                    break

            if neon_bin_path is None:
                logger.critical(
                    "Cannot find neon_local binary. Specify --neon-dir or set NEON_DIR environment variable"
                )
                exit(1)

            logger.info("Using neon_local binary at: %s", neon_bin_path)

            self.neon_bin = Neon(
                neon_bin_path,
                {
                    "POSTGRES_DISTRIB_DIR": os.path.join(neon_dir, "pg_install"),
                },
                args.dry_run,
            )

        return self.neon_bin

    def get_xactserver(self, args):
        if self.xactserver_bin is None:
            xactserver_dir = args.xactserver_dir or os.environ.get(
                "XACTSERVER_DIR", "."
            )
            xactserver_bin_candidates = [
                os.path.abspath(os.path.join(xactserver_dir, b))
                for b in [
                    "target/debug/xactserver",
                    "target/release/xactserver",
                ]
            ]

            xactserver_bin_path = None
            for b in xactserver_bin_candidates:
                if os.path.isfile(b):
                    xactserver_bin_path = b
                    break

            if xactserver_bin_path is None:
                logger.critical(
                    "Cannot find xactserver binary. Specify --xactserver-dir or set XACTSERVER_DIR environment variable"
                )
                exit(1)

            logger.info("Using xactserver binary at: %s", xactserver_bin_path)

            self.xactserver_bin = XactServer(
                xactserver_bin_path, {}, args.dry_run or args.no_xactserver
            )

        return self.xactserver_bin

    def get_regions_in_root_dir(self, args):
        return sorted(
            [
                f
                for f in os.listdir(args.data_dir)
                if os.path.isdir(os.path.join(args.data_dir, f))
            ]
        )

    def start_all_regions(self, args):
        regions_and_dirs = [
            (r, os.path.join(args.data_dir, r))
            for r in self.get_regions_in_root_dir(args)
        ]

        logger.info("Starting neon in all regions")

        neon = self.get_neon(args)
        for region, dir in regions_and_dirs:
            neon.run(["start"], cwd=dir)
            neon.run(
                ["endpoint", "start", region],
                cwd=dir,
            )

        if args.no_xactserver:
            logger.warning(
                "Xactservers are not started. Run the following commands in separate terminals to start the xactservers"
            )
        else:
            logger.info("Starting xactserver in all regions")

        xactserver = self.get_xactserver(args)
        xactserver_nodes = [
            f"http://localhost:{23000 + i}" for i in range(len(regions_and_dirs))
        ]
        for i, (region, dir) in enumerate(regions_and_dirs):
            xactserver.run(
                [
                    "--node-id",
                    str(i),
                    "--listen-pg",
                    f"127.0.0.1:{10000 + i}",
                    "--connect-pg",
                    f"postgresql://cloud_admin@localhost:{55432 + 2 * i}/postgres",
                    "--nodes",
                    ",".join(xactserver_nodes),
                    "--listen-http",
                    f"127.0.0.1:{8080 + i}",
                ],
                cwd=dir,
            )

    def stop_all_regions(self, args):
        region_dirs = [
            os.path.join(args.data_dir, r) for r in self.get_regions_in_root_dir(args)
        ]
        xactserver = self.get_xactserver(args)
        neon = self.get_neon(args)

        logger.info("Stopping xactserver in all regions")
        for dir in region_dirs:
            xactserver.stop(cwd=dir)

        logger.info("Stopping neon in all regions")
        for dir in region_dirs:
            neon.run(["stop"], cwd=dir, check=False)


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

        logger.info(
            f"============== CREATING TIMELINES FOR {args.num_regions} REGIONS =============="
        )
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

        logger.info(
            "============== CLONING THE GLOBAL REGION TO OTHER REGIONS =============="
        )
        for i, region in enumerate(region_names):
            if i == 0:
                continue
            region_dir = os.path.join(args.data_dir, region)
            logger.info(f'Cloning "{global_region_dir}" into "{region_dir}"')
            if not args.dry_run:
                os.makedirs(region_dir, exist_ok=True)
                clone_neon(global_region_dir, region_dir, i)

        logger.info(
            "============== CREATING ENDPOINTS FOR EACH NON-GLOBAL REGION =============="
        )
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
