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
import json
import logging
import time

import pandas as pd
import yaml

import benchmark

from pathlib import Path
from typing import TypedDict, TypeAlias

from rich.logging import RichHandler
from rich.prompt import Confirm

from utils import get_main_config

BASE_PATH = Path(__file__).absolute().parent
EXPERIMENTS_PATH = BASE_PATH / "experiments"
PROGRESS_PATH = BASE_PATH / "progress"
DELAY_SECONDS = 3

LOG = logging.getLogger(__name__)
LOG.addHandler(RichHandler())
LOG.setLevel(logging.INFO)


ParameterValue: TypeAlias = str | int | float | bool


class NamedParameterValue(TypedDict):
    name: str
    value: ParameterValue | None


class NameOnlyParameterValue(TypedDict):
    name: str


class Replace(TypedDict):
    match: dict[str, ParameterValue | NamedParameterValue]
    set: dict[str, ParameterValue]


class Experiment(TypedDict):
    reload_every: int
    benchmark: str
    scalefactor: int
    time: int
    rate: int
    parameters: dict[
        str,
        list[NamedParameterValue | ParameterValue | None]
        | NamedParameterValue
        | ParameterValue,
    ]
    order: list[str] | None
    replace: list[Replace] | None


def load_experiment(exp_name: str) -> tuple[Experiment, Path] | None:
    exp_path = EXPERIMENTS_PATH / f"{exp_name}.yaml"
    if not exp_path.exists():
        exp_path = EXPERIMENTS_PATH / f"{exp_name}.yml"
    if not exp_path.exists():
        return None

    with open(exp_path) as f:
        return yaml.safe_load(f), exp_path


def replace_values(
    replacements: list[Replace], named_values: dict[str, NamedParameterValue]
):
    for replace in replacements:
        is_matched = True
        for param, value in replace["match"].items():
            if param not in named_values:
                raise ValueError(f"Unknown parameter \"{param}\" specified in replace.match")
            if isinstance(value, dict):
                if value["name"] != named_values[param]["name"]:
                    is_matched = False
                    break
            elif value != named_values[param]["value"]:
                is_matched = False
                break

        if is_matched:
            for param, value in replace["set"].items():
                if param not in named_values:
                    raise ValueError(f"Unknown parameter \"{param}\" specified in replace.set")
                named_values[param] = {
                    "name": named_values[param]["name"],
                    "value": value,
                }


def benchmark_args(exp: Experiment, prefix: str):
    """Generates the benchmark arguments for the given experiment

    Args:
        exp: The experiment parameters

    Yields:
        A tuple of the benchmark arguments and the corresponding metadata
    """
    benchmark = exp["benchmark"]
    scalefactor = exp["scalefactor"]
    time = exp["time"]
    rate = exp["rate"]

    base_args = [
        "-s",
        f"benchmark={benchmark}",
        "-s",
        f"scalefactor={scalefactor}",
        "-s",
        f"time={time}",
        "-s",
        f"rate={rate}",
    ]

    base_metadata: dict[str, ParameterValue] = {
        "benchmark": benchmark,
        "scalefactor": scalefactor,
        "time": time,
        "rate": rate,
    }

    parameters = exp["parameters"]

    # Get the order of iteration of the parameters
    order = exp.get("order", None) or sorted(parameters.keys())
    if set(order) != set(parameters.keys()):
        raise ValueError(
            f"The parameter names in order {order} does not those in parameters {parameters.keys()}"
        )

    named_parameters: dict[str, list[NamedParameterValue]] = {}
    tag_params = set()
    for param, values in parameters.items():
        if not isinstance(values, list):
            values = [values]

        named_values: list[NamedParameterValue] = []
        for value in values:
            if isinstance(value, dict):
                name = value["name"]
                value = value["value"]
            else:
                param_suffix = param.split(".")[-1]
                name = f"{param_suffix}{value}"
                value = value
            named_values.append({"name": name, "value": value})

        # Only include in the tag the params with more than one values
        if len(values) > 1:
            tag_params.add(param)

        named_parameters[param] = named_values

    # Generate all combinations of the param values
    combinations: list[dict[str, NamedParameterValue]] = [{}]
    for param in order:
        combinations = [
            {**x, param: v} for x in combinations for v in named_parameters[param]
        ]

    def sanitize(value: ParameterValue):
        if isinstance(value, str):
            return value.replace(",", "\\,")
        return value

    # Generate the arguments and metadata from each combination
    for combination in combinations:
        workload_args: list[str] = []
        workload_metadata: dict[str, ParameterValue] = {}
        tag_parts: list[str] = []

        if prefix:
            tag_parts.append(prefix)

        # Apply the replacements
        replace_values(exp.get("replace") or [], combination)

        # Build the workload arguments, metadata, and tag
        for param, named_value in combination.items():
            if named_value["value"] is None:
                raise ValueError(f"Combination {combination} has a None value for \"{param}\"")

            workload_args += [
                "-s",
                f"{param}={sanitize(named_value["value"])}",
            ]
            workload_metadata[param] = named_value["value"]
            if param in tag_params:
                tag_parts.append(named_value["name"])

        tag = "-".join(tag_parts)
        workload_args += ["-s", f"tag={tag}"]
        workload_metadata["tag"] = tag

        yield base_args + workload_args, {**base_metadata, **workload_metadata}


