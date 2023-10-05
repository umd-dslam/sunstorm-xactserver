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
import itertools
import json
import logging
import time

import pandas as pd
import yaml

import benchmark

from collections import namedtuple
from pathlib import Path
from rich.logging import RichHandler
from rich.prompt import Confirm

BASE_PATH = Path(__file__).absolute().parent
EXPERIMENTS_PATH = BASE_PATH / "experiments"
PROGRESS_PATH = BASE_PATH / "progress"
DELAY_SECONDS = 3

LOG = logging.getLogger(__name__)
LOG.addHandler(RichHandler())
LOG.setLevel(logging.INFO)


def load_experiment(exp_name: str):
    exp_path = EXPERIMENTS_PATH / f"{exp_name}.yaml"
    if not exp_path.exists():
        exp_path = EXPERIMENTS_PATH / f"{exp_name}.yml"
    if not exp_path.exists():
        return None

    with open(exp_path) as f:
        return yaml.safe_load(f), exp_path


def benchmark_args(exp, prefix):
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

    base_metadata = {
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

    NamedValue = namedtuple("NamedValue", ["param", "name", "value"])
    # List of named values for each param
    named_param_values = []
    # Set of params that are included in the tag
    tag_params = set()
    for param in order:
        values = parameters[param]
        if not isinstance(values, list):
            values = [values]

        # List of named value for the current param
        named_values = []
        for item in values:
            if isinstance(item, dict):
                name = item["name"]
                value = item["value"]
            else:
                param_suffix = param.split(".")[-1]
                name = f"{param_suffix}{item}"
                value = item
            named_values.append(NamedValue(param, name, value))
        named_param_values.append(named_values)

        # Only include in the tag the params with more than one values
        if len(values) > 1:
            tag_params.add(param)

    def sanitize(value):
        if isinstance(value, str):
            return value.replace(",", "\\,")
        return value

    # Generate all combinations of the param values
    for combination in itertools.product(*named_param_values):
        workload_args = []
        workload_metadata = {}
        tag_parts = []

        if prefix:
            tag_parts.append(prefix)

        # Build the workload arguments, metadata, and tag
        for named_value in combination:
            workload_args += [
                "-s",
                f"{named_value.param}={sanitize(named_value.value)}",
            ]
            workload_metadata[named_value.param] = named_value.value
            if named_value.param in tag_params:
                tag_parts.append(named_value.name)

        tag = "-".join(tag_parts)
        workload_args += ["-s", f"tag={tag}"]
        workload_metadata["tag"] = tag

        yield base_args + workload_args, {**base_metadata, **workload_metadata}


class Progress:
    DONE = "done"
    ERROR = "error"
    PENDING = "pending"
    STATUSES = [DONE, ERROR, PENDING]

    def __init__(self, path: Path, dry_run):
        self.path = path
        self.progress = []
        self.dry_run = dry_run

    def load(self) -> bool:
        if self.path.exists():
            self.progress = pd.read_csv(self.path).to_dict("records")
            return True
        return False

    def save(self):
        if self.dry_run:
            return
        self.path.parent.mkdir(exist_ok=True)
        pd.DataFrame(self.progress).to_csv(self.path, index=False)

    def get_or_insert(self, metadata) -> dict:
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


def header(fmt, *args):
    LOG.info(f"[b yellow]{fmt}[/]", *args, extra={"markup": True})


def main(args):
    exp, exp_path = load_experiment(args.experiment)
    if exp:
        LOG.info(f'Using experiment file "{exp_path}"')
    else:
        LOG.error(f'No experiment file found for "{args.experiment}"')
        return 1

    progress_file_name = f"{args.progress or args.experiment}.csv"
    progress = Progress(PROGRESS_PATH / progress_file_name, args.dry_run)
    if args.cont and progress.load():
        LOG.info(f'Continuing from "{progress.path}"')
    else:
        LOG.info(f'Starting a new progress file "{progress.path}"')

    reload_every = exp["reload_every"]
    LOG.info(f"Reload the database every {reload_every} benchmark executions")

    if not args.y and not Confirm.ask("Start the benchmark?", default=True):
        return 0

    reload_counter = 0
    dry_run_arg = ["--dry-run"] if args.dry_run else []
    for bm_args, metadata in benchmark_args(exp, args.prefix):
        record = progress.get_or_insert(metadata)

        if record["status"] not in Progress.STATUSES:
            raise ValueError(f"Unknown status: {record['status']}")
        elif record["status"] == Progress.DONE:
            LOG.warning("Skip already run benchmark: %s", metadata)
        else:
            try:
                if reload_counter == 0:
                    header("Creating the database")

                    benchmark.main(["create"] + bm_args + dry_run_arg)
                    header(
                        "Waiting %d seconds for the change to propagate", DELAY_SECONDS
                    )
                    if not args.dry_run:
                        time.sleep(DELAY_SECONDS)

                    header("Loading data")
                    benchmark.main(["load"] + bm_args + dry_run_arg)
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
                benchmark.main(["execute"] + bm_args + dry_run_arg)
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
    args = parser.parse_args()

    exit(main(args))
