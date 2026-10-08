# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The integration suite's fixtures: a stream compiled here, run on `tatolabd`.

Every test starts the runtime unit's own binaries — `tatolabd` on a compiled
graph, or `tatolab` on a project — in a runtime directory and a
`STREAMLIB_HOME` of its own, and kills whatever it started however it ends.

Fixtures, each documented where it is defined:
- `runtime_unit` — the runtime unit under test.
- `private_runtime_directories` — this test's runtime directory, home and environment.
- `start_tatolabd` — start `tatolabd` on a `@stream` function, a graph dict or graph text.
- `load_stream_graph_on_tatolabd` — the GPU-free load: start, end at the GPU, report the load.
- `make_tatolab_project` — a project directory whose `.venv` is the suite venv.
- `run_tatolab` / `start_tatolab` — `tatolab` to completion, or started.
- `run_observation_verb` — a Python observation verb, with the lend on `PYTHONPATH`.
- `held_node_module` — a node module in a project of its own whose describe parks the load.
"""

from __future__ import annotations

import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from node_module_whose_describe_holds_the_load import NodeModuleWhoseDescribeHoldsTheLoad
from runtime_process_under_test import (
    DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS,
    ENGINE_STARTED_LOG_LINE,
    STREAM_LOADED_LOG_LINE_PATTERN,
    RuntimeProcessUnderTest,
)
from runtime_unit_under_test import (
    STREAM_ON_RUNTIME_SUITE_DIRECTORY,
    SUITE_VENV_INTERPRETER,
    SUITE_VENV_PREFIX,
    RuntimeUnitUnderTest,
    locate_the_runtime_unit,
    run_observation_verb_with_the_lend,
)

#: Set to 1 to run the tests an `awaiting_macos_parity` mark would not run on
#: macOS — what the ticket bringing them does to prove it.
RUN_AWAITING_MACOS_PARITY_ENVIRONMENT_VARIABLE = "STREAMLIB_RUN_AWAITING_MACOS_PARITY"

#: Set to 1, with someone listening, to run the tests an `audible_on_macos`
#: mark would not run on macOS. The standing sweep there stays silent.
RUN_ATTENDED_AUDIBLE_TESTS_ENVIRONMENT_VARIABLE = "STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS"

#: Engine variables a test's runtime must not inherit from the shell running
#: the suite; a test that wants one passes it in `extra_environment`.
ENGINE_VARIABLES_NOT_INHERITED = (
    "STREAMLIB_APP_DIRECTORY",
    "STREAMLIB_RUNTIME_NAME",
    "STREAMLIB_QUIET",
    "STREAMLIB_HOME",
    "STREAMLIB_ICEORYX2_DOMAIN_ROOT",
)

#: Under `/tmp` and short: the engine binds Unix sockets in the runtime
#: directory, and `sun_path` holds 108 bytes. pytest's own `tmp_path` overruns it.
SHORT_TEMPORARY_DIRECTORY = Path("/tmp")

#: The Vulkan loader's own variables naming the driver files to load. Pointed
#: at a path that does not exist, they leave the loader no driver, so
#: `tatolabd` loads the graph and is then refused at the GPU, before any device
#: opens. They also keep `tatolabd` from naming its bundled driver on macOS.
VULKAN_LOADER_DRIVER_FILE_VARIABLES = ("VK_DRIVER_FILES", "VK_ICD_FILENAMES", "VK_ADD_DRIVER_FILES")

#: The engine's refusal when the Vulkan loader finds no driver, on every platform.
NO_VULKAN_DRIVER_REFUSAL = "No usable Vulkan driver (ICD) was found"

TATOLAB_RUN_TO_COMPLETION_TIMEOUT_SECONDS = 120.0

StreamOrGraph = Any


def pytest_collection_modifyitems(config: pytest.Config, items: "list[pytest.Item]") -> None:
    """Turns the platform markers and the audible marker into a skip or a strict xfail."""
    for item in items:
        audible = item.get_closest_marker("audible_on_macos")
        if audible is not None:
            reason = audible.kwargs.get("reason")
            if not reason:
                raise pytest.UsageError(f"{item.nodeid}: audible_on_macos needs reason=")
            if (
                sys.platform == "darwin"
                and os.environ.get(RUN_ATTENDED_AUDIBLE_TESTS_ENVIRONMENT_VARIABLE) != "1"
            ):
                item.add_marker(
                    pytest.mark.skip(
                        reason=f"audible on a Mac, so attended only ({reason}) — set "
                        f"{RUN_ATTENDED_AUDIBLE_TESTS_ENVIRONMENT_VARIABLE}=1 with someone listening"
                    )
                )
        linux_only = item.get_closest_marker("linux_only_capability")
        if linux_only is not None:
            reason = linux_only.kwargs.get("reason")
            if not reason:
                raise pytest.UsageError(f"{item.nodeid}: linux_only_capability needs reason=")
            if sys.platform != "linux":
                item.add_marker(pytest.mark.skip(reason=f"Linux-only: {reason}"))
        awaiting = item.get_closest_marker("awaiting_macos_parity")
        if awaiting is not None:
            issue = awaiting.kwargs.get("issue")
            if not isinstance(issue, int):
                raise pytest.UsageError(f"{item.nodeid}: awaiting_macos_parity needs issue=<number>")
            if sys.platform == "darwin":
                # Not run by default: a test waiting on another ticket's code
                # fails slowly, often at a timeout. The ticket that brings it
                # sets the variable, and the strict xfail then turns red the
                # moment the test passes, forcing the mark off.
                item.add_marker(
                    pytest.mark.xfail(
                        strict=True,
                        run=os.environ.get(RUN_AWAITING_MACOS_PARITY_ENVIRONMENT_VARIABLE) == "1",
                        reason=f"#{issue} brings this to macOS",
                    )
                )


def pytest_sessionstart(session: pytest.Session) -> None:
    """Refuse a venv holding the runtime, or one missing `tatolab.stream`.

    The suite venv is the venv every fixture stream's processor interpreters
    start from; one holding `tatolab.runtime` would let a node run without the
    lend and prove nothing about it.
    """
    lent_runtime_spec = importlib.util.find_spec("tatolab.runtime")
    if lent_runtime_spec is not None:
        pytest.exit(
            f"the suite venv at {SUITE_VENV_PREFIX} imports `tatolab.runtime` from "
            f"{lent_runtime_spec.origin or lent_runtime_spec.submodule_search_locations}: the "
            f"integration suite's venv holds `tatolab-stream` only, and processor interpreters "
            f"borrow the runtime from the lend. Unset PYTHONPATH, or rebuild the venv with "
            f"`uv sync` in {STREAM_ON_RUNTIME_SUITE_DIRECTORY}.",
            returncode=4,
        )
    if importlib.util.find_spec("tatolab.stream") is None:
        pytest.exit(
            f"the suite venv at {SUITE_VENV_PREFIX} cannot import `tatolab.stream`. Build it "
            f"with `uv sync` in {STREAM_ON_RUNTIME_SUITE_DIRECTORY}.",
            returncode=4,
        )


@pytest.fixture(scope="session")
def runtime_unit() -> RuntimeUnitUnderTest:
    """The runtime unit's `bin/tatolabd`, `bin/tatolab` and lend; a missing one ends the session."""
    located = locate_the_runtime_unit()
    if isinstance(located, str):
        pytest.exit(located, returncode=4)
    return located


