# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


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
from runtime_process_under_test import (
    DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS,
    NOT_YET_SEEN,
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
)
from stream_runs_on_tatolabd import (
    StreamOrGraph,
    TatolabRunOfAProject,
    write_a_hand_written_graph_project,
    write_a_one_stream_project_of_a_suite_stream_function,
)

pytest_plugins = ["runtime_launching_fixtures", "project_scaffolding_fixtures"]

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

STREAM_UNLOAD_REMOVED_NODES_LOG_LINE_PATTERN = re.compile(
    r"\[stop\] Queued removal of (?P<node_count>\d+) node\(s\)"
)

#: The line `tatolab open`, `play`, `reload`, `stop` and `rm` print for the stream action they
#: queued, and `tatolab dev`'s note on standard error for each action it queues, prefixed
#: `tatolab dev: `.
QUEUED_STREAM_ACTION_LINE_PATTERN = re.compile(
    r"^(?:tatolab dev: )?(?P<action>\S+) queued: (?P<kind>\S+) of ", re.MULTILINE
)

#: The verb `run_stream` starts a stream with: the open of its project, its playback queued behind.
OPEN_AND_PLAY_VERB = "open"

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
    lent_runtime_spec = importlib.util.find_spec("tatolab.runtime")
    if lent_runtime_spec is not None:
        pytest.exit(
            f"the suite venv at {SUITE_VENV_PREFIX} imports `tatolab.runtime` from "
            f"{lent_runtime_spec.origin or lent_runtime_spec.submodule_search_locations}: the "
            f"integration suite's venv holds `tatolab-stream` only, and node interpreters "
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


class TatolabRunOrDevOnTheRuntime(RuntimeProcessUnderTest):
    """A started `tatolab open --play` or `tatolab dev`. `open --play` prints `<action> queued:
    open of <project>`, then `<action> queued: start_playback of <project>`, and exits 0 once
    both are queued, and 1 with its refusal on standard error. `dev` writes those lines as notes
    on standard error — its open, its playback, then a load per settled save — and follows the
    project's log on standard output; a Ctrl-C to it ends the following and leaves the streams
    loaded. The playback is the load a test follows: what it did is its action's, read with
    `describe_stream_action`, and its streams' listing; a stream the open failed is read from the
    open's action."""

    def __init__(
        self,
        process: "subprocess.Popen[str]",
        *,
        command_description: str,
        refusal_line_prefix: str,
        verb: str,
        working_directory: Path,
        project_dir_argument: str,
        tatolab_commands: "TatolabCommandsOfThisTest",
    ) -> None:
        super().__init__(
            process,
            command_description=command_description,
            refusal_line_prefix=refusal_line_prefix,
        )
        self.verb = verb
        self.working_directory = working_directory
        self.project_dir_argument = project_dir_argument
        self._tatolab_commands = tatolab_commands
        self._ended_actions: "dict[str, dict[str, Any]]" = {}

    def _local_api(self) -> LocalApiClient:
        return LocalApiClient(
            self._tatolab_commands.private_machine_directories.machine_directories.local_api_socket_path
        )

    def _actions_open_and_play_queued(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS
    ) -> "list[re.Match[str]]":
        """Each action `open --play` queued, the open's first, once it exited 0; a refused one
        fails the test with the refusal."""
        exit_status = self.await_exit(timeout=timeout)
        assert exit_status == 0, (
            f"`{self.command_description}` exited {exit_status}, its load refused: "
            f"{self.refusal()}\nstandard error:\n{self.recent_stderr()}"
        )
        queued_actions = list(QUEUED_STREAM_ACTION_LINE_PATTERN.finditer(self.stdout_text))
        assert queued_actions, (
            f"`{self.command_description}` exited 0 naming no action; standard output:\n"
            f"{self.stdout_text}"
        )
        return queued_actions

    def queued_action(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS, occurrence: int = 1
    ) -> str:
        """The id of the `occurrence`-th load this verb queued, the playback first. `open --play`
        queues one: it is waited to its exit, and a refused load fails the test with the
        refusal."""
        if self.verb != "dev":
            assert occurrence == 1, "`tatolab open --play` queues one playback"
            return self._actions_open_and_play_queued(timeout=timeout)[-1].group("action")
        queued_note_count = 0

        def observe(line: str) -> Any:
            nonlocal queued_note_count
            queued_note = QUEUED_STREAM_ACTION_LINE_PATTERN.match(line.rstrip("\n"))
            if queued_note is None or queued_note.group("kind") == "open":
                return NOT_YET_SEEN
            queued_note_count += 1
            return queued_note.group("action") if queued_note_count == occurrence else NOT_YET_SEEN

        return self._await_stderr_line_satisfying(
            observe, f"its note that a load was queued (occurrence {occurrence})", timeout
        )

    def action_once_ended(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS, occurrence: int = 1
    ) -> "dict[str, Any]":
        """`describe_stream_action` of the `occurrence`-th load this verb queued, once it ended
        `done` or `failed`."""
        action = self.queued_action(timeout=timeout, occurrence=occurrence)
        if action not in self._ended_actions:
            self._ended_actions[action] = self._local_api().stream_action_once_ended(
                action, timeout=timeout
            )
        return self._ended_actions[action]

    def await_loaded(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS, occurrence: int = 1
    ) -> "dict[str, str]":
        """Follow the `occurrence`-th load this verb queued to its end, and answer the one stream
        it took in, running — its `stream_id`, `display_name`, `project_directory`, `project`
        and `stream_asset`; a load that failed, or took in a failed stream, fails the test with
        why."""
        ended_action = self.action_once_ended(timeout=timeout, occurrence=occurrence)
        assert ended_action["state"] == "done", (
            f"the load {ended_action['action']} failed: {ended_action['failed_because']}"
        )
        assert len(ended_action["streams"]) == 1, (
            f"the load {ended_action['action']} took in other than one stream: {ended_action}"
        )
        [loaded_stream] = ended_action["streams"]
        stream_id = loaded_stream["stream"]
        listed_streams = self._local_api().list_streams()
        listed_stream = next(
            (listed for listed in listed_streams if listed["stream"] == stream_id), None
        )
        assert listed_stream is not None, (
            f"the load {ended_action['action']} took in {stream_id}, which the runtime does not "
            f"list: {listed_streams}"
        )
        assert loaded_stream["state"] == "running", (
            f"the load {ended_action['action']} recorded {stream_id} failed: "
            f"{listed_stream['failed_because']}"
        )
        return {
            "stream_id": stream_id,
            "display_name": listed_stream["display_name"],
            "project_directory": listed_stream["location"]["project_directory"],
            "project": listed_stream["location"]["project"],
            "stream_asset": listed_stream["location"]["stream_asset"],
        }

    def actions_once_ended(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS, occurrence: int = 1
    ) -> "list[dict[str, Any]]":
        """`describe_stream_action` of the `occurrence`-th load — or, for `open --play`, of its
        open and its playback, the open first — once each ended."""
        if self.verb == "dev":
            return [self.action_once_ended(timeout=timeout, occurrence=occurrence)]
        return [
            self._local_api().stream_action_once_ended(queued.group("action"), timeout=timeout)
            for queued in self._actions_open_and_play_queued(timeout=timeout)
        ]

    def failed_stream(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS, occurrence: int = 1
    ) -> "tuple[str, str] | None":
        """The stream the `occurrence`-th load — or, for `open --play`, its open or its
        playback — recorded failed and why, as the runtime lists it; `None` when it took in
        none failed."""
        ended_actions = self.actions_once_ended(timeout=timeout, occurrence=occurrence)
        failed_stream_ids = [
            stream["stream"]
            for ended_action in ended_actions
            for stream in ended_action["streams"]
            if stream["state"] == "failed"
        ]
        if not failed_stream_ids:
            return None
        listed_failed_because = {
            listed["stream"]: listed["failed_because"] for listed in self._local_api().list_streams()
        }
        return failed_stream_ids[0], listed_failed_because.get(failed_stream_ids[0]) or ""

    def failed_because(self) -> "str | None":
        """Why the stream the load recorded failed is failed; `None` when none failed."""
        failed_stream = self.failed_stream()
        return failed_stream[1] if failed_stream is not None else None

    def stop_the_stream(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> int:
        """Run `tatolab stop --dir <dir>` on the project this load ran, once its stream is
        loaded, to its exit, and follow the stop it queued to its end; return its exit status.
        Every stream of the project is unloaded and recorded stopped."""
        self.await_loaded(timeout=timeout)
        stop = self._tatolab_commands.start_tatolab(
            "stop", "--dir", self.project_dir_argument, working_directory=self.working_directory
        )
        exit_status = stop.await_exit(timeout=timeout)
        assert exit_status == 0, (
            f"`tatolab stop --dir {self.project_dir_argument}` exited {exit_status}; "
            f"standard error:\n"
            f"{stop.recent_stderr()}"
        )
        queued_stop = QUEUED_STREAM_ACTION_LINE_PATTERN.search(stop.stdout_text)
        assert queued_stop is not None, f"`tatolab stop` printed no action: {stop.stdout_text}"
        stop_once_ended = self._local_api().stream_action_once_ended(
            queued_stop.group("action"), timeout=timeout
        )
        assert stop_once_ended["state"] == "done", stop_once_ended
        return exit_status


@dataclass
class TatolabCommandsOfThisTest:
    """Starts the runtime unit's `tatolab` in this test's machine environment, and
    turns what a test hands a loading fixture into the project `tatolab open --play` loads."""

    runtime_unit: RuntimeUnitUnderTest
    private_machine_directories: PrivateMachineDirectories
    started_runtime_processes: StartedRuntimeProcesses
    scratch_directory: Path
    written_project_count: int = field(default=0)

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
        project_directory: "Path | None" = None,
        node_interpreter: "Path | None" = None,
    ) -> TatolabRunOfAProject:
        """The `tatolab open --play` that loads `stream_or_graph`; see the module docstring."""
        if isinstance(stream_or_graph, TatolabRunOfAProject):
            assert project_directory is None and node_interpreter is None, (
                "a project run as it is names its own project"
            )
            return stream_or_graph
        self.written_project_count += 1
        if isinstance(stream_or_graph, (dict, str)):
            return write_a_hand_written_graph_project(
                self.scratch_directory / f"hand-written-graph-project-{self.written_project_count}",
                stream_or_graph,
                reported_project_directory=project_directory,
                node_interpreter=node_interpreter,
            ).tatolab_run
        assert project_directory is None and node_interpreter is None, (
            "a `@graph_generator` function of the suite runs in a one-stream project of its own; run "
            "another project's stream with `TatolabRunOfAProject`"
        )
        return write_a_one_stream_project_of_a_suite_stream_function(
            self.scratch_directory / f"suite-stream-project-{self.written_project_count}",
            stream_or_graph,
        )


class TatolabdUnderTest(RuntimeProcessUnderTest):
    """This test's started `tatolabd`, its local API at `<root>/run/local-api.sock`,
    and the streams run on it with `tatolab open --play`."""

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
        #: Each `tatolab open --play` or `dev` started on this `tatolabd`, in order.
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
        """Follow the latest `tatolab open --play`'s load to its end; return the id of the stream it
        loaded."""
        assert self.stream_runs, "no `tatolab open --play` was started on this tatolabd"
        return self.stream_runs[-1].await_loaded(timeout=timeout)["stream_id"]

    def run_stream(
        self,
        stream_or_graph: StreamOrGraph,
        *,
        project_directory: "Path | None" = None,
        node_interpreter: "Path | None" = None,
        extra_environment: "dict[str, str | None] | None" = None,
        verb: str = OPEN_AND_PLAY_VERB,
    ) -> TatolabRunOrDevOnTheRuntime:
        tatolab_run = self._tatolab_commands.tatolab_run_of(
            stream_or_graph,
            project_directory=project_directory,
            node_interpreter=node_interpreter,
        )
        stream_run = self._tatolab_commands.start_tatolab(
            *([verb, "--play"] if verb == OPEN_AND_PLAY_VERB else [verb]),
            "--dir",
            tatolab_run.dir_argument,
            working_directory=tatolab_run.working_directory,
            extra_environment=extra_environment,
            process_class=functools.partial(
                TatolabRunOrDevOnTheRuntime,
                verb=verb,
                working_directory=tatolab_run.working_directory,
                project_dir_argument=tatolab_run.dir_argument,
                tatolab_commands=self._tatolab_commands,
            ),
        )
        self.stream_runs.append(stream_run)
        return stream_run


@dataclass(frozen=True)
class StreamRunWithNoVulkanDriverOutcome:
    """How one `tatolab open --play` on a `tatolabd` that reaches no Vulkan driver ended."""

    #: Whether the stream's graph loaded — its start then refused at the GPU.
    loaded: bool
    #: The stream id the engine loaded it under, once it loaded.
    loaded_stream_id: "str | None"
    loaded_node_count: "int | None"
    #: Whether the load ended without a running stream: refused, failed, or a stream it took in
    #: recorded failed.
    failed_or_refused: bool
    #: Why: the failed stream's `failed_because` as the runtime listed it, the failed load's
    #: `failed_because`, or `tatolab open --play`'s refusal, from its `error: <reason>` line on.
    failed_or_refused_because: "str | None"
    tatolab_run_stderr_text: str
    #: `tatolabd`'s standard error from the moment `tatolab open --play` started.
    tatolabd_stderr_text_during_the_run: str
    #: The runtime it ran on, still serving.
    tatolabd: TatolabdUnderTest
    #: The `tatolab open --play` that loaded it: for a hand-written graph, its project's
    #: `working_directory/.venv/bin/python` is the wrapper the runtime started.
    tatolab_run: TatolabRunOfAProject
    #: The graph of each stream the run took in, by stream id, as the runtime served it once
    #: the run ended, before the fixture removed its project; a stream holding no graph, as one
    #: recorded failed before its graph loaded, is left out.
    graphs_of_the_streams_it_took_in: "dict[str, dict[str, Any]]"
