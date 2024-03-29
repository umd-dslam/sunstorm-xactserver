#!/usr/bin/python3
import argparse
import copy
import shutil
import toml

from functools import partial
from pathlib import Path

from utils import get_logger

LOG = get_logger(__name__)


class CloneError(Exception):
    pass


def update_inplace(prefix: str, d: dict, fn_or_val, path: list[str]):
    """Update a field in a nested dict in place

    Attributes:
        prefix: for logging
        d: the dict
        fn_or_val: a function to map old value to new value or a value to
                   replace the old value
        path: a list of keys to locate the updated value
    """
    ref = d
    for p in path[:-1]:
        ref = ref[p]
    last = path[-1]
    old = copy.copy(ref[last])
    if callable(fn_or_val):
        ref[last] = fn_or_val(ref[last])
    else:
        ref[last] = fn_or_val
    dotted = ".".join(path)
    if old != ref[last]:
        LOG.info(f"Updated '{prefix}{dotted}':\t{old} => {ref[last]}")
    return ref[last]


def deep_copy_neon(src: Path, dst: Path):
    if not src.is_dir():
        raise CloneError(f"Source directory not found or not a directory: '{src}'")

    src_neon = src / ".neon"
    if not src_neon.is_dir():
        raise CloneError(f"The '.neon' directory is not found in '{src}'")

    LOG.info(f"Found '.neon' in '{src}/'")

    if not dst.is_dir():
        raise CloneError(f"Destination directory not found or not a directory: '{dst}'")

    shutil.copytree(src_neon, dst / ".neon")
    LOG.info(f"Cloned '.neon' to '{dst}/'")


def edit_configs(ordinal: int, hostname: str | None, dst_neon: Path):
    def change_addr(hostname_and_port: str) -> str:
        """Bumps the port by `ordinal` and replaces hostname"""
        nonlocal ordinal, hostname
        old_hostname, port = hostname_and_port.split(":")
        new_port = int(port) + ordinal
        return f"{hostname or old_hostname}:{new_port}"

    #######################################
    #   Edit the pageserver.toml file     #
    #######################################
    pageserver_toml_path = dst_neon / "pageserver.toml"
    pageserver_toml = toml.load(pageserver_toml_path)
    update_pageserver_toml = partial(
        update_inplace, prefix="pageserver.toml/", d=pageserver_toml
    )
    listen_http_addr = update_pageserver_toml(
        fn_or_val=change_addr, path=["listen_http_addr"]
    )
    listen_pg_addr = update_pageserver_toml(
        fn_or_val=change_addr, path=["listen_pg_addr"]
    )
    pageserver_id = update_pageserver_toml(fn_or_val=lambda p: p + ordinal, path=["id"])
    with pageserver_toml_path.open("w") as f:
        toml.dump(pageserver_toml, f)

    #######################################
    #       Edit the config file          #
    #######################################
    config_path = dst_neon / "config"
    config = toml.load(config_path)
    update_config = partial(update_inplace, prefix="config/", d=config)
    update_config(
        fn_or_val=listen_http_addr,
        path=["pageserver", "listen_http_addr"],
    )
    update_config(
        fn_or_val=listen_pg_addr,
        path=["pageserver", "listen_pg_addr"],
    )
    update_config(
        fn_or_val=change_addr,
        path=["xactserver", "listen_pg_addr"],
    )
    update_config(
        fn_or_val=pageserver_id,
        path=["pageserver", "id"],
    )
    for sk in config["safekeepers"]:
        update_sk = partial(
            update_inplace,
            prefix="config/safekeepers.",
            d=sk,
            fn_or_val=lambda p: p + ordinal,
        )
        update_sk(path=["http_port"])
        update_sk(path=["pg_port"])

    with config_path.open("w") as f:
        toml.dump(config, f)


def edit_safekeeper_ids(ordinal: int, dst_neon: Path):
    safekeepers_path = dst_neon / "safekeepers"
    if not safekeepers_path.is_dir():
        raise CloneError(
            "Safekeepers directory not found or not a directory: "
            f"'{safekeepers_path}'"
        )

    config_path = dst_neon / "config"
    config = toml.load(config_path)
    num_sk = len(config["safekeepers"])
    for sk in config["safekeepers"]:
        old_id = sk["id"]
        new_id = sk["id"] + num_sk * ordinal
        sk_path = safekeepers_path / f"sk{old_id}"
        if not sk_path.is_dir():
            raise CloneError(f"Cannot find directory for safekeeper 'sk{old_id}'")
        sk_path = sk_path.rename(safekeepers_path / f"sk{new_id}")
        (sk_path / "safekeeper.id").unlink(missing_ok=True)

        LOG.info(f"Renamed 'safekeepers/sk{old_id}' to 'safekeepers/sk{new_id}'")
        sk["id"] = new_id

    with config_path.open("w") as f:
        toml.dump(config, f)


def clone_neon(src: str, dst: str, offset: int, hostname: str | None = None):
    # Clone the .neon directory
    dst_path = Path(dst)
    deep_copy_neon(Path(src), dst_path)

    # Edit the config files
    dst_neon_path = dst_path / ".neon"
    edit_configs(offset, hostname, dst_neon_path)
    edit_safekeeper_ids(offset, dst_neon_path)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "src", help="Path to the directory containing the .neon directory"
    )
    parser.add_argument("dst", help="Where to place the cloned .neon directory")
    parser.add_argument(
        "-o",
        "--offset",
        default=1,
        type=int,
        help="Offset number of this clone "
        "(i.e. Is this the first, second, etc. clone). "
        "Some numeric values in the cloned configs (e.g. ports) will "
        "be adjucted based on this argument (default: 1)",
    )
    parser.add_argument(
        "-n", "--hostname", help="Change hostname of the cloned data to this value"
    )
    try:
        clone_neon(**vars(parser.parse_args()))
    except CloneError as e:
        LOG.error(e)
