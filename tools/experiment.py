"""Runs experiments across multiple sets of parameters.

This tool runs the benchmark with the different sets of parameters specified
in an experiment file, which is a YAML file under the "experiments" directory.
The file name is the name of the experiment.

The progress is saved in a CSV file under the "progress" directory. By default,
the file name is the same as the experiment name but it can be customized.

The progress are frequently saved so that the experiment can be resumed from the
last saved point in case when the experiment is interrupted. On resuming, the
successful cases will be skipped.
"""
import argparse
import csv
import json
import logging
import time
import threading

import minio  # type: ignore
import yaml

import benchmark

from collections import OrderedDict
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import TypedDict, TypeAlias

from rich.logging import RichHandler
from rich.prompt import Confirm

from utils import get_main_config

BASE_PATH = Path(__file__).absolute().parent
EXPERIMENTS_PATH = BASE_PATH / "experiments"
PROGRESS_PATH = BASE_PATH / "workspace"
DELAY_SECONDS = 3

LOG = logging.getLogger(__name__)
LOG.addHandler(RichHandler())
LOG.setLevel(logging.INFO)


ParameterValue: TypeAlias = str | int | float | bool


class ParameterKey(TypedDict):
    name: str
    always_used_in_tag: bool


class NamedParameterValue(TypedDict):
    name: str
    value: ParameterValue | None


class NameOnlyParameterValue(TypedDict):
    name: str


ReplaceCondition: TypeAlias = dict[str, ParameterValue | NamedParameterValue]


class Replace(TypedDict):
    match: list[ReplaceCondition]
    set: dict[str, ParameterValue]


class Experiment(TypedDict):
    name: str
    dbtype: str
    reload_every: int
    benchmark: str
    scalefactor: int
    time: int
    rate: int
    isolation: str
    param_keys: list[str | ParameterKey]
    param_values: dict[
        str,
        list[NamedParameterValue | ParameterValue | None]
        | NamedParameterValue
        | ParameterValue,
    ]
    replace: list[Replace] | None


def replace_values(
    replacements: list[Replace], named_values: dict[str, NamedParameterValue]
):
    for replace in replacements:
        conditions: list[ReplaceCondition] = replace.get("match") or []
        if not isinstance(conditions, list):
            raise ValueError(
                "replace.match must be empty or a list of OR-ed conditions"
            )

        # If there are no conditions, the replacement will be applied to all
        match_or = len(conditions) == 0
        for and_clause in conditions:
            match_and = True
            for param, value in and_clause.items():
                if param not in named_values:
                    raise ValueError(
                        f'Unknown parameter "{param}" specified in replace.match'
                    )
                if isinstance(value, dict):
                    if value["name"] != named_values[param]["name"]:
                        match_and = False
                        break
                elif value != named_values[param]["value"]:
                    match_and = False
                    break
            match_or = match_or or match_and

        if match_or:
            for param, value in replace["set"].items():
                if param not in named_values:
                    raise ValueError(
                        f'Unknown parameter "{param}" specified in replace.set'
                    )
                named_values[param] = {
                    "name": named_values[param]["name"],
                    "value": value,
                }


