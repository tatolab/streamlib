# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime unit the suite drives, and the suite's own directory and venv.

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


def run_observation_verb_with_the_lend(
    runtime_unit: RuntimeUnitUnderTest,
    environment: "dict[str, str]",
    *verb_arguments: str,
    timeout: float = OBSERVATION_VERB_TIMEOUT_SECONDS,
) -> "subprocess.CompletedProcess[str]":
    """`PYTHONPATH=<lend> python -m tatolab.runtime.cli <verb_arguments...>`, to completion.

    The observation verbs stay in the lend's Python CLI until the native CLI
    takes them; the suite venv's interpreter runs them with the lend leading
    `PYTHONPATH`, in `environment`.
    """
    existing_python_path = environment.get("PYTHONPATH")
    return subprocess.run(
        [str(SUITE_VENV_INTERPRETER), "-m", "tatolab.runtime.cli", *verb_arguments],
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
        env={
            **environment,
            "PYTHONPATH": os.pathsep.join(
                [str(runtime_unit.lend_directory)]
                + ([existing_python_path] if existing_python_path else [])
            ),
        },
    )