@dataclass(frozen=True)
class PrivateRuntimeDirectories:
    """This test's own runtime directory and `STREAMLIB_HOME`, and the environment naming them.

    On Linux `XDG_RUNTIME_DIR` is a short directory of this test's own, so the
    runtime directory — `<XDG_RUNTIME_DIR>/streamlib`, where the registry and
    sockets live — is private. On macOS the runtime directory is fixed at
    `/tmp/streamlib-<uid>`, shared with every runtime on the machine, so a run's
    registry entry is found by its `tatolabd`'s pid.
    """

    xdg_runtime_directory: Path
    streamlib_runtime_directory: Path
    streamlib_home: Path
    environment: "dict[str, str]"


@pytest.fixture
def private_runtime_directories() -> "Iterator[PrivateRuntimeDirectories]":
    """This test's runtime directory and `STREAMLIB_HOME`, removed afterwards."""
    xdg_runtime_directory = Path(tempfile.mkdtemp(prefix="sl-", dir=SHORT_TEMPORARY_DIRECTORY))
    xdg_runtime_directory.chmod(0o700)
    streamlib_home = xdg_runtime_directory / "home"
    streamlib_home.mkdir()
    streamlib_runtime_directory = (
        xdg_runtime_directory / "streamlib"
        if sys.platform == "linux"
        else Path(f"/tmp/streamlib-{os.getuid()}")
    )
    environment = {
        name: value
        for name, value in os.environ.items()
        if name not in ENGINE_VARIABLES_NOT_INHERITED
    }
    environment["XDG_RUNTIME_DIR"] = str(xdg_runtime_directory)
    environment["STREAMLIB_HOME"] = str(streamlib_home)
    try:
        yield PrivateRuntimeDirectories(
            xdg_runtime_directory=xdg_runtime_directory,
            streamlib_runtime_directory=streamlib_runtime_directory,
            streamlib_home=streamlib_home,
            environment=environment,
        )
    finally:
        shutil.rmtree(xdg_runtime_directory, ignore_errors=True)