def benchmark_args(exp: Experiment, prefix: str | None, suffix: str | None):
    """Generates the benchmark arguments for the given experiment

    Args:
        exp: The experiment parameters
        prefix: The prefix to add to the tag.
                This value is included in the metadata
        suffix: The suffix to add to the tag.
                This value is NOT included in the metadata

    Yields:
        A tuple of the benchmark arguments and the corresponding metadata
    """
    dbtype = exp["dbtype"]
    benchmark = exp["benchmark"]
    scalefactor = exp["scalefactor"]
    time = exp["time"]
    rate = exp["rate"]
    isolation = exp["isolation"]

    base_args = [
        "-s",
        f"dbtype={dbtype}",
        "-s",
        f"benchmark={benchmark}",
        "-s",
        f"scalefactor={scalefactor}",
        "-s",
        f"time={time}",
        "-s",
        f"rate={rate}",
        "-s",
        f"isolation={isolation}",
    ]

    base_metadata: dict[str, ParameterValue] = {
        "prefix": prefix or "",
        "benchmark": benchmark,
        "scalefactor": scalefactor,
        "time": time,
        "rate": rate,
        "isolation": isolation,
    }

    param_keys = OrderedDict()
    for param_key in exp["param_keys"]:
        if isinstance(param_key, str):
            param_keys[param_key] = {"always_used_in_tag": False}
        else:
            param_keys[param_key["name"]] = {
                "always_used_in_tag": param_key["always_used_in_tag"]
            }

    param_values = exp["param_values"]

    if param_keys.keys() != param_values.keys():
        raise ValueError(
            f"The parameters in param_values do not match those in param_keys"
        )

    named_parameters: dict[str, list[NamedParameterValue]] = {}
    tag_params = set()
    for param, values in param_values.items():
        if not isinstance(values, list):
            values = [values]

        named_values: list[NamedParameterValue] = []
        for value in values:
            if isinstance(value, dict):
                # If a name is specified, use it
                name = value["name"]
                value = value["value"]
            else:
                # Otherwise, use the value as the name
                param_suffix = param.split(".")[-1]
                name = f"{param_suffix}{value}"
                value = value
            named_values.append({"name": name, "value": value})

        # Only include in the tag the params with more than one values or the
        # params that are specified to always be used in the tag
        if len(values) > 1 or param_keys[param]["always_used_in_tag"]:
            tag_params.add(param)

        named_parameters[param] = named_values

    # Generate all combinations of the param values
    combinations: list[dict[str, NamedParameterValue]] = [{}]
    for param in param_keys.keys():
        combinations = [
            {**x, param: v} for x in combinations for v in named_parameters[param]
        ]

    def escape(value: ParameterValue):
        """Escape the comma in the string values"""
        if isinstance(value, str):
            return value.replace(",", "\\,")
        return value

    # Generate the arguments and metadata from each combination
    for combination in combinations:
        workload_args: list[str] = []
        workload_metadata: dict[str, ParameterValue] = {}
        tag_parts: list[str] = []

        # Add the prefix to the tag
        if prefix:
            tag_parts.append(prefix)

        tag_parts.append(exp["name"])

        # Apply the replacements
        replace_values(exp.get("replace") or [], combination)

        # Build the workload arguments, metadata, and tag
        for param, named_value in combination.items():
            value = named_value["value"]
            if value is None:
                raise ValueError(
                    f'Combination {combination} has a None value for "{param}"'
                )

            workload_args += [
                "-s",
                f"{param}={escape(value)}",
            ]
            workload_metadata[param] = value
            if param in tag_params:
                tag_parts.append(named_value["name"])

        # Add the suffix to the tag
        if suffix:
            tag_parts.append(suffix)

        tag = "-".join(tag_parts)
        workload_args += ["-s", f"tag={tag}"]

        yield base_args + workload_args, {**base_metadata, **workload_metadata}, tag


class Progress:
    DONE = "done"
    ERROR = "error"
    PENDING = "pending"
    STATUSES = [DONE, ERROR, PENDING]

    progress: list[dict[str, ParameterValue]]

    def __init__(self, path: Path):
        self.path = path
        self.progress = []

    def __str__(self):
        return str(self.progress)

    def load(self) -> bool:
        if self.path.exists():
            with open(self.path) as f:
                self.progress = list(csv.DictReader(f))
            return True
        return False

    def save(self):
        if len(self.progress) == 0:
            return

        self.path.parent.mkdir(exist_ok=True)
        keys = [p.keys() for p in self.progress]
        fieldnames = set().union(*keys)
        with open(self.path, "w") as f:
            writer = csv.DictWriter(f, fieldnames=fieldnames)
            writer.writeheader()
            writer.writerows(self.progress)

    def get_or_insert(self, metadata: dict[str, ParameterValue]):
        """Returns the status for the given metadata

        If the corresponding record does not exist, a new record for the
        given metadata will be inserted and returned.

        Args:
            metadata: The metadata of the benchmark
        """
        # Convert all values to string
        metadata = {k: str(v) for k, v in metadata.items()}

        for res in self.progress:
            if metadata.items() <= res.items():
                return res

        record: dict[str, ParameterValue] = {
            "status": self.PENDING,
            "reloaded": False,
            **metadata,
        }

        self.progress.append(record)
        self.save()

        return record


