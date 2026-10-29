# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""#2605's done-means, end to end: `tatolabd` hosts the machine's streams, and
every verb that manages them reaches it through the real CLI.

Each test drives the runtime unit's own `tatolabd` and `tatolab` under the
test's machine root, as a user at a terminal does: `tatolab run` from two
project directories, `tatolabd` restarted with a Ctrl-C and again after a
SIGKILL, `stop`, `start`, `rm`, `streams` and `expose` one call at a time, a
`tatolab dev` ended by a Ctrl-C and by a SIGKILL leaving its stream running,
and an agent doing the same over `tatolab mcp` with a JSON-RPC client speaking
over the verb's stdin and stdout.

A started stream needs a GPU context, so most tests here are `requires_gpu`;
their streams are a test pattern into a Python node that reads its frames, so
no camera and no display is needed. What needs no started stream runs without one: every verb
with no runtime, and a stopped stream through restarts, the owner's
exposure ruling and `rm`.
"""

from __future__ import annotations

import json
import os
import queue
import signal
import stat
import subprocess
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import pytest
from mcp_types import (
    CLIENT_CAPABILITIES_META_KEY,
    CLIENT_INFO_META_KEY,
    PROTOCOL_VERSION_META_KEY,
)
from mcp_types.version import LATEST_PROTOCOL_VERSION

from conftest import PrivateMachineDirectories, TatolabdUnderTest, TatolabRunOrDevOnTheRuntime
from local_api_client import LocalApiClient, LocalApiRefusedTheRequest
from runtime_process_under_test import ENGINE_GRACEFUL_STOP_LOG_LINE
from runtime_unit_under_test import MachineDirectoriesUnderATestRoot, RuntimeUnitUnderTest
from stream_runs_on_tatolabd import (
    COMPILE_ENTRY_INVOCATION,
    TatolabRunOfAProject,
    stream_id_of_the_location,
)

#: A load is the project's compile, and on the runtime's first start its GPU context.
STREAM_RUNNING_TIMEOUT_SECONDS = 90.0
TATOLABD_EXIT_TIMEOUT_SECONDS = 60.0
STREAM_LISTING_POLL_INTERVAL_SECONDS = 0.05
#: How often the first-rendering watch reads the machine-wide graph during a re-load.
FIRST_RENDERING_POLL_INTERVAL_SECONDS = 0.005
MCP_RESPONSE_TIMEOUT_SECONDS = 120.0
MCP_VERB_EXIT_TIMEOUT_SECONDS = 15.0
#: What every request of the agent's session carries in its `_meta`.
MCP_REQUEST_META = {
    PROTOCOL_VERSION_META_KEY: LATEST_PROTOCOL_VERSION,
    CLIENT_CAPABILITIES_META_KEY: {},
    CLIENT_INFO_META_KEY: {"name": "done-means-agent", "version": "0"},
}

#: The stream functions of the done-means's two projects, each the sole `@stream`
#: of its own `stream.py` and the default display name of its stream.
ALPHA_STREAM_FUNCTION = "alpha_pattern"
BRAVO_STREAM_FUNCTION = "bravo_pattern"

#: Every project stream here: a test pattern into a node that reads its frames,
#: its `video` exposed public by the stream function — the author's suggestion,
#: which an owner's ruling overrides.
PROJECT_STREAM_SOURCE_TEMPLATE = '''\
from frame_reader import PatternFrameReader
from tatolab.stream import Exposure, StreamBuilder, TestPatternSource, stream


@stream
def {stream_function_name}(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, name="pattern", config={{"width": 320, "height": 180}}
    )
    reader = stream_builder.add(PatternFrameReader, name="reader")
    stream_builder.connect(pattern.output("video"), reader.input("video_from_upstream"))
    stream_builder.expose(pattern.output("video"), Exposure.PUBLIC)
'''
#: The project's reader, in its own module: a processor's only legal home.
PROJECT_FRAME_READER_SOURCE = '''\
from tatolab.stream import RuntimeContextLimitedAccess, node


@node
class PatternFrameReader:
    """Reads each frame it receives and keeps none."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video_from_upstream")
