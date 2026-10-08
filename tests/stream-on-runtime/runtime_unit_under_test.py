# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime unit the suite drives, its `tatolab nodes` table as the suite
reads it, and the suite's own directory and venv.

The runtime unit is what `cargo xtask build-runtime` lays out:
`bin/tatolabd`, `bin/tatolab` and `lib/tatolab/lend/`. It is found at
`$STREAMLIB_RUNTIME_UNIT_DIRECTORY`, else `<repository root>/target/tatolab-runtime`.

The suite directory is the `--project` every fixture stream is started with by
default, and the suite venv's interpreter is its `--interpreter`: a venv holding
`tatolab-stream`, the fixture streams' own dependencies and the test tooling,
and no `tatolab.runtime` — a processor interpreter borrows that from the lend.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

#: The directory this suite's tests, fixture streams and node modules live in.
STREAM_ON_RUNTIME_SUITE_DIRECTORY = Path(__file__).resolve().parent

REPOSITORY_ROOT = STREAM_ON_RUNTIME_SUITE_DIRECTORY.parents[1]

#: The suite venv's interpreter, as its own path: never resolved, because a
#: venv's interpreter is a symlink whose own path is what makes it that venv's.
SUITE_VENV_INTERPRETER = Path(sys.executable)

#: The suite venv's root, which a `tatolab` project's `.venv` symlinks.
SUITE_VENV_PREFIX = Path(sys.prefix)

RUNTIME_UNIT_DIRECTORY_ENVIRONMENT_VARIABLE = "STREAMLIB_RUNTIME_UNIT_DIRECTORY"

DEFAULT_RUNTIME_UNIT_DIRECTORY = REPOSITORY_ROOT / "target" / "tatolab-runtime"

LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT = Path("lib/tatolab/lend")

PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND = Path(
    "tatolab/runtime/_processor_interpreter_bootstrap.py"
)

OBSERVATION_VERB_TIMEOUT_SECONDS = 60.0

#: The header `tatolab nodes` prints over its rows.
TATOLAB_NODES_TABLE_COLUMN_NAMES = (
    "RUNTIME_NAME",
    "RUNTIME_ID",
    "LOCAL_API_SOCKET",
    "PID",
    "ALIVE?",
    "HINT",
)

#: `tatolab nodes` pads its columns apart with two or more spaces; the last,
#: HINT, may carry single spaces of its own.
TATOLAB_NODES_TABLE_COLUMN_SEPARATOR = re.compile(r" {2,}")

#: What `tatolab nodes` prints over an empty registry, naming the registry directory.
TATOLAB_NODES_EMPTY_REGISTRY_MESSAGE = re.compile(
    r"No running nodes found in (?P<node_registry_directory>.+)\."
)

#: The registry's directory inside the runtime directory.
NODE_REGISTRY_DIRECTORY_NAME = "nodes"


@dataclass(frozen=True)
class RuntimeUnitUnderTest:
    """The runtime unit's two binaries and its lend, each checked present."""

    runtime_unit_directory: Path
    tatolabd_executable: Path
    tatolab_executable: Path
    lend_directory: Path


def runtime_unit_directory_named_by_the_environment() -> Path:
    """`$STREAMLIB_RUNTIME_UNIT_DIRECTORY`, else the repository's own build."""
    named = os.environ.get(RUNTIME_UNIT_DIRECTORY_ENVIRONMENT_VARIABLE)
    return Path(named) if named else DEFAULT_RUNTIME_UNIT_DIRECTORY


def locate_the_runtime_unit() -> "RuntimeUnitUnderTest | str":
    """The runtime unit, or the reason none is usable, naming what is missing."""
    runtime_unit_directory = runtime_unit_directory_named_by_the_environment()
    runtime_unit = RuntimeUnitUnderTest(
        runtime_unit_directory=runtime_unit_directory,
        tatolabd_executable=runtime_unit_directory / "bin" / "tatolabd",
        tatolab_executable=runtime_unit_directory / "bin" / "tatolab",
        lend_directory=runtime_unit_directory / LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT,
    )
    required_files = (
        runtime_unit.tatolabd_executable,
        runtime_unit.tatolab_executable,
        runtime_unit.lend_directory / PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND,
    )
    missing_files = [str(required) for required in required_files if not required.is_file()]
    if missing_files:
        return (
            f"no runtime unit at {runtime_unit_directory} (set "
            f"{RUNTIME_UNIT_DIRECTORY_ENVIRONMENT_VARIABLE} to name another): missing "
            f"{', '.join(missing_files)}. Build one with `cargo xtask build-runtime`."
        )
    return runtime_unit