class ResultCollector:
    def __init__(self, path: Path, minio_address: str, dry_run: bool):
        self.path = path
        self.dry_run = dry_run
        self.minio = minio.Minio(
            minio_address,
            access_key="minioadmin",
            secret_key="minioadmin",
            secure=False,
        )
        self.thread_pool = ThreadPoolExecutor()
        self.in_progress_count = 0
        self.in_progress_cv = threading.Condition()

    def check_connection(self):
        try:
            self.minio.list_buckets()
        except Exception as e:
            LOG.exception("Failed to connect to MinIO", exc_info=e)
            return False

        return True

    def submit(self, tag: str):
        if not self.dry_run:
            self.thread_pool.submit(self._collect, tag)

    def join(self):
        with self.in_progress_cv:
            while self.in_progress_count > 0:
                self.in_progress_cv.wait()

        self.thread_pool.shutdown()

    def _collect(self, tag: str):
        LOG.info('Collecting the result files for "%s"', tag)
        try:
            with self.in_progress_cv:
                self.in_progress_count += 1

            regions = [
                obj.object_name
                for obj in self.minio.list_objects("results", prefix=f"{tag}/")
            ]

            def collect_region(region):
                objects = self.minio.list_objects(
                    "results", prefix=region, recursive=True
                )
                for obj in objects:
                    obj_path = self.path / obj.object_name
                    obj_path.parent.mkdir(parents=True, exist_ok=True)
                    self.minio.fget_object("results", obj.object_name, obj_path)

            list(self.thread_pool.map(collect_region, regions))

            LOG.info("Saved result files in %s", self.path / tag)

            with self.in_progress_cv:
                self.in_progress_count -= 1
                self.in_progress_cv.notify_all()
        except Exception as e:
            LOG.exception(e)


def load_experiment(exp_name: str) -> tuple[Experiment, Path] | None:
    """Loads the experiment file"""

    class Loader(yaml.SafeLoader):
        """A YAML loader that supports !include directive"""

        def __init__(self, stream):
            self._root = Path(stream.name).parent  # type: ignore
            super().__init__(stream)

        def include(self, node):
            filename = str(self.construct_scalar(node))
            path = self._root / filename
            with open(path, "r") as f:
                return yaml.load(f, Loader)

    Loader.add_constructor("!include", Loader.include)

    exp_path = EXPERIMENTS_PATH / f"{exp_name}.yaml"
    if not exp_path.exists():
        exp_path = EXPERIMENTS_PATH / f"{exp_name}.yml"
    if not exp_path.exists():
        return None

    with open(exp_path) as f:
        exp = yaml.load(f, Loader)
        exp["name"] = exp_name
        return exp, exp_path


def header(fmt: str, *args):
    LOG.info(f"[b yellow]{fmt}[/]", *args, extra={"markup": True})


def create_set_arg(args) -> list[str]:
    set_arg = []
    for s in args.set or []:
        set_arg += ["-s", s]
    return set_arg


