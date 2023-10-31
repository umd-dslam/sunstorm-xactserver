import argparse

from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
from utils import (
    Kube,
    get_main_config,
    get_logger,
    get_namespaces,
    run_subprocess,
)

LOG = get_logger(__name__)
BASE_PATH = Path(__file__).parent.resolve() / "deploy"

def run_kubectl(namespace, region, kubectl_args, dry_run):
    context = Kube.get_context(BASE_PATH, region)
    cmds = [
        "kubectl",
        "--namespace",
        namespace,
        "--context",
        context,
    ] + kubectl_args
    run_subprocess(
        cmds,
        (LOG, f'[{namespace}] Running: {" ".join(cmds)}'),
        dry_run,
        check=True,
    )


def main(cmd_args: list[str]):
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "kubectl_args", nargs="*", help="Arguments to pass to kubectl."
    )
    parser.add_argument(
        "--namespaces",
        "-n",
        nargs="*",
        help="The namespaces to access.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Don't actually run the commands.",
    )
    args = parser.parse_args(cmd_args)

    config = get_main_config(BASE_PATH)
    namespaces = {
        ns: info 
        for ns, info in get_namespaces(config).items()
        if not args.namespaces or ns in args.namespaces
    }

    max_workers = len(namespaces)
    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        for namespace, ns_info in namespaces.items():
            executor.submit(
                run_kubectl,
                namespace,
                str(ns_info["region"]),
                args.kubectl_args,
                args.dry_run,
            )


if __name__ == "__main__":
    import sys

    main(sys.argv[1:])