def run_tatolab_observation_verb_in_environment(
    runtime_unit: RuntimeUnitUnderTest,
    environment: "dict[str, str]",
    *verb_arguments: str,
    timeout: float = OBSERVATION_VERB_TIMEOUT_SECONDS,
) -> "subprocess.CompletedProcess[str]":
    """The runtime unit's `tatolab <verb_arguments...>` in `environment`, to completion, output captured."""
    return subprocess.run(
        [str(runtime_unit.tatolab_executable), *verb_arguments],
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
        env=environment,
    )


@dataclass(frozen=True)
class TatolabNodesTableRow:
    """One row of `tatolab nodes`: a registered runtime, and whether its local API answered."""

    runtime_name: str
    runtime_id: str
    local_api_socket_path: Path
    pid: int
    local_api_answered: bool
    hint: str


def rows_of_the_tatolab_nodes_table(tatolab_nodes_output: str) -> "list[TatolabNodesTableRow]":
    """The rows `tatolab nodes` printed; none for its empty-registry message."""
    lines = tatolab_nodes_output.splitlines()
    if not lines or TATOLAB_NODES_EMPTY_REGISTRY_MESSAGE.fullmatch(lines[0]):
        return []
    header = TATOLAB_NODES_TABLE_COLUMN_SEPARATOR.split(lines[0].strip())
    assert tuple(header) == TATOLAB_NODES_TABLE_COLUMN_NAMES, (
        f"`tatolab nodes` printed neither its table nor its empty-registry message:\n"
        f"{tatolab_nodes_output}"
    )
    rows: "list[TatolabNodesTableRow]" = []
    for line in lines[1:]:
        columns = TATOLAB_NODES_TABLE_COLUMN_SEPARATOR.split(
            line.strip(), len(TATOLAB_NODES_TABLE_COLUMN_NAMES) - 1
        )
        if len(columns) == len(TATOLAB_NODES_TABLE_COLUMN_NAMES) - 1:
            columns.append("")
        assert len(columns) == len(TATOLAB_NODES_TABLE_COLUMN_NAMES), (
            f"`tatolab nodes` printed a row this table does not hold: {line!r}"
        )
        runtime_name, runtime_id, local_api_socket_path, pid, alive, hint = columns
        rows.append(
            TatolabNodesTableRow(
                runtime_name=runtime_name,
                runtime_id=runtime_id,
                local_api_socket_path=Path(local_api_socket_path),
                pid=int(pid),
                local_api_answered=alive == "yes",
                hint=hint,
            )
        )
    return rows


def runtime_directory_tatolab_nodes_read(tatolab_nodes_output: str) -> Path:
    """The runtime directory `tatolab nodes` read: the parent of the registry
    directory its empty-registry message names, else the one directory every
    listed local API socket sits in."""
    empty_registry_message = TATOLAB_NODES_EMPTY_REGISTRY_MESSAGE.fullmatch(
        tatolab_nodes_output.strip()
    )
    if empty_registry_message is not None:
        node_registry_directory = Path(empty_registry_message["node_registry_directory"])
        assert node_registry_directory.name == NODE_REGISTRY_DIRECTORY_NAME, (
            f"`tatolab nodes` named {node_registry_directory} as its registry, which is not a "
            f"runtime directory's `{NODE_REGISTRY_DIRECTORY_NAME}/`"
        )
        return node_registry_directory.parent
    local_api_socket_directories = {
        row.local_api_socket_path.parent
        for row in rows_of_the_tatolab_nodes_table(tatolab_nodes_output)
    }
    assert len(local_api_socket_directories) == 1, (
        f"`tatolab nodes` listed local API sockets in {sorted(map(str, local_api_socket_directories))}, "
        f"not in one runtime directory:\n{tatolab_nodes_output}"
    )
    (runtime_directory,) = local_api_socket_directories
    return runtime_directory
