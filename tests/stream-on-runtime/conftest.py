# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The integration suite's fixtures: one `tatolabd` per test, and streams run on it with `tatolab run`.

Every test drives the runtime unit's own binaries under a machine root of its
own — `TATOLAB_TEST_MACHINE_ROOT`, a short fresh directory under `/tmp` — so
its `tatolabd` takes that root's machine lock, serves its local API at
`<root>/run/local-api.sock` and keeps its streams in `<root>/state/`, never the
machine's. Whatever a test started is killed however it ends, and the root is
removed.

A stream reaches the runtime only as a user's does: `tatolab run` from a
project directory, which loads it, prints its id and returns — the runtime
records the stream and runs it until `tatolab stop` or `tatolab rm` — or
`tatolab dev`, which loads it and follows it. What a test hands a loading
fixture — `stream_or_graph` — is one of:

- a `@stream` function of one of the suite's `*_streams.py` (or other non-test)
  modules, run as `tatolab run <module>:<function> [--name <display_name>]` from
  the suite directory, a project whose `.venv` is the suite venv. A function
  defined in a test module, inside another function or built at run time is
  refused by name: move it to a `*_streams.py` module, taking what varies from
  its config;
- a graph written by hand, a dict or its text verbatim — refusal tests, or a
  graph compiled in this process — run from a project of its own whose
  `.venv/bin/python` prints it as the compile document, naming
  `project_directory` (default: the suite directory) as the directory its Python
  node types are described and run from, and execs `processor_interpreter`
  (default: the suite venv's) for every other invocation;
- `TatolabRunOfAProject(working_directory, tatolab_run_arguments)`, a project's
  own `tatolab run`, verbatim.

Fixtures, each documented where it is defined:
- `runtime_unit` — the runtime unit under test; one without the test-root marker ends the session.
- `private_machine_directories` — this test's machine root, its directories and environment.
- `start_tatolabd` — start this test's `tatolabd`; returns a `TatolabdUnderTest`.
- `start_tatolabd_running_stream` — start `tatolabd`, then run a stream on it with `tatolab run`.
- `run_stream_on_tatolabd_with_no_vulkan_driver` — the GPU-free run: loaded and refused at the GPU, or refused at the load.
- `make_tatolab_project` — a project directory whose `.venv` is the suite venv.
- `run_tatolab` / `start_tatolab` — any `tatolab` command to completion, or started.
- `run_tatolab_observation_verb` — a `tatolab` verb against this test's runtime, to completion.
- `held_node_module` — a node module in a project of its own whose describe parks the load.

`TatolabdUnderTest` is the started `tatolabd`: every wait of a
`RuntimeProcessUnderTest` (marker lines of every loaded stream land on its
standard error), plus `local_api_socket_path`, `await_serving()`,
`local_api_client()` and `run_stream(stream_or_graph, ...)` →
`TatolabRunOrDevOnTheRuntime`. `tatolabd.interrupt()` is a machine shutdown:
every loaded stream is unloaded and `tatolabd` exits.
`TatolabRunOrDevOnTheRuntime.await_loaded()` waits for the line naming the
stream it loaded — `tatolabd.await_the_latest_stream_loaded()` for the latest
one, returning its id — and `stop_the_stream()` runs `tatolab stop <id>`, the
one way a test ends that stream short of the machine shutdown; a Ctrl-C to
`dev` ends the following and leaves the stream loaded. The local API serves every stream the runtime holds, so the client's
waits on one stream's graph name it: `await_every_node_running(stream=...)`.
"""

from __future__ import annotations

import functools
import importlib.util
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from local_api_client import LocalApiClient
from node_module_whose_describe_holds_the_load import NodeModuleWhoseDescribeHoldsTheLoad
from runtime_process_under_test import (
    DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS,
    ENGINE_STARTED_LOG_LINE,
    NOT_YET_SEEN,
    STREAM_START_BEGAN_LOG_LINE_PATTERN,
    TATOLAB_REFUSAL_LINE_PREFIX,
    TATOLABD_REFUSAL_LINE_PREFIX,
    RuntimeProcessUnderTest,
)
from runtime_unit_under_test import (
    STREAM_ON_RUNTIME_SUITE_DIRECTORY,
    SUITE_VENV_PREFIX,
    TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE,
    MachineDirectoriesUnderATestRoot,
    RuntimeUnitUnderTest,
    locate_the_runtime_unit,
    run_tatolab_verb_in_environment,
)
from stream_runs_on_tatolabd import (
    StreamOrGraph,
    TatolabRunOfAProject,
    tatolab_run_of_a_suite_stream_function,
    write_a_hand_written_graph_project,
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
    "STREAMLIB_QUIET",
    "STREAMLIB_HOME",
    "STREAMLIB_ICEORYX2_DOMAIN_ROOT",
    TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE,
)

#: Under `/tmp` and short: the runtime binds Unix sockets in `<root>/run/`, and
#: `sun_path` holds 104 bytes on macOS. pytest's own `tmp_path` overruns it.
SHORT_TEMPORARY_DIRECTORY = Path("/tmp")

#: The variable a test build reads its loopback HTTP listener's port from; on
#: macOS its bind is the machine runtime lock, so a test build there needs one.
TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE = "TATOLAB_TEST_LOOPBACK_HTTP_PORT"

#: The Vulkan loader's own variables naming the driver files to load. Pointed
#: at a path that does not exist, they leave the loader no driver, so a stream
#: loads and its start is then refused at the GPU, before any device opens.
#: They also keep `tatolabd` from naming the driver its lend carries.
VULKAN_LOADER_DRIVER_FILE_VARIABLES = ("VK_DRIVER_FILES", "VK_ICD_FILENAMES", "VK_ADD_DRIVER_FILES")

#: The engine's refusal when the Vulkan loader finds no driver, on every platform.
NO_VULKAN_DRIVER_REFUSAL = "No usable Vulkan driver (ICD) was found"

#: The engine's line as a stream's unload removes the processors its load added.
STREAM_UNLOAD_REMOVED_PROCESSORS_LOG_LINE_PATTERN = re.compile(
    r"\[stop\] Queued removal of (?P<processor_count>\d+) processor\(s\)"
)

#: The line naming the stream a load started: `tatolab run`'s first line on standard output, or
#: `tatolab dev`'s note on standard error, prefixed `tatolab dev: `.
RUNNING_STREAM_LINE_PATTERN = re.compile(
    r'^(?:tatolab dev: )?(?P<stream_id>\S+) running as "(?P<display_name>.*)" '
    r"\(project (?P<project_directory>.+), (?P<stream_function>[^,]+)\)$"
)

TATOLAB_RUN_TO_COMPLETION_TIMEOUT_SECONDS = 120.0

#: How often a wait on the local API socket looks again.
LOCAL_API_SOCKET_POLL_INTERVAL_SECONDS = 0.05


def reason_a_reasoned_marker_carries(item: pytest.Item, reasoned_marker_name: str) -> "str | None":
    """The `reason=` the item's `reasoned_marker_name` marker carries, `None` when it has no such
    marker; a marker without one is a usage error."""
    reasoned_marker = item.get_closest_marker(reasoned_marker_name)
    if reasoned_marker is None:
        return None
    reason = reasoned_marker.kwargs.get("reason")
    if not reason:
        raise pytest.UsageError(f"{item.nodeid}: {reasoned_marker_name} needs reason=")
    return reason


def pytest_collection_modifyitems(config: pytest.Config, items: "list[pytest.Item]") -> None:
    """Turns the platform markers and the audible marker into a skip or a strict xfail, and
    refuses a reasoned marker without its reason or a rig-only test that needs no device."""
    for item in items:
        audible_reason = reason_a_reasoned_marker_carries(item, "audible_on_macos")
        if audible_reason is not None:
            if (
                sys.platform == "darwin"
                and os.environ.get(RUN_ATTENDED_AUDIBLE_TESTS_ENVIRONMENT_VARIABLE) != "1"
            ):
                item.add_marker(
                    pytest.mark.skip(
                        reason=f"audible on a Mac, so attended only ({audible_reason}) — set "
                        f"{RUN_ATTENDED_AUDIBLE_TESTS_ENVIRONMENT_VARIABLE}=1 with someone listening"
                    )
                )
        linux_only_reason = reason_a_reasoned_marker_carries(item, "linux_only_capability")
        if linux_only_reason is not None and sys.platform != "linux":
            item.add_marker(pytest.mark.skip(reason=f"Linux-only: {linux_only_reason}"))
        rig_only_reason = reason_a_reasoned_marker_carries(item, "rig_only_capability")
        # The rig's run selects `-m requires_gpu`, so a rig-only test without it runs nowhere.
        if rig_only_reason is not None and item.get_closest_marker("requires_gpu") is None:
            raise pytest.UsageError(
                f"{item.nodeid}: rig_only_capability ({rig_only_reason}) needs requires_gpu too"
            )
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

    The suite venv is the venv every fixture stream compiles in and its
    processor interpreters start from; one holding `tatolab.runtime` would let a
    node run without the lend and prove nothing about it.
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
    """The runtime unit's `bin/tatolabd`, `bin/tatolab` and lend; a missing one, or one
    built without the test-root marker, ends the session."""
    located = locate_the_runtime_unit()
    if isinstance(located, str):
        pytest.exit(located, returncode=4)
    return located


@dataclass(frozen=True)
class PrivateMachineDirectories:
    """This test's machine root, the directories under it, and the environment naming it.

    `environment` is the suite's own minus the engine variables a runtime must
    not inherit, with `TATOLAB_TEST_MACHINE_ROOT` naming the root and
    `STREAMLIB_HOME` a directory under it. On macOS it names this root's own
    loopback HTTP port too, `loopback_http_port_on_macos`, whose bind is the
    machine runtime lock there. `XDG_RUNTIME_DIR` stays the user's, so their
    PipeWire, PulseAudio and Wayland sessions stay reachable.
    """

    machine_directories: MachineDirectoriesUnderATestRoot
    streamlib_home: Path
    environment: "dict[str, str]"
    loopback_http_port_on_macos: "int | None"


@pytest.fixture
def private_machine_directories() -> "Iterator[PrivateMachineDirectories]":
    """This test's machine root, made fresh and removed afterwards."""
    root = Path(tempfile.mkdtemp(prefix="tl-", dir=SHORT_TEMPORARY_DIRECTORY))
    root.chmod(0o700)
    machine_directories = MachineDirectoriesUnderATestRoot(root=root)
    loopback_http_port_on_macos = (
        a_tcp_port_free_on_loopback() if sys.platform == "darwin" else None
    )
    streamlib_home = root / "home"
    streamlib_home.mkdir()
    environment = {
        name: value
        for name, value in os.environ.items()
        if name not in ENGINE_VARIABLES_NOT_INHERITED
    }
    environment[TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE] = str(root)
    environment["STREAMLIB_HOME"] = str(streamlib_home)
    if loopback_http_port_on_macos is not None:
        environment[TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE] = str(loopback_http_port_on_macos)
    try:
        yield PrivateMachineDirectories(
            machine_directories=machine_directories,
            streamlib_home=streamlib_home,
            environment=environment,
            loopback_http_port_on_macos=loopback_http_port_on_macos,
        )
    finally:
        shutil.rmtree(root, ignore_errors=True)


def a_tcp_port_free_on_loopback() -> int:
    """A TCP port nothing holds on `127.0.0.1` now."""
    with socket.create_server(("127.0.0.1", 0)) as probe:
        return probe.getsockname()[1]


def environment_overlaid_with(
    environment: "dict[str, str]", extra_environment: "dict[str, str | None] | None"
) -> "dict[str, str]":
    """`environment` with `extra_environment` laid over it; a `None` value unsets that variable."""
    overlaid = {**environment, **(extra_environment or {})}
    return {name: value for name, value in overlaid.items() if value is not None}


def environment_reaching_no_vulkan_driver(directory: Path) -> "dict[str, str]":
    """Variables that leave the Vulkan loader no driver, so a stream's start is refused at the GPU."""
    no_such_driver_file = str(directory / "no-vulkan-driver-here.json")
    return {variable: no_such_driver_file for variable in VULKAN_LOADER_DRIVER_FILE_VARIABLES}


@dataclass
class StartedRuntimeProcesses:
    """Every process a test started, killed with its descendants at teardown."""

    started: "list[RuntimeProcessUnderTest]"

    def kill_every_one(self) -> None:
        for started_process in reversed(self.started):
            started_process.kill_every_process_it_started()


@pytest.fixture
def started_runtime_processes() -> "Iterator[StartedRuntimeProcesses]":
    """The list every started `tatolabd` and `tatolab` joins, reaped however the test ends.

    Without the reaping a failed assertion strands a live engine holding a GPU
    context, a device, an iceoryx2 node and a socket, contaminating every later
    run on the rig.
    """
    started_runtime_processes = StartedRuntimeProcesses(started=[])
    try:
        yield started_runtime_processes
    finally:
        started_runtime_processes.kill_every_one()


class TatolabRunOrDevOnTheRuntime(RuntimeProcessUnderTest):
    """A started `tatolab run` or `tatolab dev`. `run` prints the line naming the
    stream it loaded on standard output and exits — 0 once the load is accepted,
    1 with its refusal on standard error. `dev` writes that line as a note on
    standard error, follows the stream's records on standard output, and runs it
    again on every settled save; a Ctrl-C to it ends the following and leaves the
    stream loaded."""

    def __init__(
        self,
        process: "subprocess.Popen[str]",
        *,
        command_description: str,
        refusal_line_prefix: str,
        verb: str,
        working_directory: Path,
        tatolab_commands: "TatolabCommandsOfThisTest",
    ) -> None:
        super().__init__(
            process,
            command_description=command_description,
            refusal_line_prefix=refusal_line_prefix,
        )
        self.verb = verb
        self.working_directory = working_directory
        self._tatolab_commands = tatolab_commands

    def await_loaded(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS, occurrence: int = 1
    ) -> "re.Match[str]":
        """Wait for its `occurrence`-th line naming the stream a load started; the
        match's groups are `stream_id`, `display_name`, `project_directory` and
        `stream_function`. `run` names one load: it is waited to its exit, and a
        refused load fails the test with the refusal."""
        if self.verb != "dev":
            assert occurrence == 1, "`tatolab run` names the one stream it loaded"
            exit_status = self.await_exit(timeout=timeout)
            assert exit_status == 0, (
                f"`{self.command_description}` exited {exit_status}, its load refused: "
                f"{self.refusal()}\nstandard error:\n{self.recent_stderr()}"
            )
            running_lines = [
                running_line
                for running_line in (
                    RUNNING_STREAM_LINE_PATTERN.match(stdout_line.rstrip("\n"))
                    for stdout_line in self.stdout_lines
                )
                if running_line is not None
            ]
            assert running_lines, (
                f"`{self.command_description}` exited 0 naming no stream; standard output:\n"
                f"{self.stdout_text}"
            )
            return running_lines[0]
        running_note_count = 0

        def observe(line: str) -> Any:
            nonlocal running_note_count
            running_note = RUNNING_STREAM_LINE_PATTERN.match(line.rstrip("\n"))
            if running_note is None:
                return NOT_YET_SEEN
            running_note_count += 1
            return running_note if running_note_count == occurrence else NOT_YET_SEEN

        return self._await_stderr_line_satisfying(
            observe, f"its note that the stream is running (occurrence {occurrence})", timeout
        )

    def stop_the_stream(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> int:
        """Run `tatolab stop <id>` on the stream this load started, to its exit; return its
        exit status. The stream is unloaded and recorded stopped."""
        stream_id = self.await_loaded(timeout=timeout)["stream_id"]
        stop = self._tatolab_commands.start_tatolab(
            "stop", stream_id, working_directory=self.working_directory
        )
        exit_status = stop.await_exit(timeout=timeout)
        assert exit_status == 0, (
            f"`tatolab stop {stream_id}` exited {exit_status}; standard error:\n"
            f"{stop.recent_stderr()}"
        )
        return exit_status


@dataclass
class TatolabCommandsOfThisTest:
    """Starts the runtime unit's `tatolab` in this test's machine environment, and
    turns what a test hands a loading fixture into the `tatolab run` that loads it."""

    runtime_unit: RuntimeUnitUnderTest
    private_machine_directories: PrivateMachineDirectories
    started_runtime_processes: StartedRuntimeProcesses
    scratch_directory: Path
    hand_written_graph_project_count: int = field(default=0)

    def start_tatolab(
        self,
        *arguments: "str | Path",
        working_directory: Path,
        extra_environment: "dict[str, str | None] | None" = None,
        process_class: "Callable[..., RuntimeProcessUnderTest]" = RuntimeProcessUnderTest,
    ) -> Any:
        """`tatolab <arguments...>` started in a session of its own."""
        process = subprocess.Popen(
            [str(self.runtime_unit.tatolab_executable), *map(str, arguments)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            bufsize=1,
            start_new_session=True,
            cwd=working_directory,
            env=environment_overlaid_with(
                self.private_machine_directories.environment, extra_environment
            ),
        )
        started_tatolab = process_class(
            process,
            command_description=" ".join(["tatolab", *map(str, arguments)]),
            refusal_line_prefix=TATOLAB_REFUSAL_LINE_PREFIX,
        )
        self.started_runtime_processes.started.append(started_tatolab)
        return started_tatolab

    def tatolab_run_of(
        self,
        stream_or_graph: StreamOrGraph,
        *,
        display_name: "str | None" = None,
        project_directory: "Path | None" = None,
        processor_interpreter: "Path | None" = None,
    ) -> TatolabRunOfAProject:
        """The `tatolab run` that loads `stream_or_graph`; see the module docstring."""
        if isinstance(stream_or_graph, TatolabRunOfAProject):
            assert display_name is None and project_directory is None and processor_interpreter is None, (
                "a project's own `tatolab run` names its stream and project in its arguments"
            )
            return stream_or_graph
        if isinstance(stream_or_graph, (dict, str)):
            assert display_name is None, (
                "a hand-written graph names its stream in its `stream` key"
            )
            self.hand_written_graph_project_count += 1
            return write_a_hand_written_graph_project(
                self.scratch_directory
                / f"hand-written-graph-project-{self.hand_written_graph_project_count}",
                stream_or_graph,
                reported_project_directory=project_directory,
                processor_interpreter=processor_interpreter,
            ).tatolab_run
        assert project_directory is None and processor_interpreter is None, (
            "a `@stream` function of the suite runs in the suite project; run another "
            "project's stream with `TatolabRunOfAProject`"
        )
        return tatolab_run_of_a_suite_stream_function(stream_or_graph, display_name)


class TatolabdUnderTest(RuntimeProcessUnderTest):
    """This test's started `tatolabd`, its local API at `<root>/run/local-api.sock`,
    and the streams run on it with `tatolab run`."""

    def __init__(
        self,
        process: "subprocess.Popen[str]",
        *,
        tatolab_commands: TatolabCommandsOfThisTest,
    ) -> None:
        super().__init__(
            process, command_description="tatolabd", refusal_line_prefix=TATOLABD_REFUSAL_LINE_PREFIX
        )
        self._tatolab_commands = tatolab_commands
        self.machine_directories = tatolab_commands.private_machine_directories.machine_directories
        #: Each `tatolab run` or `dev` started on this `tatolabd`, in order.
        self.stream_runs: "list[TatolabRunOrDevOnTheRuntime]" = []

    @property
    def local_api_socket_path(self) -> Path:
        """`<root>/run/local-api.sock`, the one socket the runtime serves its local API on."""
        return self.machine_directories.local_api_socket_path

    def await_serving(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> None:
        """Wait until the local API answers `/health`; a `tatolabd` that exits first fails the test."""
        deadline = time.monotonic() + timeout
        local_api = LocalApiClient(self.local_api_socket_path)
        while True:
            if self.process.poll() is not None:
                raise AssertionError(
                    f"tatolabd exited {self.process.returncode} before its local API answered at "
                    f"{self.local_api_socket_path}; standard error:\n{self.recent_stderr()}"
                )
            if self.local_api_socket_path.exists():
                try:
                    if local_api.health() == "ok":
                        return
                except OSError:
                    pass
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"tatolabd's local API did not answer at {self.local_api_socket_path} within "
                    f"{timeout}s; standard error:\n{self.recent_stderr()}"
                )
            time.sleep(LOCAL_API_SOCKET_POLL_INTERVAL_SECONDS)

    def local_api_client(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> LocalApiClient:
        """A client of this runtime's local API, once it answers."""
        self.await_serving(timeout=timeout)
        return LocalApiClient(self.local_api_socket_path)

    def await_the_latest_stream_loaded(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS
    ) -> str:
        """Wait for the latest `tatolab run`'s line naming the stream it loaded;
        return the stream's id."""
        assert self.stream_runs, "no `tatolab run` was started on this tatolabd"
        return self.stream_runs[-1].await_loaded(timeout=timeout)["stream_id"]

    def run_stream(
        self,
        stream_or_graph: StreamOrGraph,
        *,
        display_name: "str | None" = None,
        project_directory: "Path | None" = None,
        processor_interpreter: "Path | None" = None,
        extra_environment: "dict[str, str | None] | None" = None,
        verb: str = "run",
    ) -> TatolabRunOrDevOnTheRuntime:
        """Start `tatolab run` (or `verb`, such as `dev`) on `stream_or_graph`; return it started.

        `extra_environment` reaches the `tatolab` process alone: a stream's
        processor interpreters inherit `tatolabd`'s environment, never the CLI's.
        """
        tatolab_run = self._tatolab_commands.tatolab_run_of(
            stream_or_graph,
            display_name=display_name,
            project_directory=project_directory,
            processor_interpreter=processor_interpreter,
        )
        stream_run = self._tatolab_commands.start_tatolab(
            verb,
            *tatolab_run.tatolab_run_arguments,
            working_directory=tatolab_run.working_directory,
            extra_environment=extra_environment,
            process_class=functools.partial(
                TatolabRunOrDevOnTheRuntime,
                verb=verb,
                working_directory=tatolab_run.working_directory,
                tatolab_commands=self._tatolab_commands,
            ),
        )
        self.stream_runs.append(stream_run)
        return stream_run


@pytest.fixture
def tatolab_commands_of_this_test(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    started_runtime_processes: StartedRuntimeProcesses,
    tmp_path: Path,
) -> TatolabCommandsOfThisTest:
    """What `start_tatolab` and every `TatolabdUnderTest` start `tatolab` through."""
    return TatolabCommandsOfThisTest(
        runtime_unit=runtime_unit,
        private_machine_directories=private_machine_directories,
        started_runtime_processes=started_runtime_processes,
        scratch_directory=tmp_path,
    )


@pytest.fixture
def start_tatolabd(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    started_runtime_processes: StartedRuntimeProcesses,
    tatolab_commands_of_this_test: TatolabCommandsOfThisTest,
    tmp_path: Path,
) -> "Callable[..., TatolabdUnderTest]":
    """Start `tatolabd` under this test's machine root, in a session of its own.

    `start_tatolabd(*, extra_environment=None, working_directory=None,
    wait_until_serving=True) -> TatolabdUnderTest`:
    - `extra_environment` is laid over the test's environment, a `None` value
      unsetting that variable; every stream's processor interpreters inherit it;
    - `working_directory` defaults to a fresh directory of the test's own;
    - with `wait_until_serving`, returns once the local API answers, and a
      `tatolabd` that exits first fails the test. A second `tatolabd` while one
      runs is refused by the machine lock — pass `wait_until_serving=False` to
      observe that refusal.
    """
    start_count = 0

    def start(
        *,
        extra_environment: "dict[str, str | None] | None" = None,
        working_directory: "Path | None" = None,
        wait_until_serving: bool = True,
    ) -> TatolabdUnderTest:
        nonlocal start_count
        start_count += 1
        if working_directory is None:
            working_directory = tmp_path / f"tatolabd-working-directory-{start_count}"
            working_directory.mkdir()
        process = subprocess.Popen(
            [str(runtime_unit.tatolabd_executable)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            bufsize=1,
            start_new_session=True,
            cwd=working_directory,
            env=environment_overlaid_with(private_machine_directories.environment, extra_environment),
        )
        started_tatolabd = TatolabdUnderTest(process, tatolab_commands=tatolab_commands_of_this_test)
        started_runtime_processes.started.append(started_tatolabd)
        if wait_until_serving:
            started_tatolabd.await_serving()
        return started_tatolabd

    return start


@pytest.fixture
def start_tatolabd_running_stream(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
) -> "Callable[..., TatolabdUnderTest]":
    """Start this test's `tatolabd`, then run `stream_or_graph` on it with `tatolab run`.

    `start_tatolabd_running_stream(stream_or_graph, *, display_name=None,
    project_directory=None, processor_interpreter=None, extra_environment=None,
    working_directory=None) -> TatolabdUnderTest`. `extra_environment` and
    `working_directory` are `tatolabd`'s, so the stream's processor interpreters
    inherit the environment. Returns as soon as `tatolab run` is started, its
    load not awaited: the run is `tatolabd.stream_runs[-1]`, and
    `await_loaded()` on it waits for the load, while a refused load ends that
    run with its refusal and leaves `tatolabd` serving.
    """

    def start(
        stream_or_graph: StreamOrGraph,
        *,
        display_name: "str | None" = None,
        project_directory: "Path | None" = None,
        processor_interpreter: "Path | None" = None,
        extra_environment: "dict[str, str | None] | None" = None,
        working_directory: "Path | None" = None,
    ) -> TatolabdUnderTest:
        started_tatolabd = start_tatolabd(
            extra_environment=extra_environment, working_directory=working_directory
        )
        started_tatolabd.run_stream(
            stream_or_graph,
            display_name=display_name,
            project_directory=project_directory,
            processor_interpreter=processor_interpreter,
        )
        return started_tatolabd

    return start


@dataclass(frozen=True)
class StreamRunWithNoVulkanDriverOutcome:
    """How one `tatolab run` on a `tatolabd` that reaches no Vulkan driver ended."""

    #: Whether the stream's graph loaded — its start then refused at the GPU.
    loaded: bool
    #: The stream id the engine loaded it under, once it loaded.
    loaded_stream_id: "str | None"
    #: How many processors the load added, as its unload removed them.
    loaded_node_count: "int | None"
    #: `tatolab run`'s refusal, from its `error: <reason>` line on.
    refusal: "str | None"
    tatolab_run_exit_status: int
    tatolab_run_stderr_text: str
    #: `tatolabd`'s standard error from the moment `tatolab run` started.
    tatolabd_stderr_text_during_the_run: str
    #: The runtime it ran on, still serving.
    tatolabd: TatolabdUnderTest
    #: The `tatolab run` that loaded it: for a hand-written graph, its project's
    #: `working_directory/.venv/bin/python` is the wrapper the runtime started.
    tatolab_run: TatolabRunOfAProject


@pytest.fixture
def run_stream_on_tatolabd_with_no_vulkan_driver(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    tatolab_commands_of_this_test: TatolabCommandsOfThisTest,
    tmp_path: Path,
) -> "Callable[..., StreamRunWithNoVulkanDriverOutcome]":
    """Run a stream on a `tatolabd` that reaches no Vulkan driver, to the end of the run.

    `run_stream_on_tatolabd_with_no_vulkan_driver(stream_or_graph, *,
    display_name=None, project_directory=None, processor_interpreter=None,
    extra_environment=None) -> StreamRunWithNoVulkanDriverOutcome`. Needs no
    device: a graph the load refuses ends `tatolab run` with that refusal; one
    it loads is refused at its start, at the GPU, before any device opens. The
    runtime outlives either: `tatolabd` still serves, holding no stream and no
    record — anything else fails the test, as does a loaded stream whose run
    ended any other way, so a driver override that did not take cannot start a
    stream's devices unseen.

    One `tatolabd` serves every call in a test; a call with a different
    `extra_environment` (`tatolabd`'s) interrupts it and starts another.
    """
    tatolabd_and_its_extra_environment: "list[tuple[TatolabdUnderTest, dict[str, str | None]]]" = []

    def the_tatolabd_with_no_vulkan_driver(
        extra_environment: "dict[str, str | None]",
    ) -> TatolabdUnderTest:
        if tatolabd_and_its_extra_environment:
            running_tatolabd, its_extra_environment = tatolabd_and_its_extra_environment[-1]
            if its_extra_environment == extra_environment and running_tatolabd.process.poll() is None:
                return running_tatolabd
            running_tatolabd.interrupt()
            running_tatolabd.await_exit()
        started_tatolabd = start_tatolabd(extra_environment=extra_environment)
        tatolabd_and_its_extra_environment.append((started_tatolabd, extra_environment))
        return started_tatolabd

    def run(
        stream_or_graph: StreamOrGraph,
        *,
        display_name: "str | None" = None,
        project_directory: "Path | None" = None,
        processor_interpreter: "Path | None" = None,
        extra_environment: "dict[str, str | None] | None" = None,
    ) -> StreamRunWithNoVulkanDriverOutcome:
        tatolabd = the_tatolabd_with_no_vulkan_driver(
            {**environment_reaching_no_vulkan_driver(tmp_path), **(extra_environment or {})}
        )
        tatolab_run = tatolab_commands_of_this_test.tatolab_run_of(
            stream_or_graph,
            display_name=display_name,
            project_directory=project_directory,
            processor_interpreter=processor_interpreter,
        )
        tatolabd_stderr_mark = tatolabd.stderr_line_count()
        stream_run = tatolabd.run_stream(tatolab_run)
        exit_status = stream_run.await_exit(timeout=TATOLAB_RUN_TO_COMPLETION_TIMEOUT_SECONDS)
        refusal = stream_run.refusal()
        listed_streams = tatolabd.local_api_client().list_streams()
        tatolabd_stderr_text = tatolabd.stderr_text_since(tatolabd_stderr_mark)
        start_began = STREAM_START_BEGAN_LOG_LINE_PATTERN.search(tatolabd_stderr_text)
        unload = STREAM_UNLOAD_REMOVED_PROCESSORS_LOG_LINE_PATTERN.search(tatolabd_stderr_text)
        assert listed_streams == [], (
            f"`{stream_run.command_description}` ended {exit_status} and left the runtime "
            f"holding {listed_streams}; standard error:\n{stream_run.recent_stderr()}"
        )
        if start_began is not None:
            assert ENGINE_STARTED_LOG_LINE not in tatolabd_stderr_text, (
                f"the engine started a stream that was to end at the GPU:\n{tatolabd.recent_stderr()}"
            )
            assert exit_status == 1 and refusal is not None and NO_VULKAN_DRIVER_REFUSAL in refusal, (
                f"a loaded stream was to be refused for the missing Vulkan driver; "
                f"`{stream_run.command_description}` ended {exit_status}:\n"
                f"{stream_run.recent_stderr()}\ntatolabd:\n{tatolabd.recent_stderr()}"
            )
        return StreamRunWithNoVulkanDriverOutcome(
            loaded=start_began is not None,
            loaded_stream_id=start_began.group("stream_id") if start_began else None,
            loaded_node_count=(
                int(unload.group("processor_count")) if start_began and unload else None
            ),
            refusal=refusal,
            tatolab_run_exit_status=exit_status,
            tatolab_run_stderr_text=stream_run.stderr_text,
            tatolabd_stderr_text_during_the_run=tatolabd_stderr_text,
            tatolabd=tatolabd,
            tatolab_run=tatolab_run,
        )

    return run


@pytest.fixture
def make_tatolab_project(tmp_path: Path) -> "Callable[..., Path]":
    """Make a `tatolab` project directory whose `.venv` symlinks the suite venv.

    `make_tatolab_project(files={relative_path: text}, directory_name="app") -> Path`:
    each file is written under the project, and `.venv` points at the suite
    venv's root, so the runtime compiles the project's stream in
    `<project>/.venv/bin/python`, holding `tatolab-stream` and nothing of the runtime.
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
    tatolab_commands_of_this_test: TatolabCommandsOfThisTest,
) -> "Callable[..., RuntimeProcessUnderTest]":
    """Start `tatolab <arguments...>` in a session of its own; returns its `RuntimeProcessUnderTest`.

    `start_tatolab(*arguments, working_directory, extra_environment=None)`, in
    this test's machine environment: every verb but `new` reaches the runtime
    this test started with `start_tatolabd`, or refuses naming the socket
    nothing answers at.
    """

    def start(
        *arguments: "str | Path",
        working_directory: Path,
        extra_environment: "dict[str, str | None] | None" = None,
    ) -> RuntimeProcessUnderTest:
        return tatolab_commands_of_this_test.start_tatolab(
            *arguments, working_directory=working_directory, extra_environment=extra_environment
        )

    return start


@pytest.fixture
def run_tatolab(
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
) -> "Callable[..., subprocess.CompletedProcess[str]]":
    """Run `tatolab <arguments...>` to completion; returns the completed process, output captured.

    `run_tatolab(*arguments, working_directory, extra_environment=None, timeout=120.0)`.
    Started through `start_tatolab`, so a run that outlives `timeout` fails the
    test and is killed with it.
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
def run_tatolab_observation_verb(
    runtime_unit: RuntimeUnitUnderTest, private_machine_directories: PrivateMachineDirectories
) -> "Callable[..., subprocess.CompletedProcess[str]]":
    """Run a `tatolab` verb (`graph`, `streams`, `logs`, ...) against this test's runtime.

    `run_tatolab_observation_verb(*verb_arguments, timeout=60.0)`: the runtime
    unit's `bin/tatolab`, in this test's machine environment, to completion.
    """

    def run(*verb_arguments: str, **keyword_arguments: Any) -> "subprocess.CompletedProcess[str]":
        return run_tatolab_verb_in_environment(
            runtime_unit,
            private_machine_directories.environment,
            *verb_arguments,
            **keyword_arguments,
        )

    return run


@pytest.fixture
def held_node_module(
    request: pytest.FixtureRequest, tmp_path: Path
) -> "Iterator[NodeModuleWhoseDescribeHoldsTheLoad]":
    """A node module in a project directory of its own, whose describe parks the load.

    Name `<held_node_module.name>:LoadedFrameRelay` in a hand-written graph run
    with `project_directory=held_node_module.project_directory`, then
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