def environment_reaching_no_vulkan_driver(directory: Path) -> "dict[str, str]":
    """Variables that leave the Vulkan loader no driver, so a run ends at the GPU, after its load."""
    no_such_driver_file = str(directory / "no-vulkan-driver-here.json")
    return {variable: no_such_driver_file for variable in VULKAN_LOADER_DRIVER_FILE_VARIABLES}


def environment_overlaid_with(
    environment: "dict[str, str]", extra_environment: "dict[str, str | None] | None"
) -> "dict[str, str]":
    """`environment` with `extra_environment` laid over it; a `None` value removes the variable."""
    overlaid = dict(environment)
    for name, value in (extra_environment or {}).items():
        if value is None:
            overlaid.pop(name, None)
        else:
            overlaid[name] = value
    return overlaid


def stream_graph_text_of(stream_or_graph: StreamOrGraph, stream_name: "str | None") -> str:
    """The graph file's text: a `@stream` compiled here, a graph dict as JSON, or text verbatim."""
    # Imported here so a venv missing `tatolab.stream` reaches the session guard's message.
    from tatolab.stream import compile_stream_to_graph

    if isinstance(stream_or_graph, str):
        return stream_or_graph
    if isinstance(stream_or_graph, dict):
        return json.dumps(stream_or_graph, allow_nan=False)
    return json.dumps(compile_stream_to_graph(stream_or_graph, name=stream_name), allow_nan=False)


@dataclass
class StartedRuntimeProcesses:
    """Every process a test started, killed with its descendants at teardown."""

    started: "list[RuntimeProcessUnderTest]"

    def kill_every_one(self) -> None:
        for started_process in self.started:
            started_process.kill_every_process_it_started()


@pytest.fixture
def started_runtime_processes() -> "Iterator[StartedRuntimeProcesses]":
    """The list `start_tatolabd` and `start_tatolab` add to, reaped however the test ends.

    Without the reaping a failed assertion strands a live engine holding a GPU
    context, a device, an iceoryx2 node and a socket, contaminating every later
    run on the rig.
    """
    started_runtime_processes = StartedRuntimeProcesses(started=[])
    try:
        yield started_runtime_processes
    finally:
        started_runtime_processes.kill_every_one()


