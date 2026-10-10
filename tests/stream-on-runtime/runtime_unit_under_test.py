# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime unit the suite drives, the machine directories a test keeps
under its own root, and the suite's own directory and venv.

The runtime unit is what `cargo xtask build-runtime
--machine-directories-under-a-test-root` lays out: `bin/tatolabd`, `bin/tatolab`,
`lib/tatolab/lend/` and, at its root, the marker file saying both binaries keep
the machine runtime lock, the state directory and the runtime directory under
`$TATOLAB_TEST_MACHINE_ROOT`. A unit without the marker is refused by name: its
`tatolabd` would take this machine's real lock. It is found at
`$STREAMLIB_RUNTIME_UNIT_DIRECTORY`, else `<repository root>/target/tatolab-runtime`.

The suite directory is the project every `@stream` function of the suite is
run from — `tatolab run <module>:<function>` with the suite directory as the
working directory — so its `.venv` is the interpreter that compiles each stream
and that every processor interpreter starts from: a venv holding
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

#: The file `cargo xtask build-runtime --machine-directories-under-a-test-root`
#: leaves at the unit's root once both binaries carry the feature.
MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME = "machine-directories-under-a-test-root"

#: The variable a test build of `tatolabd` and `tatolab` reads its machine root from.
TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE = "TATOLAB_TEST_MACHINE_ROOT"

#: The local API socket's name in the runtime directory: one per machine.
LOCAL_API_SOCKET_FILE_NAME = "local-api.sock"

OBSERVATION_VERB_TIMEOUT_SECONDS = 60.0


@dataclass(frozen=True)
class RuntimeUnitUnderTest:
    """The runtime unit's two binaries and its lend, each checked present, and its marker."""

    runtime_unit_directory: Path
    tatolabd_executable: Path
    tatolab_executable: Path
    lend_directory: Path


@dataclass(frozen=True)
class MachineDirectoriesUnderATestRoot:
    """The machine directories a test build keeps under one root, each by its path.

    The layout is the client contract's `TestMachineRoot`: the runtime
    directory `R/run/`, the state directory `R/state/`, and on macOS the lock
    file `R/lock/runtime.lock`; on Linux the lock is the abstract socket
    `tatolab-runtime:R`, which lives with no file.
    """

    root: Path

    @property
    def runtime_directory(self) -> Path:
        """`R/run/`: the iceoryx2 domain, the surface-sharing socket and the local API socket."""
        return self.root / "run"

    @property
    def local_api_socket_path(self) -> Path:
        """`R/run/local-api.sock`, the one socket the machine's runtime serves its local API on."""
        return self.runtime_directory / LOCAL_API_SOCKET_FILE_NAME

    @property
    def state_directory(self) -> Path:
        """`R/state/`: the kept streams and the runtime's own log."""
        return self.root / "state"

    @property
    def kept_streams_directory(self) -> Path:
        """`R/state/streams/`, one record per kept stream."""
        return self.state_directory / "streams"

    @property
    def runtime_log_directory(self) -> Path:
        """`R/state/logs/`, the runtime's own JSONL log."""
        return self.state_directory / "logs"

    @property
    def machine_runtime_lock_directory(self) -> Path:
        """`R/lock/`, the directory the macOS lock file sits in."""
        return self.root / "lock"

    @property
    def machine_runtime_lock_file(self) -> Path:
        """`R/lock/runtime.lock`, the macOS lock file a test build expects owned by this user."""
        return self.machine_runtime_lock_directory / "runtime.lock"


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
    build_command = "cargo xtask build-runtime --machine-directories-under-a-test-root"
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
            f"{', '.join(missing_files)}. Build one with `{build_command}`."
        )
    marker = runtime_unit_directory / MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME
    if not marker.is_file():
        return (
            f"the runtime unit at {runtime_unit_directory} carries no "
            f"`{MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME}` marker, so its "
            f"binaries were built without that feature and would take this machine's real "
            f"runtime lock, state directory and local API socket. The suite runs only a unit "
            f"whose directories move under {TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE}: build one "
            f"with `{build_command}`."
        )
    return runtime_unit


def run_tatolab_verb_in_environment(
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