'''
PROJECT_STREAM_NODE_COUNT = 2
EXPOSED_NODE = "pattern"
EXPOSED_PORT = "video"


def exposure_at(level: str) -> "list[dict[str, str]]":
    """A graph's `exposed` holding the pattern's `video` at `level`; `internal` has no entry."""
    if level == "internal":
        return []
    return [{"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": level}]


#: How every verb refuses when nothing answers at the socket, before it names the socket.
NO_RUNTIME_REFUSAL_PREFIX = "no runtime is running on this machine: nothing answers at "
NO_RUNTIME_REFUSAL_REMEDY = "Start one by running `tatolabd` in a terminal."

STREAM_RECORD_FILE_MODE = 0o600
STREAM_RECORD_SCHEMA_VERSION = 3


def runtime_serving_log_line(reloaded_count: int, skipped_count: int) -> str:
    """`tatolabd`'s line once its start's re-loads are over."""
    return (
        f"the runtime is serving: {reloaded_count} recorded streams re-loaded, "
        f"{skipped_count} skipped"
    )


def stream_stopped_log_line(stream_id: str) -> str:
    """The engine's line once a stream's graceful stop has finished."""
    return f"{ENGINE_GRACEFUL_STOP_LOG_LINE}{stream_id}` stopped"


def make_project_running_a_pattern_stream(
    make_tatolab_project: "Callable[..., Path]", stream_function_name: str
) -> Path:
    """A project directory of its own whose `stream.py` holds the pattern stream, resolved."""
    return make_tatolab_project(
        {
            "stream.py": PROJECT_STREAM_SOURCE_TEMPLATE.format(
                stream_function_name=stream_function_name
            ),
            "frame_reader.py": PROJECT_FRAME_READER_SOURCE
        },
        directory_name=stream_function_name.replace("_", "-"),
    ).resolve()


def stream_function_of_the_pattern_project(stream_function_name: str) -> str:
    """The pattern stream's function as its location names it: `stream:<function>`."""
    return f"stream:{stream_function_name}"


def stream_id_of_the_pattern_project(project_directory: Path, stream_function_name: str) -> str:
    """The stream id the runtime keys and addresses the project's pattern stream by."""
    return stream_id_of_the_location(
        project_directory, stream_function_of_the_pattern_project(stream_function_name)
    )


def listed_streams_by_id(local_api: LocalApiClient) -> "dict[str, dict[str, Any]]":
    """`list_streams`, keyed by stream id."""
    return {listed["stream"]: listed for listed in local_api.list_streams()}


def listing_of(
    stream_id: str,
    stream_function_name: str,
    state: str,
    project_directory: Path,
    node_count: "int | None",
) -> "dict[str, Any]":
    """One `list_streams` entry of a pattern stream that is not failed, shown under its
    function's name."""
    return {
        "stream": stream_id,
        "display_name": stream_function_name,
        "location": {
            "project_directory": str(project_directory),
            "stream_function": stream_function_of_the_pattern_project(stream_function_name),
        },
        "state": state,
        "node_count": node_count,
        "failed_because": None,
    }


def assert_the_pattern_is_wired_to_the_reader_inside_the_stream(
    local_api: LocalApiClient, stream_id: str
) -> None:
    """The stream's one link, the pattern into the reader, reaches `wired`: proof it runs,
    read from inside the stream, never by reading a port across its edge."""
    stream_links = local_api.graph(stream_id)["links"]
    assert len(stream_links) == 1, stream_links
    link_state = local_api.await_link_state(stream_links[0]["id"], "wired", stream=stream_id)
    assert link_state == "wired", f"the pattern-to-reader link of `{stream_id}` is {link_state}"


def stream_ids_in_the_machine_graph(local_api: LocalApiClient) -> "list[str]":
    """Each loaded stream's id, from `graph` with no stream named."""
    return sorted(stream_graph["stream"] for stream_graph in local_api.graph()["streams"])


def stream_record_path(machine_directories: MachineDirectoriesUnderATestRoot, stream_id: str) -> Path:
    """`<state>/streams/<stream id>.json`."""
    return machine_directories.streams_directory / f"{stream_id}.json"


def read_stream_record(tatolabd: TatolabdUnderTest, stream_id: str) -> "dict[str, Any]":
    """The stream record the runtime wrote, after checking it is its owner's alone."""
    record_path = stream_record_path(tatolabd.machine_directories, stream_id)
    assert stat.S_IMODE(os.stat(record_path).st_mode) == STREAM_RECORD_FILE_MODE, record_path
    return json.loads(record_path.read_text(encoding="utf-8"))


def assert_tatolab_succeeded(completed: "subprocess.CompletedProcess[str]") -> str:
    """Require exit 0 from a `tatolab` verb; return its standard output."""
    assert completed.returncode == 0, (
        f"`{' '.join(map(str, completed.args))}` exited {completed.returncode}; "
        f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
    )
    return completed.stdout


def restart_tatolabd(
    tatolabd: TatolabdUnderTest, start_tatolabd: "Callable[..., TatolabdUnderTest]"
) -> TatolabdUnderTest:
    """A Ctrl-C to `tatolabd`, its clean exit, and a new `tatolabd` under the same machine root."""
    stop_tatolabd_cleanly(tatolabd)
    return start_tatolabd()


def crash_tatolabd(tatolabd: TatolabdUnderTest) -> None:
    """SIGKILL `tatolabd` alone, as a crash ends it: no teardown, its socket file left behind."""
    tatolabd.send_signal(signal.SIGKILL)
    assert tatolabd.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == -signal.SIGKILL
    assert tatolabd.local_api_socket_path.exists(), "a crash leaves the socket file behind"


def stop_tatolabd_cleanly(tatolabd: TatolabdUnderTest) -> None:
    """A Ctrl-C to `tatolabd`, and its clean exit."""
    tatolabd.interrupt()
    assert tatolabd.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == 0, tatolabd.recent_stderr()


def first_rendering_of_the_stream_once_tatolabd_serves(
    start_tatolabd: "Callable[..., TatolabdUnderTest]", stream_id: str
) -> "tuple[TatolabdUnderTest, dict[str, Any], int]":
    """Start `tatolabd`, read the machine-wide graph from the moment its socket
    answers, and return the first rendering that holds `stream_id` with the
    count of renderings read before it.

    `tatolabd` serves before it re-loads its recorded streams, and the watch reads
    from the moment it answers, so the stream's first rendering is the earliest
    any client of the runtime could have seen the port.
    """
    tatolabd = start_tatolabd(wait_until_serving=False)
    local_api = LocalApiClient(tatolabd.local_api_socket_path)
    deadline = time.monotonic() + STREAM_RUNNING_TIMEOUT_SECONDS
    renderings_without_the_stream = 0
    while True:
        if tatolabd.process.poll() is not None:
            raise AssertionError(
                f"tatolabd exited {tatolabd.process.returncode} before it re-loaded "
                f"`{stream_id}`:\n{tatolabd.recent_stderr()}"
            )
        try:
            machine_graph = local_api.graph()
        except (OSError, LocalApiRefusedTheRequest):
            machine_graph = None
        if machine_graph is not None:
            for stream_graph in machine_graph["streams"]:
                if stream_graph["stream"] == stream_id:
                    return tatolabd, stream_graph, renderings_without_the_stream
            renderings_without_the_stream += 1
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"tatolabd did not re-load `{stream_id}` within {STREAM_RUNNING_TIMEOUT_SECONDS}s:"
                f"\n{tatolabd.recent_stderr()}"
            )
        time.sleep(FIRST_RENDERING_POLL_INTERVAL_SECONDS)


class McpToolCallRefused(Exception):
    """A `tools/call` the runtime answered with a tool error."""


class McpJsonRpcClientOverTheVerb:
    """An agent's MCP session over `tatolab mcp`: JSON-RPC, one message per
    line, written to the verb's stdin and read off its stdout.

    The protocol is the stateless 2026-07-28 revision: no handshake, each
    request carrying its protocol version and the client's capabilities in its
    `_meta`, and `server/discover` read once on construction. The verb is
    started as an MCP host starts it, in a session of its own. `close_standard_input()` ends the session as
    a host that exits does; `kill()` ends it as a host that crashed does.
    """

    def __init__(self, runtime_unit: RuntimeUnitUnderTest, environment: "dict[str, str]") -> None:
        self.process = subprocess.Popen(
            [str(runtime_unit.tatolab_executable), "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            start_new_session=True,
        )
        self._messages_from_the_runtime: "queue.Queue[dict[str, Any] | None]" = queue.Queue()
        self._standard_error_lines: "list[bytes]" = []
        self._next_request_id = 0
        threading.Thread(target=self._pump_standard_output, daemon=True).start()
        threading.Thread(target=self._pump_standard_error, daemon=True).start()
        discovered = self._request("server/discover", {})
        assert LATEST_PROTOCOL_VERSION in discovered["supportedVersions"], discovered

    def _pump_standard_output(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            if line.strip():
                self._messages_from_the_runtime.put(json.loads(line))
        self._messages_from_the_runtime.put(None)

    def _pump_standard_error(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self._standard_error_lines.append(line)

    @property
    def standard_error_text(self) -> str:
        """Everything the verb wrote to standard error so far."""
        return b"".join(self._standard_error_lines).decode(errors="replace")

    def _write(self, message: "dict[str, Any]") -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(message).encode() + b"\n")
        self.process.stdin.flush()

    def _request(self, method: str, params: "dict[str, Any]") -> "dict[str, Any]":
        self._next_request_id += 1
        request_id = self._next_request_id
        self._write(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": {**params, "_meta": MCP_REQUEST_META},
            }
        )
        deadline = time.monotonic() + MCP_RESPONSE_TIMEOUT_SECONDS
        while True:
            remaining = deadline - time.monotonic()
            try:
                message = self._messages_from_the_runtime.get(timeout=max(remaining, 0))
            except queue.Empty:
                raise AssertionError(
                    f"`tatolab mcp` did not answer `{method}` within "
                    f"{MCP_RESPONSE_TIMEOUT_SECONDS}s; stderr:\n{self.standard_error_text}"
                ) from None
            if message is None:
                raise AssertionError(
                    f"`tatolab mcp` closed its stdout before answering `{method}` (exit "
                    f"{self.process.poll()}); stderr:\n{self.standard_error_text}"
                )
            if message.get("id") != request_id:
                continue
            assert "error" not in message, f"`{method}` was refused: {message['error']}"
            return message["result"]

    def call_tool(self, tool_name: str, arguments: "dict[str, Any] | None" = None) -> Any:
        """`tools/call`, its text content parsed as JSON; a tool error raises `McpToolCallRefused`."""
        result = self._request("tools/call", {"name": tool_name, "arguments": arguments or {}})
        text = "\n".join(
            content["text"] for content in result["content"] if content.get("type") == "text"
        )
        if result.get("isError"):
            raise McpToolCallRefused(text)
        return json.loads(text)

    def close_standard_input(self) -> int:
        """Close the verb's stdin, as a host that exits does; return the verb's exit status."""
        assert self.process.stdin is not None
        self.process.stdin.close()
        return self.process.wait(timeout=MCP_VERB_EXIT_TIMEOUT_SECONDS)

    def kill(self) -> int:
        """SIGKILL the verb, as a host's crash ends it; return its exit status."""
        self.process.send_signal(signal.SIGKILL)
        return self.process.wait(timeout=MCP_VERB_EXIT_TIMEOUT_SECONDS)

    def end_however_it_stands(self) -> None:
        """Kill the verb if it still runs; the test's teardown."""
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=MCP_VERB_EXIT_TIMEOUT_SECONDS)


@pytest.fixture
def start_mcp_session_over_the_verb(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
) -> "Iterator[Callable[[], McpJsonRpcClientOverTheVerb]]":
    """Start `tatolab mcp` in this test's machine environment and open an MCP session over it;
    every verb still running at teardown is killed."""
    started: "list[McpJsonRpcClientOverTheVerb]" = []

    def start() -> McpJsonRpcClientOverTheVerb:
        session = McpJsonRpcClientOverTheVerb(runtime_unit, private_machine_directories.environment)
        started.append(session)
        return session

    try:
        yield start
    finally:
        for session in started:
            session.end_however_it_stands()


# --- with no runtime ------------------------------------------------------------


def test_every_verb_that_reaches_the_runtime_fails_at_once_naming_the_socket_and_starts_no_runtime(
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """No verb ever starts a runtime: each fails at the socket, naming it and
    how to start the runtime, and leaves the machine lock free for `tatolabd`."""
    project_directory = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(project_directory, ALPHA_STREAM_FUNCTION)
    local_api_socket_path = private_machine_directories.machine_directories.local_api_socket_path
    every_verb_reaching_the_runtime = (
        ("run",),
        ("run", "--dir", str(project_directory), "--name", "other"),
        ("dev",),
        ("stop", alpha_stream_id),
        ("start", alpha_stream_id),
        ("rm", alpha_stream_id),
        ("streams",),
        ("expose", alpha_stream_id, EXPOSED_NODE, EXPOSED_PORT),
        ("expose", alpha_stream_id, EXPOSED_NODE, EXPOSED_PORT, "--public"),
        ("expose", alpha_stream_id, EXPOSED_NODE, EXPOSED_PORT, "--remove"),
        ("graph",),
        ("graph", "--stream", alpha_stream_id),
        ("logs", "--stream", alpha_stream_id),
        ("logs", "--stream", alpha_stream_id, "-f"),
        ("mcp",),
    )

    for verb_arguments in every_verb_reaching_the_runtime:
        refused = run_tatolab(*verb_arguments, working_directory=project_directory)

        assert refused.returncode == 1, (verb_arguments, refused.stdout, refused.stderr)
        assert (
            f"{NO_RUNTIME_REFUSAL_PREFIX}{local_api_socket_path}. {NO_RUNTIME_REFUSAL_REMEDY}"
            in refused.stderr
        ), (verb_arguments, refused.stderr)
        assert refused.stdout == "", (verb_arguments, refused.stdout)
        assert not local_api_socket_path.exists(), (
            f"`tatolab {' '.join(verb_arguments)}` left a socket behind: a verb started a runtime"
        )

    assert start_tatolabd().local_api_client().list_streams() == [], (
        "no verb may hold the machine lock or leave a stream behind"
    )


def test_describe_runtime_answers_the_running_tatolabd_its_uptime_growing_and_reset_by_a_restart(
    private_machine_directories: PrivateMachineDirectories,
    run_tatolab_observation_verb: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """`describe_runtime` names the running `tatolabd`'s version and build, the
    folder under the test's state directory its own log is written to, and a
    monotonic uptime that grows and that a restart resets; `tatolab --version`
    shows the same runtime, and `runtime: not running` without one."""
    without_a_runtime = run_tatolab_observation_verb("--version")
    assert without_a_runtime.returncode == 0, without_a_runtime.stderr
    cli_version_line, runtime_line = without_a_runtime.stdout.splitlines()
    assert runtime_line == "runtime: not running", without_a_runtime.stdout
    cli_version = cli_version_line.removeprefix("tatolab ")

    tatolabd = start_tatolabd()
    first_description = tatolabd.local_api_client().call_tool("describe_runtime")
    first_description_measured_at = time.monotonic()

    assert first_description["version"] == cli_version, first_description
    assert first_description["build_id"].startswith(f"{cli_version}+"), first_description
    runtime_log_directory = Path(first_description["runtime_log_directory"])
    assert runtime_log_directory.is_absolute(), first_description
    machine_directories = private_machine_directories.machine_directories
    assert runtime_log_directory == machine_directories.runtime_log_directory, first_description
    assert list(runtime_log_directory.glob("tatolabd-*.jsonl")), (
        f"no tatolabd-*.jsonl in {runtime_log_directory}"
    )

    versioned = run_tatolab_observation_verb("--version")
    assert versioned.returncode == 0, versioned.stderr
    runtime_line = versioned.stdout.splitlines()[1]
    assert runtime_line.startswith(
        f"runtime {cli_version} ({first_description['build_id']}), up "
    ), versioned.stdout
    assert runtime_line.endswith(f", log {runtime_log_directory}"), versioned.stdout

    while time.monotonic() - first_description_measured_at < 1.0:
        time.sleep(STREAM_LISTING_POLL_INTERVAL_SECONDS)
    second_description = tatolabd.local_api_client().call_tool("describe_runtime")
    assert second_description["up_for_ms"] > first_description["up_for_ms"], (
        first_description,
        second_description,
    )

    old_tatolabd_stopping_at = time.monotonic()
    restarted_tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    restarted_local_api = restarted_tatolabd.local_api_client()
    description_after_the_restart = restarted_local_api.call_tool("describe_runtime")
    measured_since_stopping_the_old_tatolabd_ms = (
        time.monotonic() - old_tatolabd_stopping_at
    ) * 1000
    assert (
        description_after_the_restart["up_for_ms"] <= measured_since_stopping_the_old_tatolabd_ms
    ), (description_after_the_restart, measured_since_stopping_the_old_tatolabd_ms)


# --- a stopped stream, with no started stream ------------------------------------


def write_the_record_run_leaves(
    private_machine_directories: PrivateMachineDirectories,
    project_directory: Path,
    *,
    stopped: bool,
) -> "dict[str, Any]":
    """Write the stream record `tatolab run` leaves for the project's
    sole stream — and, `stopped`, the one a `tatolab stop` after it leaves —
    its graph and location compiled by the project's own interpreter as the
    runtime compiles them, under the id that location computes to; return the
    record.

    Starting a stream needs a GPU context, so a GPU-free test cannot have the
    runtime write it; a stopped stream is never started again by a restart, so
    from here on the runtime treats it exactly as one it stopped itself. A
    record not stopped is one a `tatolabd` start re-loads.
    """
    project_interpreter = project_directory / ".venv" / "bin" / "python"
    compiled = subprocess.run(
        [str(project_interpreter), *COMPILE_ENTRY_INVOCATION, "--verb", "run"],
        cwd=project_directory,
        capture_output=True,
        text=True,
        check=True,
        env={name: value for name, value in os.environ.items() if not name.startswith("PYTHON")},
    )
    compile_document = json.loads(compiled.stdout)
    record = {
        "schema_version": STREAM_RECORD_SCHEMA_VERSION,
        "stream_id": stream_id_of_the_location(
            Path(compile_document["project_directory"]), compile_document["stream_function"]
        ),
        "location": {
            "project_directory": compile_document["project_directory"],
            "stream_function": compile_document["stream_function"],
        },
        "display_name": compile_document["stream_graph"]["stream"],
        "interpreter": str(project_interpreter),
        "graph": compile_document["stream_graph"],
        "stopped": stopped,
        "failed_because": None,
        "runtime_crashes_in_a_row_implicating_it": 0,
        "exposure_rulings": [],
    }
    record_path = stream_record_path(
        private_machine_directories.machine_directories, record["stream_id"]
    )
    record_path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    record_path.write_text(json.dumps(record), encoding="utf-8")
    record_path.chmod(STREAM_RECORD_FILE_MODE)
    return record


def test_a_stopped_stream_stays_stopped_across_restarts_takes_the_owners_ruling_and_rm_forgets_it(
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    project_directory = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(project_directory, ALPHA_STREAM_FUNCTION)
    record = write_the_record_run_leaves(
        private_machine_directories, project_directory, stopped=True
    )
    assert record["stream_id"] == alpha_stream_id, record
    assert record["graph"]["exposed"] == exposure_at("public"), record["graph"]
    recorded_project_directory = record["location"]["project_directory"]
    stopped_listing = listing_of(
        alpha_stream_id, ALPHA_STREAM_FUNCTION, "stopped", Path(recorded_project_directory), None
    )

    tatolabd = start_tatolabd()
    tatolabd.await_stderr_containing(runtime_serving_log_line(0, 0))
    assert f"the stream `{alpha_stream_id}` is stopped, so it is not re-loaded" in tatolabd.stderr_text
    assert tatolabd.local_api_client().list_streams() == [stopped_listing]
    listed_table = assert_tatolab_succeeded(run_tatolab("streams", working_directory=project_directory))
    assert listed_table.splitlines()[0].split() == [
        "ID",
        "NAME",
        "STATE",
        "NODES",
        "PROJECT",
        "FUNCTION",
    ]
    assert listed_table.splitlines()[1].split() == [
        alpha_stream_id,
        ALPHA_STREAM_FUNCTION,
        "stopped",
        "-",
        recorded_project_directory,
        stream_function_of_the_pattern_project(ALPHA_STREAM_FUNCTION),
    ]

    exposed = assert_tatolab_succeeded(
        run_tatolab(
            "expose", alpha_stream_id, EXPOSED_NODE, EXPOSED_PORT, working_directory=project_directory
        )
    )
    assert exposed == (
        f"{alpha_stream_id}/{EXPOSED_NODE}/{EXPOSED_PORT} is private (recorded; it holds across "
        f"restarts)\n"
    )
    refused_node = run_tatolab(
        "expose", alpha_stream_id, "no-such-node", EXPOSED_PORT, working_directory=project_directory
    )
    assert refused_node.returncode == 1, refused_node.stderr
    assert "holds no node `no-such-node`" in refused_node.stderr, refused_node.stderr
    owners_ruling = {"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": "private"}
    assert read_stream_record(tatolabd, alpha_stream_id)["exposure_rulings"] == [owners_ruling]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(runtime_serving_log_line(0, 0))
    assert tatolabd.local_api_client().list_streams() == [stopped_listing]
    restarted_record = read_stream_record(tatolabd, alpha_stream_id)
    assert restarted_record["stopped"] is True
    assert restarted_record["exposure_rulings"] == [owners_ruling]
    refused_stop = run_tatolab("stop", alpha_stream_id, working_directory=project_directory)
    assert refused_stop.returncode == 1, refused_stop.stderr
    assert "already stopped" in refused_stop.stderr, refused_stop.stderr

    removed = assert_tatolab_succeeded(run_tatolab("rm", alpha_stream_id, working_directory=project_directory))
    assert removed == f"{alpha_stream_id} removed (forgotten)\n"
    assert not stream_record_path(tatolabd.machine_directories, alpha_stream_id).exists()
    assert assert_tatolab_succeeded(
        run_tatolab("streams", working_directory=project_directory)
    ) == "No streams in this runtime.\n"
    refused_rm = run_tatolab("rm", alpha_stream_id, working_directory=project_directory)
    assert refused_rm.returncode == 1, refused_rm.stderr

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(runtime_serving_log_line(0, 0))
    assert tatolabd.local_api_client().list_streams() == []
    stop_tatolabd_cleanly(tatolabd)


# --- with started streams ------------------------------------------------------


@pytest.mark.requires_gpu
def test_two_projects_streams_run_come_back_after_a_restart_and_stop_start_rm_hold(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(alpha_project, ALPHA_STREAM_FUNCTION)
    bravo_project = make_project_running_a_pattern_stream(make_tatolab_project, BRAVO_STREAM_FUNCTION)
    bravo_stream_id = stream_id_of_the_pattern_project(bravo_project, BRAVO_STREAM_FUNCTION)
    tatolabd = start_tatolabd()

    for stream_id, stream_function_name, project_directory in (
        (alpha_stream_id, ALPHA_STREAM_FUNCTION, alpha_project),
        (bravo_stream_id, BRAVO_STREAM_FUNCTION, bravo_project),
    ):
        running = assert_tatolab_succeeded(run_tatolab("run", working_directory=project_directory))
        assert running == (
            f'{stream_id} running as "{stream_function_name}" (project {project_directory}, '
            f"{stream_function_of_the_pattern_project(stream_function_name)})\n"
            f"  follow it: tatolab logs -f\n"
        )
    alpha_running = listing_of(alpha_stream_id, ALPHA_STREAM_FUNCTION, "running", alpha_project, PROJECT_STREAM_NODE_COUNT)
    bravo_running = listing_of(bravo_stream_id, BRAVO_STREAM_FUNCTION, "running", bravo_project, PROJECT_STREAM_NODE_COUNT)

    def assert_both_streams_run(tatolabd: TatolabdUnderTest) -> None:
        local_api = tatolabd.local_api_client()
        for stream_id in (alpha_stream_id, bravo_stream_id):
            local_api.await_every_node_running(
                stream=stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
            )
        assert listed_streams_by_id(local_api) == {
            alpha_stream_id: alpha_running,
            bravo_stream_id: bravo_running,
        }
        assert stream_ids_in_the_machine_graph(local_api) == [alpha_stream_id, bravo_stream_id]
        for stream_id in (alpha_stream_id, bravo_stream_id):
            assert_the_pattern_is_wired_to_the_reader_inside_the_stream(local_api, stream_id)

    assert_both_streams_run(tatolabd)
    table_rows = [
        row.split()
        for row in assert_tatolab_succeeded(
            run_tatolab("streams", working_directory=alpha_project)
        ).splitlines()[1:]
    ]
    assert sorted(table_rows) == [
        [
            alpha_stream_id,
            ALPHA_STREAM_FUNCTION,
            "running",
            str(PROJECT_STREAM_NODE_COUNT),
            str(alpha_project),
            stream_function_of_the_pattern_project(ALPHA_STREAM_FUNCTION),
        ],
        [
            bravo_stream_id,
            BRAVO_STREAM_FUNCTION,
            "running",
            str(PROJECT_STREAM_NODE_COUNT),
            str(bravo_project),
            stream_function_of_the_pattern_project(BRAVO_STREAM_FUNCTION),
        ],
    ]
    for stream_id, project_directory in ((alpha_stream_id, alpha_project), (bravo_stream_id, bravo_project)):
        record = read_stream_record(tatolabd, stream_id)
        assert (record["stream_id"], record["location"]["project_directory"], record["stopped"]) == (
            stream_id,
            str(project_directory),
            False,
        )

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(
        runtime_serving_log_line(2, 0), timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert_both_streams_run(tatolabd)

    stopped = assert_tatolab_succeeded(run_tatolab("stop", alpha_stream_id, working_directory=alpha_project))
    assert stopped == (
        f"{alpha_stream_id} stopped; it stays stopped across restarts until "
        f"`tatolab start {alpha_stream_id}`\n"
    )
    alpha_stopped = listing_of(alpha_stream_id, ALPHA_STREAM_FUNCTION, "stopped", alpha_project, None)
    local_api = tatolabd.local_api_client()
    assert listed_streams_by_id(local_api) == {alpha_stream_id: alpha_stopped, bravo_stream_id: bravo_running}
    assert stream_ids_in_the_machine_graph(local_api) == [bravo_stream_id]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(
        runtime_serving_log_line(1, 0), timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert f"the stream `{alpha_stream_id}` is stopped, so it is not re-loaded" in tatolabd.stderr_text
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_id(local_api) == {alpha_stream_id: alpha_stopped, bravo_stream_id: bravo_running}
    assert stream_ids_in_the_machine_graph(local_api) == [bravo_stream_id]

    started = assert_tatolab_succeeded(run_tatolab("start", alpha_stream_id, working_directory=alpha_project))
    assert started == f"{alpha_stream_id} started ({PROJECT_STREAM_NODE_COUNT} nodes)\n"
    assert_both_streams_run(tatolabd)
    assert read_stream_record(tatolabd, alpha_stream_id)["stopped"] is False

    removed = assert_tatolab_succeeded(run_tatolab("rm", alpha_stream_id, working_directory=alpha_project))
    assert removed == f"{alpha_stream_id} removed (unloaded and forgotten)\n"
    assert not stream_record_path(tatolabd.machine_directories, alpha_stream_id).exists()
    assert listed_streams_by_id(local_api) == {bravo_stream_id: bravo_running}
    assert stream_ids_in_the_machine_graph(local_api) == [bravo_stream_id]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(
        runtime_serving_log_line(1, 0), timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_id(local_api) == {bravo_stream_id: bravo_running}
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_a_stream_dev_loaded_runs_on_past_a_ctrl_c_and_a_killed_cli_with_the_owners_ruling(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """Every load persists: a `tatolab dev` ended by a Ctrl-C, and another
    killed outright, each leave the stream it loaded running and recorded, the
    owner's ruling made meanwhile included, beside a stream `tatolab run`
    loaded."""
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(alpha_project, ALPHA_STREAM_FUNCTION)
    bravo_project = make_project_running_a_pattern_stream(make_tatolab_project, BRAVO_STREAM_FUNCTION)
    bravo_stream_id = stream_id_of_the_pattern_project(bravo_project, BRAVO_STREAM_FUNCTION)
    tatolabd = start_tatolabd()
    assert_tatolab_succeeded(run_tatolab("run", working_directory=bravo_project))
    local_api = tatolabd.local_api_client()
    bravo_running = listing_of(bravo_stream_id, BRAVO_STREAM_FUNCTION, "running", bravo_project, PROJECT_STREAM_NODE_COUNT)
    alpha_running = listing_of(alpha_stream_id, ALPHA_STREAM_FUNCTION, "running", alpha_project, PROJECT_STREAM_NODE_COUNT)

    def dev_alpha() -> TatolabRunOrDevOnTheRuntime:
        dev = tatolabd.run_stream(TatolabRunOfAProject(working_directory=alpha_project), verb="dev")
        loaded = dev.await_loaded(timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
        assert (loaded["stream_id"], loaded["display_name"]) == (alpha_stream_id, ALPHA_STREAM_FUNCTION)
        local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
        assert listed_streams_by_id(local_api) == {
            alpha_stream_id: alpha_running,
            bravo_stream_id: bravo_running,
        }
        return dev

    dev_ended_by_ctrl_c = dev_alpha()
    exposed = assert_tatolab_succeeded(
        run_tatolab(
            "expose",
            alpha_stream_id,
            EXPOSED_NODE,
            EXPOSED_PORT,
            "--remove",
            working_directory=alpha_project,
        )
    )
    assert exposed == (
        f"{alpha_stream_id}/{EXPOSED_NODE}/{EXPOSED_PORT} is internal (recorded; it holds across "
        f"restarts)\n"
    )
    assert local_api.graph(alpha_stream_id)["exposed"] == exposure_at("internal")
    assert read_stream_record(tatolabd, alpha_stream_id)["exposure_rulings"] == [
        {"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": "internal"}
    ]
    dev_ended_by_ctrl_c.interrupt()
    assert dev_ended_by_ctrl_c.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == 0, (
        dev_ended_by_ctrl_c.recent_stderr()
    )
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: bravo_running,
    }, "a Ctrl-C to `dev` ends the following, never the stream"

    dev_ended_by_sigkill = dev_alpha()
    assert local_api.graph(alpha_stream_id)["exposed"] == exposure_at("internal"), (
        "a run of the recorded location replaces the stream keeping the owner's ruling"
    )
    os.kill(dev_ended_by_sigkill.pid, signal.SIGKILL)
    assert dev_ended_by_sigkill.await_exit() == -signal.SIGKILL
    local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: bravo_running,
    }, "a killed `dev` leaves its stream running"
    assert stream_ids_in_the_machine_graph(local_api) == [alpha_stream_id, bravo_stream_id]
    assert read_stream_record(tatolabd, alpha_stream_id)["stopped"] is False
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_expose_sets_a_running_streams_port_internal_private_or_public_live_and_nothing_restarts(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(alpha_project, ALPHA_STREAM_FUNCTION)
    tatolabd = start_tatolabd()
    assert_tatolab_succeeded(run_tatolab("run", working_directory=alpha_project))
    local_api = tatolabd.local_api_client()
    running_graph = local_api.await_every_node_running(
        stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert running_graph["exposed"] == exposure_at("public")
    node_ids_while_running = sorted(node["id"] for node in running_graph["nodes"])
    start_line = f"[start] Starting the stream `{alpha_stream_id}`"
    assert tatolabd.stderr_text.count(start_line) == 1, tatolabd.recent_stderr()

    for flags, level in (((), "private"), (("--remove",), "internal"), (("--public",), "public")):
        exposed = assert_tatolab_succeeded(
            run_tatolab(
                "expose",
                alpha_stream_id,
                EXPOSED_NODE,
                EXPOSED_PORT,
                *flags,
                working_directory=alpha_project,
            )
        )
        assert exposed == (
            f"{alpha_stream_id}/{EXPOSED_NODE}/{EXPOSED_PORT} is {level} (recorded; it holds across "
            f"restarts)\n"
        )
        graph_right_after = local_api.graph(alpha_stream_id)
        assert graph_right_after["exposed"] == exposure_at(level), (level, graph_right_after["exposed"])
        assert sorted(node["id"] for node in graph_right_after["nodes"]) == node_ids_while_running, (
            "expose must change the level live, never reload the stream"
        )
        cli_graph = json.loads(
            assert_tatolab_succeeded(
                run_tatolab("graph", "--stream", alpha_stream_id, working_directory=alpha_project)
            )
        )
        assert cli_graph["exposed"] == exposure_at(level), (level, cli_graph["exposed"])
        assert read_stream_record(tatolabd, alpha_stream_id)["exposure_rulings"] == [
            {"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": level}
        ]

    local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert tatolabd.stderr_text.count(start_line) == 1, (
        f"expose restarted the stream:\n{tatolabd.recent_stderr()}"
    )
    assert stream_stopped_log_line(alpha_stream_id) not in tatolabd.stderr_text
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: listing_of(alpha_stream_id, ALPHA_STREAM_FUNCTION, "running", alpha_project, PROJECT_STREAM_NODE_COUNT)
    }
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_the_owners_restriction_holds_through_a_restart_and_a_crashs_restart_before_any_reader(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The function exposes the port public; the owner restricts it. After a
    restart and after a crash, the stream's very first rendering — read from
    the moment `tatolabd` serves, while it re-loads — already carries the
    owner's level, so no reader could have been admitted at the function's."""
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(alpha_project, ALPHA_STREAM_FUNCTION)
    tatolabd = start_tatolabd()
    assert_tatolab_succeeded(run_tatolab("run", working_directory=alpha_project))
    tatolabd.local_api_client().await_every_node_running(
        stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert_tatolab_succeeded(
        run_tatolab("expose", alpha_stream_id, EXPOSED_NODE, EXPOSED_PORT, working_directory=alpha_project)
    )
    record = read_stream_record(tatolabd, alpha_stream_id)
    assert record["graph"]["exposed"] == exposure_at("public"), (
        "the record keeps the function's own level beside the owner's ruling"
    )
    assert record["exposure_rulings"] == [
        {"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": "private"}
    ]

    def assert_the_first_rendering_carries(
        first_rendering: "dict[str, Any]", renderings_before_it: int, level: str
    ) -> None:
        assert first_rendering["exposed"] == exposure_at(level), (
            f"the stream's first rendering after its re-load must carry the owner's {level}; it "
            f"carried {first_rendering['exposed']} after {renderings_before_it} renderings "
            f"without the stream"
        )

    stop_tatolabd_cleanly(tatolabd)
    tatolabd, first_rendering, renderings_before_it = first_rendering_of_the_stream_once_tatolabd_serves(
        start_tatolabd, alpha_stream_id
    )
    assert_the_first_rendering_carries(first_rendering, renderings_before_it, "private")
    local_api = tatolabd.local_api_client()
    assert local_api.await_every_node_running(
        stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )["exposed"] == exposure_at("private")
    assert "kept and not applied" not in tatolabd.stderr_text, tatolabd.recent_stderr()
    assert_the_pattern_is_wired_to_the_reader_inside_the_stream(local_api, alpha_stream_id)

    assert_tatolab_succeeded(
        run_tatolab(
            "expose",
            alpha_stream_id,
            EXPOSED_NODE,
            EXPOSED_PORT,
            "--remove",
            working_directory=alpha_project,
        )
    )
    assert local_api.graph(alpha_stream_id)["exposed"] == exposure_at("internal")
    crash_tatolabd(tatolabd)
    tatolabd, first_rendering, renderings_before_it = first_rendering_of_the_stream_once_tatolabd_serves(
        start_tatolabd, alpha_stream_id
    )
    assert_the_first_rendering_carries(first_rendering, renderings_before_it, "internal")
    local_api = tatolabd.local_api_client()
    graph_running_after_the_crash = local_api.await_every_node_running(
        stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert graph_running_after_the_crash["exposed"] == exposure_at("internal")
    assert_the_pattern_is_wired_to_the_reader_inside_the_stream(local_api, alpha_stream_id)
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: listing_of(alpha_stream_id, ALPHA_STREAM_FUNCTION, "running", alpha_project, PROJECT_STREAM_NODE_COUNT)
    }
    assert read_stream_record(tatolabd, alpha_stream_id)["graph"]["exposed"] == exposure_at("public")
    stop_tatolabd_cleanly(tatolabd)


#: The stream functions of the recorded streams a start re-loads ahead of the three
#: an owner changes while it re-loads; each is a GPU start of its own, which
#: holds the window open.
STREAM_FUNCTIONS_RE_LOADED_AHEAD = tuple(f"a{index}_pattern" for index in range(6))
#: Each named so its project folder, the leading part of its stream id, sorts
#: after every stream re-loaded ahead, as a start re-loads in record-file order.
RESTRICTED_WHILE_RE_LOADING_STREAM_FUNCTION = "zz_restricted_pattern"
STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION = "zz_stopped_pattern"
REMOVED_WHILE_RE_LOADING_STREAM_FUNCTION = "zz_removed_pattern"
#: How long a start re-loading seven recorded streams may take to say it is serving.
RE_LOAD_OF_SEVEN_STREAMS_TIMEOUT_SECONDS = 2 * STREAM_RUNNING_TIMEOUT_SECONDS


@pytest.mark.requires_gpu
def test_a_restriction_stop_or_rm_made_while_tatolabd_re_loads_its_recorded_streams_holds(
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """`tatolabd` serves before it re-loads its recorded streams. An owner who
    restricts a port, stops a stream and removes another the moment the socket
    answers — before the re-load reaches them — is never overruled by it: the
    restriction holds from the stream's load, the stopped stream stays
    unloaded, and the removed one is not loaded."""
    projects_by_stream_function = {
        stream_function_name: make_project_running_a_pattern_stream(
            make_tatolab_project, stream_function_name
        )
        for stream_function_name in (
            *STREAM_FUNCTIONS_RE_LOADED_AHEAD,
            RESTRICTED_WHILE_RE_LOADING_STREAM_FUNCTION,
            STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION,
            REMOVED_WHILE_RE_LOADING_STREAM_FUNCTION,
        )
    }
    records_by_stream_function = {
        stream_function_name: write_the_record_run_leaves(
            private_machine_directories, project_directory, stopped=False
        )
        for stream_function_name, project_directory in projects_by_stream_function.items()
    }
    stream_id_of = {
        stream_function_name: record["stream_id"]
        for stream_function_name, record in records_by_stream_function.items()
    }
    restricted_stream_id = stream_id_of[RESTRICTED_WHILE_RE_LOADING_STREAM_FUNCTION]
    stopped_stream_id = stream_id_of[STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION]
    removed_stream_id = stream_id_of[REMOVED_WHILE_RE_LOADING_STREAM_FUNCTION]

    tatolabd = start_tatolabd()
    some_project = projects_by_stream_function[RESTRICTED_WHILE_RE_LOADING_STREAM_FUNCTION]
    assert_tatolab_succeeded(
        run_tatolab(
            "expose",
            restricted_stream_id,
            EXPOSED_NODE,
            EXPOSED_PORT,
            "--remove",
            working_directory=some_project,
        )
    )
    assert_tatolab_succeeded(run_tatolab("stop", stopped_stream_id, working_directory=some_project))
    assert_tatolab_succeeded(run_tatolab("rm", removed_stream_id, working_directory=some_project))
    re_load_reached_them_first = "the runtime is serving: " in tatolabd.stderr_text
    tatolabd.await_stderr_containing(
        "the runtime is serving: ", timeout=RE_LOAD_OF_SEVEN_STREAMS_TIMEOUT_SECONDS
    )
    local_api = tatolabd.local_api_client()

    restricted_graph = local_api.await_every_node_running(
        stream=restricted_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert restricted_graph["exposed"] == exposure_at("internal"), (
        "the owner's restriction made while the start re-loaded was overruled by the re-load; "
        f"the re-load had already finished when the owner acted: {re_load_reached_them_first}"
    )
    assert read_stream_record(tatolabd, restricted_stream_id)["exposure_rulings"] == [
        {"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": "internal"}
    ]
    assert read_stream_record(tatolabd, stopped_stream_id)["stopped"] is True
    assert not stream_record_path(tatolabd.machine_directories, removed_stream_id).exists()

    def listing_of_the_recorded(stream_function_name: str, state: str, node_count: "int | None") -> "dict[str, Any]":
        return listing_of(
            stream_id_of[stream_function_name],
            stream_function_name,
            state,
            Path(records_by_stream_function[stream_function_name]["location"]["project_directory"]),
            node_count,
        )

    stream_functions_running = (
        *STREAM_FUNCTIONS_RE_LOADED_AHEAD,
        RESTRICTED_WHILE_RE_LOADING_STREAM_FUNCTION,
    )
    assert listed_streams_by_id(local_api) == {
        **{
            stream_id_of[stream_function_name]: listing_of_the_recorded(
                stream_function_name, "running", PROJECT_STREAM_NODE_COUNT
            )
            for stream_function_name in stream_functions_running
        },
        stopped_stream_id: listing_of_the_recorded(
            STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION, "stopped", None
        ),
    }, f"the re-load had already finished when the owner acted: {re_load_reached_them_first}"
    assert stream_ids_in_the_machine_graph(local_api) == sorted(
        stream_id_of[stream_function_name] for stream_function_name in stream_functions_running
    )
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_an_agent_does_all_of_it_over_tatolab_mcp(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    start_mcp_session_over_the_verb: "Callable[[], McpJsonRpcClientOverTheVerb]",
):
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(alpha_project, ALPHA_STREAM_FUNCTION)
    bravo_project = make_project_running_a_pattern_stream(make_tatolab_project, BRAVO_STREAM_FUNCTION)
    bravo_stream_id = stream_id_of_the_pattern_project(bravo_project, BRAVO_STREAM_FUNCTION)
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()
    alpha_running = listing_of(alpha_stream_id, ALPHA_STREAM_FUNCTION, "running", alpha_project, PROJECT_STREAM_NODE_COUNT)
    bravo_running = listing_of(bravo_stream_id, BRAVO_STREAM_FUNCTION, "running", bravo_project, PROJECT_STREAM_NODE_COUNT)

    def listed_through(agent: McpJsonRpcClientOverTheVerb) -> "dict[str, dict[str, Any]]":
        return {listed["stream"]: listed for listed in agent.call_tool("list_streams")["streams"]}

    def run_stream(agent: McpJsonRpcClientOverTheVerb, project_directory: Path) -> "dict[str, Any]":
        return agent.call_tool("run_stream", {"project_directory": str(project_directory)})

    # Two projects run, both running; the owner sets one port at each level
    # live, ending on private, and nothing restarts.
    agent = start_mcp_session_over_the_verb()
    for stream_id, stream_function_name, project_directory in (
        (alpha_stream_id, ALPHA_STREAM_FUNCTION, alpha_project),
        (bravo_stream_id, BRAVO_STREAM_FUNCTION, bravo_project),
    ):
        run = run_stream(agent, project_directory)
        stream_instance = run.pop("stream_instance")
        assert isinstance(stream_instance, str) and stream_instance, run
        assert run == {
            "stream": stream_id,
            "display_name": stream_function_name,
            "location": {
                "project_directory": str(project_directory),
                "stream_function": stream_function_of_the_pattern_project(stream_function_name),
            },
            "node_count": PROJECT_STREAM_NODE_COUNT,
            "replaced_a_recorded_stream": False,
            "compile_warnings": [],
        }
        local_api.await_every_node_running(stream=stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_through(agent) == {alpha_stream_id: alpha_running, bravo_stream_id: bravo_running}
    assert sorted(
        stream_graph["stream"] for stream_graph in agent.call_tool("graph")["streams"]
    ) == [alpha_stream_id, bravo_stream_id]
    alpha_graph_while_running = agent.call_tool("graph", {"stream": alpha_stream_id})
    assert alpha_graph_while_running["exposed"] == exposure_at("public")
    alpha_node_ids_while_running = sorted(node["id"] for node in alpha_graph_while_running["nodes"])
    alpha_start_line = f"[start] Starting the stream `{alpha_stream_id}`"
    assert tatolabd.stderr_text.count(alpha_start_line) == 1, tatolabd.recent_stderr()
    for level in ("internal", "public", "private"):
        assert agent.call_tool(
            "expose_port",
            {"stream": alpha_stream_id, "node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": level},
        ) == {
            "stream": alpha_stream_id,
            "node": EXPOSED_NODE,
            "port": EXPOSED_PORT,
            "level": level,
            "recorded": True,
        }
        alpha_graph_right_after = agent.call_tool("graph", {"stream": alpha_stream_id})
        assert alpha_graph_right_after["exposed"] == exposure_at(level), (
            level,
            alpha_graph_right_after["exposed"],
        )
        assert sorted(
            node["id"] for node in alpha_graph_right_after["nodes"]
        ) == alpha_node_ids_while_running, "expose_port must change the level live, never reload"
        assert read_stream_record(tatolabd, alpha_stream_id)["exposure_rulings"] == [
            {"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": level}
        ]
    local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert tatolabd.stderr_text.count(alpha_start_line) == 1, (
        f"expose_port restarted the stream:\n{tatolabd.recent_stderr()}"
    )
    assert stream_stopped_log_line(alpha_stream_id) not in tatolabd.stderr_text
    assert agent.call_tool("stop_stream", {"stream": bravo_stream_id}) == {
        "stream": bravo_stream_id,
        "stopped": True,
    }
    assert agent.close_standard_input() == 0, agent.standard_error_text
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: listing_of(bravo_stream_id, BRAVO_STREAM_FUNCTION, "stopped", bravo_project, None),
    }, "every stream outlives the connection that ran it"

    # Across a crash's restart: the running stream's first rendering already
    # carries the owner's private over the function's public, and the stopped
    # one stays stopped.
    crash_tatolabd(tatolabd)
    tatolabd, first_rendering, renderings_before_it = first_rendering_of_the_stream_once_tatolabd_serves(
        start_tatolabd, alpha_stream_id
    )
    assert first_rendering["exposed"] == exposure_at("private"), (
        f"the first rendering after the crash's re-load carried {first_rendering['exposed']} "
        f"after {renderings_before_it} renderings without the stream"
    )
    tatolabd.await_stderr_containing(
        runtime_serving_log_line(1, 0), timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    agent = start_mcp_session_over_the_verb()
    assert listed_through(agent) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: listing_of(bravo_stream_id, BRAVO_STREAM_FUNCTION, "stopped", bravo_project, None),
    }
    assert agent.call_tool("graph", {"stream": alpha_stream_id})["exposed"] == exposure_at("private")
    assert agent.call_tool("start_stream", {"stream": bravo_stream_id}) == {
        "stream": bravo_stream_id,
        "node_count": PROJECT_STREAM_NODE_COUNT,
    }
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_through(agent) == {alpha_stream_id: alpha_running, bravo_stream_id: bravo_running}
    assert agent.call_tool("remove_stream", {"stream": bravo_stream_id}) == {
        "stream": bravo_stream_id,
        "unloaded": True,
        "forgotten": True,
    }
    assert listed_through(agent) == {alpha_stream_id: alpha_running}
    assert not stream_record_path(tatolabd.machine_directories, bravo_stream_id).exists()

    # Run again by the agent, then the agent quits: the stream runs on, recorded.
    assert run_stream(agent, bravo_project)["replaced_a_recorded_stream"] is False
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_through(agent)[bravo_stream_id] == bravo_running
    assert agent.close_standard_input() == 0, agent.standard_error_text
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: bravo_running,
    }, "an MCP host that ran a stream and exited leaves it running"
    assert stream_record_path(tatolabd.machine_directories, bravo_stream_id).exists()

    # Run again over a connection whose host then crashes: the run replaces the
    # recorded stream, and the killed verb leaves it running.
    crashing_agent = start_mcp_session_over_the_verb()
    assert run_stream(crashing_agent, bravo_project)["replaced_a_recorded_stream"] is True
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert crashing_agent.kill() == -signal.SIGKILL
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: bravo_running,
    }, "a killed MCP host leaves the stream it ran running"
    assert stream_record_path(tatolabd.machine_directories, bravo_stream_id).exists()

    local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    stop_tatolabd_cleanly(tatolabd)