@pytest.fixture
def start_tatolabd(
    runtime_unit: RuntimeUnitUnderTest,
    private_runtime_directories: PrivateRuntimeDirectories,
    started_runtime_processes: StartedRuntimeProcesses,
    tmp_path: Path,
) -> "Callable[..., RuntimeProcessUnderTest]":
    """Start `tatolabd` on a stream, in a session of its own; returns its `RuntimeProcessUnderTest`.

    `start_tatolabd(stream_or_graph, *, stream_name=None, project_directory=<suite directory>,
    interpreter=<suite venv python>, extra_environment=None, working_directory=None)`:
    - `stream_or_graph` is a `@stream` function (compiled here with
      `compile_stream_to_graph(..., name=stream_name)`), a graph dict (written
      as JSON), or a `str` written to the graph file verbatim;
    - `project_directory` and `interpreter` are passed as given — a relative
      one is read from `working_directory` — and may be `bytes`;
    - `extra_environment` is laid over this test's environment, and a `None`
      value removes that variable;
    - `working_directory` defaults to a fresh directory of the test's own, so
      nothing a node imports can come from the runtime's working directory.
    """
    start_count = 0

    def start(
        stream_or_graph: StreamOrGraph,
        *,
        stream_name: "str | None" = None,
        project_directory: "str | bytes | Path" = STREAM_ON_RUNTIME_SUITE_DIRECTORY,
        interpreter: "str | bytes | Path" = SUITE_VENV_INTERPRETER,
        extra_environment: "dict[str, str | None] | None" = None,
        working_directory: "Path | None" = None,
    ) -> RuntimeProcessUnderTest:
        nonlocal start_count
        start_count += 1
        stream_graph_file = tmp_path / f"stream-graph-{start_count}.json"
        stream_graph_file.write_text(stream_graph_text_of(stream_or_graph, stream_name), encoding="utf-8")
        if working_directory is None:
            working_directory = tmp_path / f"tatolabd-working-directory-{start_count}"
            working_directory.mkdir()
        process = subprocess.Popen(
            [
                str(runtime_unit.tatolabd_executable),
                "--stream-graph",
                str(stream_graph_file),
                "--project",
                project_directory,
                "--interpreter",
                interpreter,
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            bufsize=1,
            start_new_session=True,
            cwd=working_directory,
            env=environment_overlaid_with(
                private_runtime_directories.environment, extra_environment
            ),
        )
        started_tatolabd = RuntimeProcessUnderTest(
            process,
            command_description=f"tatolabd --stream-graph {stream_graph_file}",
            streamlib_runtime_directory=private_runtime_directories.streamlib_runtime_directory,
            hosting_tatolabd_is_a_child=False,
        )
        started_runtime_processes.started.append(started_tatolabd)
        return started_tatolabd

    return start


@dataclass(frozen=True)
class StreamGraphLoadOutcome:
    """How one GPU-free load on `tatolabd` ended."""

    #: Whether `tatolabd` logged the stream loaded.
    loaded: bool
    #: The stream's name as the engine cast it, when the graph named one.
    loaded_stream_name: "str | None"
    #: How many of the stream's nodes loaded; the local API's is not counted.
    loaded_node_count: "int | None"
    #: `tatolabd`'s refusal, from its final `tatolabd: <reason>` line on.
    refusal: "str | None"
    exit_status: int
    stderr_text: str


@pytest.fixture
def load_stream_graph_on_tatolabd(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]", tmp_path: Path
) -> "Callable[..., StreamGraphLoadOutcome]":
    """Load a stream on `tatolabd` with no Vulkan driver reachable, so the run ends at the GPU.

    Takes `start_tatolabd`'s arguments. Needs no device: a graph the load
    refuses ends with that refusal; one it loads logs its load line and is then
    refused at the GPU, before any device opens. A loaded graph whose run ended
    any other way — the engine started, or a refusal other than the missing
    driver — fails the test, so a driver override that did not take cannot
    start a stream's devices unseen.
    """

    def load(stream_or_graph: StreamOrGraph, **start_arguments: Any) -> StreamGraphLoadOutcome:
        extra_environment = {
            **environment_reaching_no_vulkan_driver(tmp_path),
            **(start_arguments.pop("extra_environment", None) or {}),
        }
        started_tatolabd = start_tatolabd(
            stream_or_graph, extra_environment=extra_environment, **start_arguments
        )
        exit_status = started_tatolabd.await_exit(timeout=DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS)
        stderr_text = started_tatolabd.stderr_text
        load_line = STREAM_LOADED_LOG_LINE_PATTERN.search(stderr_text)
        refusal = started_tatolabd.refusal()
        if load_line is not None:
            assert ENGINE_STARTED_LOG_LINE not in stderr_text, (
                f"the engine started a stream that was to end at the GPU:\n"
                f"{started_tatolabd.recent_stderr()}"
            )
            assert refusal is not None and NO_VULKAN_DRIVER_REFUSAL in refusal, (
                f"a loaded stream was to be refused for the missing Vulkan driver; it ended "
                f"{exit_status}:\n{started_tatolabd.recent_stderr()}"
            )
        return StreamGraphLoadOutcome(
            loaded=load_line is not None,
            loaded_stream_name=load_line.group("stream_name") if load_line else None,
            loaded_node_count=int(load_line.group("stream_node_count")) if load_line else None,
            refusal=refusal,
            exit_status=exit_status,
            stderr_text=stderr_text,
        )

    return load


@pytest.fixture
def make_tatolab_project(tmp_path: Path) -> "Callable[..., Path]":
    """Make a `tatolab` project directory whose `.venv` symlinks the suite venv.

    `make_tatolab_project(files={relative_path: text}, directory_name="app") -> Path`:
    each file is written under the project, and `.venv` points at the suite
    venv's root, so `tatolab` finds `<project>/.venv/bin/python` holding
    `tatolab-stream` and nothing of the runtime.
    """

    def make(files: "dict[str, str] | None" = None, directory_name: str = "app") -> Path:
        project_directory = tmp_path / directory_name
        project_directory.mkdir(parents=True)
        (project_directory / ".venv").symlink_to(SUITE_VENV_PREFIX, target_is_directory=True)
        for relative_path, text in (files or {}).items():
            project_file = project_directory / relative_path
            project_file.parent.mkdir(parents=True, exist_ok=True)
            project_file.write_text(text, encoding="utf-8")
        return project_directory

    return make