class Progress:
    DONE = "done"
    ERROR = "error"
    PENDING = "pending"
    STATUSES = [DONE, ERROR, PENDING]

    progress: list[dict]

    def __init__(self, path: Path):
        self.path = path
        self.progress = []

    def load(self) -> bool:
        if self.path.exists():
            self.progress = pd.read_csv(self.path).to_dict("records")
            return True
        return False

    def save(self):
        self.path.parent.mkdir(exist_ok=True)
        pd.DataFrame(self.progress).to_csv(self.path, index=False)

    def get_or_insert(self, metadata: dict[str, ParameterValue]):
        """Returns the status for the given metadata

        If the corresponding record does not exist, a new record for the
        given metadata will be inserted and returned.

        Args:
            metadata: The metadata of the benchmark
        """
        for res in self.progress:
            if metadata.items() <= res.items():
                return res
        record = metadata.copy()
        record["status"] = self.PENDING
        self.progress.append(record)
        self.save()

        return record


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
    if args.cont and progress.load():
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
        LOG.info("[yellow]Skip the database creating and loading steps[/]", extra={"markup": True})

    main_config = get_main_config(BASE_PATH / "deploy")
    LOG.info("Using main config %s", json.dumps(main_config, indent=4))

    if not args.y and not Confirm.ask("Start the benchmark?", default=True):
        return 0

    #########################
    #   Run the benchmarks  #
    #########################
    set_arg = create_set_arg(args)
    dry_run_arg = ["--dry-run"] if args.dry_run else []
    reload_counter = 0
    for bm_args, metadata in benchmark_args(exp, args.prefix):
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
                            "Waiting %d seconds for the change to propagate", DELAY_SECONDS
                        )
                        if not args.dry_run:
                            time.sleep(DELAY_SECONDS)

                        header("Loading data")
                        benchmark.main(["load"] + bm_args + set_arg + dry_run_arg)
                        header(
                            "Waiting %d seconds for the change to propagate", DELAY_SECONDS
                        )
                        if not args.dry_run:
                            time.sleep(DELAY_SECONDS)

                        reload_counter = reload_every
                    else:
                        header(
                            "Reloading the database after %d more executions",
                            reload_counter,
                        )

                header("Running benchmark %s", json.dumps(metadata, indent=4))

                benchmark.main(["execute"] + bm_args + set_arg + dry_run_arg)

            except Exception as e:
                record["status"] = Progress.ERROR
                LOG.exception(e)
                reload_counter = 0
            else:
                record["status"] = Progress.DONE
                reload_counter -= 1

            progress.save()
            LOG.info(f"Progress written to {progress.path}")

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
        "--continue",
        "-c",
        dest="cont",
        action="store_true",
        help="If set, continue from the last saved ouput file if exists",
    )
    parser.add_argument(
        "-y",
        action="store_true",
        help="Skip the confirmation prompt",
    )
    parser.add_argument(
        "--skip-create-load",
        action="store_true",
        help="Skip the create and load steps",
    )
    args = parser.parse_args()

    exit(main(args))