def main(args):
    ##########################################################
    #   Print the general information about the experiment   #
    ##########################################################
    exp_and_path = load_experiment(args.experiment)
    if not exp_and_path:
        LOG.error("No experiment file found for %s", args.experiment)
        return 1

    exp, exp_path = exp_and_path
    LOG.info("Using experiment file %s", exp_path)

    progress_file_name = f"{args.progress or args.experiment}.csv"
    progress = Progress(PROGRESS_PATH / progress_file_name)
    if not args.new and progress.load():
        LOG.info(
            "[b yellow]Continuing[/] from %s", progress.path, extra={"markup": True}
        )
    else:
        LOG.info(
            "[b green]Starting[/] a new progress file %s",
            progress.path,
            extra={"markup": True},
        )

    reload_every = exp.get("reload_every") or 0
    if reload_every > 0:
        LOG.info("Reload the database every %d benchmark executions", reload_every)

    if args.skip_create_load:
        LOG.info(
            "[yellow]Skip the database creating and loading steps[/]",
            extra={"markup": True},
        )

    main_config = get_main_config(BASE_PATH / "deploy")
    LOG.info("Using main config %s", json.dumps(main_config, indent=4))

    collector = None
    if args.output:
        collector = ResultCollector(Path(args.output), args.minio, args.dry_run)
        LOG.info('Results will be collected to "%s"', collector.path)

    if not args.y and not Confirm.ask("Start the benchmark?", default=True):
        return 0

    #########################
    #   Run the benchmarks  #
    #########################
    timestamp = time.strftime("%Y%m%d-%H%M%S")

    if collector:
        LOG.info("Checking the connection to MinIO")
        if not collector.check_connection():
            return 1

    set_arg = create_set_arg(args)
    dry_run_arg = ["--dry-run"] if args.dry_run else []
    reload_counter = 0
    for bm_args, metadata, tag in benchmark_args(exp, args.prefix, timestamp):
        record = progress.get_or_insert(metadata)

        if record["status"] not in Progress.STATUSES:
            raise ValueError(f"Unknown status: {record['status']}")
        elif record["status"] == Progress.DONE:
            LOG.warning("Skip already run benchmark: %s", metadata)
        else:
            try:
                if not args.skip_create_load:
                    if reload_counter == 0:
                        header("Creating the database")

                        benchmark.main(["create"] + bm_args + set_arg + dry_run_arg)
                        header(
                            "Waiting %d seconds for the change to propagate",
                            DELAY_SECONDS,
                        )
                        if not args.dry_run:
                            time.sleep(DELAY_SECONDS)

                        header("Loading data")
                        benchmark.main(
                            ["sload" if args.sload else "load"]
                            + bm_args
                            + set_arg
                            + dry_run_arg
                        )
                        header(
                            "Waiting %d seconds for the change to propagate",
                            DELAY_SECONDS,
                        )
                        if not args.dry_run:
                            time.sleep(DELAY_SECONDS)

                        record["reloaded"] = True

                        reload_counter = reload_every
                    else:
                        header(
                            "Reloading the database after %d more executions",
                            reload_counter,
                        )

                header("Running benchmark %s", json.dumps(metadata, indent=4))

                result = benchmark.main(
                    [
                        "execute",
                        "--logs-per-sec",
                        str(args.logs_per_sec),
                    ]
                    + bm_args
                    + set_arg
                    + dry_run_arg
                )
                record.update(**result)
                record["tag"] = tag

                if collector:
                    collector.submit(tag)

            except Exception as e:
                record["status"] = Progress.ERROR
                LOG.exception(e)
                reload_counter = 0
            else:
                record["status"] = Progress.DONE
                reload_counter -= 1

            progress.save()
            LOG.info(f"Progress written to {progress.path}")

    if collector:
        collector.join()

    return 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "experiment",
        nargs="?",
        default="default",
        help='Name of the experiment under the "experiments" directory',
    )
    parser.add_argument(
        "--set",
        "-s",
        action="append",
        help="Override the values in the config file. Each argument should be in the form of key=value.",
    )
    parser.add_argument("--prefix", default="", help="Prefix to add to the tags")
    parser.add_argument("--progress", help="Progress file name")
    parser.add_argument("--dry-run", action="store_true", help="Dry run")
    parser.add_argument(
        "--new",
        "-n",
        action="store_true",
        help="If set, ignore the existing progress file and start a new one",
    )
    parser.add_argument(
        "-y",
        action="store_true",
        help="Skip the confirmation prompt",
    )
    parser.add_argument(
        "--output",
        "-o",
        help="Path to store the results. If not specified, the results will not be collected",
    )
    parser.add_argument(
        "--skip-create-load",
        action="store_true",
        help="Skip the create and load steps",
    )
    parser.add_argument(
        "--sload",
        action="store_true",
        help="Use sload for loading data",
    )
    parser.add_argument(
        "--minio",
        default="localhost:9000",
        help="The address of the MinIO server",
    )
    parser.add_argument(
        "--logs-per-sec",
        default=0,
        type=int,
        help="The number of logs to be collected per second",
    )
    args = parser.parse_args()

    exit(main(args))