@pytest.fixture
def start_tatolab(
    runtime_unit: RuntimeUnitUnderTest,
    private_runtime_directories: PrivateRuntimeDirectories,
    started_runtime_processes: StartedRuntimeProcesses,
) -> "Callable[..., RuntimeProcessUnderTest]":
    """Start `tatolab <arguments...>` in a session of its own; returns its `RuntimeProcessUnderTest`.

    `start_tatolab(*arguments, working_directory, extra_environment=None)`. Its
    standard error carries its `tatolabd`'s, so the same waits read both, and its
    registry entry is the one naming a child of it.
    """

    def start(
        *arguments: "str | Path",
        working_directory: Path,
        extra_environment: "dict[str, str | None] | None" = None,
    ) -> RuntimeProcessUnderTest:
        command = [str(runtime_unit.tatolab_executable), *map(str, arguments)]
        process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            bufsize=1,
            start_new_session=True,
            cwd=working_directory,
            env=environment_overlaid_with(
                private_runtime_directories.environment, extra_environment
            ),
        )
        started_tatolab = RuntimeProcessUnderTest(
            process,
            command_description=" ".join(["tatolab", *map(str, arguments)]),
            streamlib_runtime_directory=private_runtime_directories.streamlib_runtime_directory,
            hosting_tatolabd_is_a_child=True,
        )
        started_runtime_processes.started.append(started_tatolab)
        return started_tatolab

    return start


@pytest.fixture
def run_tatolab(
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
) -> "Callable[..., subprocess.CompletedProcess[str]]":
    """Run `tatolab <arguments...>` to completion; returns the completed process, output captured.

    `run_tatolab(*arguments, working_directory, extra_environment=None, timeout=120.0)`.
    Started through `start_tatolab`, so a run that outlives `timeout` fails the
    test and the `tatolabd` it started is killed with it.
    """

    def run(
        *arguments: "str | Path",
        working_directory: Path,
        extra_environment: "dict[str, str | None] | None" = None,
        timeout: float = TATOLAB_RUN_TO_COMPLETION_TIMEOUT_SECONDS,
    ) -> "subprocess.CompletedProcess[str]":
        started_tatolab = start_tatolab(
            *arguments, working_directory=working_directory, extra_environment=extra_environment
        )
        exit_status = started_tatolab.await_exit(timeout=timeout)
        return subprocess.CompletedProcess(
            args=started_tatolab.process.args,
            returncode=exit_status,
            stdout=started_tatolab.stdout_text,
            stderr=started_tatolab.stderr_text,
        )

    return run


@pytest.fixture
def run_observation_verb(
    runtime_unit: RuntimeUnitUnderTest, private_runtime_directories: PrivateRuntimeDirectories
) -> "Callable[..., subprocess.CompletedProcess[str]]":
    """Run a Python observation verb (`nodes`, `graph`, `tap`, `logs`, ...) against this test's runtimes.

    `run_observation_verb(*verb_arguments, timeout=60.0)`: the suite venv's
    interpreter runs `-m tatolab.runtime.cli` with the lend leading `PYTHONPATH`,
    in this test's runtime directory.
    """

    def run(*verb_arguments: str, **keyword_arguments: Any) -> "subprocess.CompletedProcess[str]":
        return run_observation_verb_with_the_lend(
            runtime_unit,
            private_runtime_directories.environment,
            *verb_arguments,
            **keyword_arguments,
        )

    return run


@pytest.fixture
def held_node_module(
    request: pytest.FixtureRequest, tmp_path: Path
) -> "Iterator[NodeModuleWhoseDescribeHoldsTheLoad]":
    """A node module in a project directory of its own, whose describe parks `tatolabd`'s load.

    Name `<held_node_module.name>:LoadedFrameRelay` in a graph started with
    `project_directory=held_node_module.project_directory`, then
    `wait_until_the_load_reaches_the_import()` and `release_the_load()`.
    """
    project_directory = tmp_path / "held-describe-project"
    project_directory.mkdir()
    node_module = NodeModuleWhoseDescribeHoldsTheLoad(
        project_directory, "runtime_load_held_nodes_" + re.sub(r"\W", "_", request.node.name)
    )
    try:
        yield node_module
    finally:
        node_module.close()
