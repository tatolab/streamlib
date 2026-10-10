# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""#2605's done-means, end to end: `tatolabd` hosts the machine's streams, and
every verb that manages them reaches it through the real CLI.

Each test drives the runtime unit's own `tatolabd` and `tatolab` under the
test's machine root, as a user at a terminal does: `tatolab run -d` from two
project directories, `tatolabd` restarted with a Ctrl-C and again after a
SIGKILL, `stop`, `start`, `rm`, `streams` and `expose` one call at a time, an
attached `tatolab run` ended by a Ctrl-C and by a SIGKILL, and an agent doing
the same over `tatolab mcp` with a JSON-RPC client speaking over the verb's
stdin and stdout.

A started stream needs a GPU context, so most tests here are `requires_gpu`;
their streams are native built-ins only — a test pattern into a window — so no
camera is needed. What needs no started stream runs without one: every verb
with no runtime, and a stopped kept stream through restarts, the owner's
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

from conftest import AttachedTatolabRun, PrivateMachineDirectories, TatolabdUnderTest
from local_api_client import LocalApiClient, LocalApiRefusedTheRequest
from runtime_process_under_test import ENGINE_GRACEFUL_STOP_LOG_LINE
from runtime_unit_under_test import MachineDirectoriesUnderATestRoot, RuntimeUnitUnderTest
from stream_runs_on_tatolabd import COMPILE_ENTRY_INVOCATION, TatolabRunOfAProject

#: A load is the project's compile, and on the runtime's first start its GPU context.
STREAM_RUNNING_TIMEOUT_SECONDS = 90.0
#: How long a stream whose connection closed may take to leave `list_streams`.
ATTACHED_STREAM_UNLOAD_TIMEOUT_SECONDS = 30.0
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

#: The two projects of the done-means, each running the sole `@stream` of its
#: own `stream.py`, named after the function.
ALPHA_STREAM = "alpha_pattern"
BRAVO_STREAM = "bravo_pattern"

#: Every project stream here: a test pattern into a small window, its `video`
#: exposed public by the stream function — the author's suggestion, which an
#: owner's ruling overrides.
PROJECT_STREAM_SOURCE_TEMPLATE = '''\
from tatolab.stream import DisplayWindow, Exposure, StreamBuilder, TestPatternSource, stream


@stream
def {stream_function_name}(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, name="pattern", config={{"width": 320, "height": 180}}
    )
    window = stream_builder.add(
        DisplayWindow,
        name="window",
        config={{"width": 160, "height": 90, "title": "{stream_function_name}"}},
    )
    stream_builder.connect(pattern.output("video"), window.input("video"))
    stream_builder.expose(pattern.output("video"), Exposure.PUBLIC)
'''
PROJECT_STREAM_NODE_COUNT = 2
#: Bags a tap on a running stream's pattern collects to show frames flow.
TAPPED_BAG_COUNT = 2
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

KEPT_STREAM_RECORD_FILE_MODE = 0o600
KEPT_STREAM_RECORD_SCHEMA_VERSION = 1


def runtime_serving_log_line(reloaded_count: int, skipped_count: int) -> str:
    """`tatolabd`'s line once its start's re-loads are over."""
    return (
        f"the runtime is serving: {reloaded_count} kept streams re-loaded, "
        f"{skipped_count} skipped"
    )


def stream_stopped_log_line(stream_name: str) -> str:
    """The engine's line once a stream's graceful stop has finished."""
    return f"{ENGINE_GRACEFUL_STOP_LOG_LINE}{stream_name}` stopped"


def make_project_running_a_pattern_stream(
    make_tatolab_project: "Callable[..., Path]", stream_function_name: str
) -> Path:
    """A project directory of its own whose `stream.py` holds the pattern stream, resolved."""
    return make_tatolab_project(
        {
            "stream.py": PROJECT_STREAM_SOURCE_TEMPLATE.format(
                stream_function_name=stream_function_name
            )
        },
        directory_name=stream_function_name.replace("_", "-"),
    ).resolve()


def listed_streams_by_name(local_api: LocalApiClient) -> "dict[str, dict[str, Any]]":
    """`list_streams`, keyed by stream name."""
    return {listed["name"]: listed for listed in local_api.list_streams()}


def listing_of(stream_name: str, state: str, project_directory: Path, node_count: "int | None") -> "dict[str, Any]":
    """One `list_streams` entry."""
    return {
        "name": stream_name,
        "state": state,
        "project_directory": str(project_directory),
        "node_count": node_count,
    }


def await_stream_unlisted(
    local_api: LocalApiClient, stream_name: str, *, timeout: float = ATTACHED_STREAM_UNLOAD_TIMEOUT_SECONDS
) -> "dict[str, dict[str, Any]]":
    """Poll `list_streams` until `stream_name` is no longer listed; return the listing then."""
    deadline = time.monotonic() + timeout
    while True:
        listed = listed_streams_by_name(local_api)
        if stream_name not in listed:
            return listed
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"the stream `{stream_name}` was still listed {timeout}s after its connection "
                f"closed: {listed}"
            )
        time.sleep(STREAM_LISTING_POLL_INTERVAL_SECONDS)


def assert_frames_flow_out_of_the_pattern(
    local_api: LocalApiClient,
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    stream_name: str,
    working_directory: Path,
) -> None:
    """`tatolab tap` on the stream's pattern output receives bags: the stream runs, not just loads."""
    channel = f"{local_api.graph(stream_name)['runtime_name']}/{EXPOSED_NODE}/{EXPOSED_PORT}"
    tapped = json.loads(
        assert_tatolab_succeeded(
            run_tatolab(
                "tap",
                channel,
                "--stream",
                stream_name,
                "--count",
                str(TAPPED_BAG_COUNT),
                working_directory=working_directory,
            )
        )
    )
    assert tapped["received"] > 0, f"no bag reached the tap on {channel} of `{stream_name}`: {tapped}"


def stream_names_in_the_machine_graph(local_api: LocalApiClient) -> "list[str]":
    """Each loaded stream's name, from `graph` with no stream named."""
    return sorted(stream_graph["stream"] for stream_graph in local_api.graph()["streams"])


def kept_stream_record_path(
    machine_directories: MachineDirectoriesUnderATestRoot, stream_name: str
) -> Path:
    """`<state>/streams/<stream>.json`."""
    return machine_directories.kept_streams_directory / f"{stream_name}.json"


def read_kept_stream_record(tatolabd: TatolabdUnderTest, stream_name: str) -> "dict[str, Any]":
    """The kept-stream record the runtime wrote, after checking it is its owner's alone."""
    record_path = kept_stream_record_path(tatolabd.machine_directories, stream_name)
    assert stat.S_IMODE(os.stat(record_path).st_mode) == KEPT_STREAM_RECORD_FILE_MODE, record_path
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
    start_tatolabd: "Callable[..., TatolabdUnderTest]", stream_name: str
) -> "tuple[TatolabdUnderTest, dict[str, Any], int]":
    """Start `tatolabd`, read the machine-wide graph from the moment its socket
    answers, and return the first rendering that holds `stream_name` with the
    count of renderings read before it.

    `tatolabd` serves before it re-loads its kept streams, and the watch reads
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
                f"`{stream_name}`:\n{tatolabd.recent_stderr()}"
            )
        try:
            machine_graph = local_api.graph()
        except (OSError, LocalApiRefusedTheRequest):
            machine_graph = None
        if machine_graph is not None:
            for stream_graph in machine_graph["streams"]:
                if stream_graph["stream"] == stream_name:
                    return tatolabd, stream_graph, renderings_without_the_stream
            renderings_without_the_stream += 1
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"tatolabd did not re-load `{stream_name}` within {STREAM_RUNNING_TIMEOUT_SECONDS}s:"
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
    tmp_path: Path,
):
    """No verb ever starts a runtime: each fails at the socket, naming it and
    how to start the runtime, and leaves the machine lock free for `tatolabd`."""
    project_directory = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM)
    local_api_socket_path = private_machine_directories.machine_directories.local_api_socket_path
    exchange_output_directory = tmp_path / "exchanged"
    every_verb_reaching_the_runtime = (
        ("run",),
        ("run", "-d"),
        ("run", "--dir", str(project_directory), "--name", "other"),
        ("dev",),
        ("stop", ALPHA_STREAM),
        ("start", ALPHA_STREAM),
        ("rm", ALPHA_STREAM),
        ("streams",),
        ("expose", ALPHA_STREAM, EXPOSED_NODE, EXPOSED_PORT),
        ("expose", ALPHA_STREAM, EXPOSED_NODE, EXPOSED_PORT, "--public"),
        ("expose", ALPHA_STREAM, EXPOSED_NODE, EXPOSED_PORT, "--remove"),
        ("graph",),
        ("graph", "--stream", ALPHA_STREAM),
        ("tap", f"machine/{EXPOSED_NODE}/{EXPOSED_PORT}", "--stream", ALPHA_STREAM),
        ("logs", "--stream", ALPHA_STREAM),
        ("logs", "--stream", ALPHA_STREAM, "-f"),
        ("exchange", "1#1", "--out", str(exchange_output_directory)),
        (
            "exchange",
            "--channel",
            f"machine/{EXPOSED_NODE}/{EXPOSED_PORT}",
            "--stream",
            ALPHA_STREAM,
            "--out",
            str(exchange_output_directory),
        ),
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


# --- a stopped kept stream, with no started stream --------------------------------


def write_the_record_run_d_and_stop_leave(
    private_machine_directories: PrivateMachineDirectories, project_directory: Path, stream_name: str
) -> "dict[str, Any]":
    """Write the kept-stream record `tatolab run -d` then `tatolab stop` leave for
    the project's sole stream, its graph compiled by the project's own
    interpreter as the runtime compiles it; return the record.

    Starting a stream needs a GPU context, so a GPU-free test cannot have the
    runtime write it; a stopped stream is never started again by a restart, so
    from here on the runtime treats it exactly as one it stopped itself.
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
        "schema_version": KEPT_STREAM_RECORD_SCHEMA_VERSION,
        "stream_name": stream_name,
        "project_directory": compile_document["project_directory"],
        "interpreter": str(project_interpreter),
        "stream_function": None,
        "graph": compile_document["stream_graph"],
        "stopped": True,
        "exposure_rulings": [],
    }
    record_path = kept_stream_record_path(private_machine_directories.machine_directories, stream_name)
    record_path.parent.mkdir(parents=True, mode=0o700)
    record_path.write_text(json.dumps(record), encoding="utf-8")
    record_path.chmod(KEPT_STREAM_RECORD_FILE_MODE)
    return record


def test_a_stopped_kept_stream_stays_stopped_across_restarts_takes_the_owners_ruling_and_rm_forgets_it(
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    project_directory = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM)
    record = write_the_record_run_d_and_stop_leave(
        private_machine_directories, project_directory, ALPHA_STREAM
    )
    assert record["graph"]["exposed"] == exposure_at("public"), record["graph"]
    stopped_listing = listing_of(ALPHA_STREAM, "stopped", Path(record["project_directory"]), None)

    tatolabd = start_tatolabd()
    tatolabd.await_stderr_containing(runtime_serving_log_line(0, 0))
    assert f"the kept stream `{ALPHA_STREAM}` is stopped, so it is not re-loaded" in tatolabd.stderr_text
    assert tatolabd.local_api_client().list_streams() == [stopped_listing]
    listed_table = assert_tatolab_succeeded(run_tatolab("streams", working_directory=project_directory))
    assert listed_table.splitlines()[0].split() == ["NAME", "STATE", "NODES", "PROJECT"]
    assert listed_table.splitlines()[1].split() == [
        ALPHA_STREAM,
        "stopped",
        "-",
        record["project_directory"],
    ]

    exposed = assert_tatolab_succeeded(
        run_tatolab(
            "expose", ALPHA_STREAM, EXPOSED_NODE, EXPOSED_PORT, working_directory=project_directory
        )
    )
    assert exposed == (
        f"{ALPHA_STREAM}/{EXPOSED_NODE}/{EXPOSED_PORT} is private (recorded; it holds across "
        f"restarts)\n"
    )
    refused_node = run_tatolab(
        "expose", ALPHA_STREAM, "no-such-node", EXPOSED_PORT, working_directory=project_directory
    )
    assert refused_node.returncode == 1, refused_node.stderr
    assert "holds no node `no-such-node`" in refused_node.stderr, refused_node.stderr
    owners_ruling = {"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": "private"}
    assert read_kept_stream_record(tatolabd, ALPHA_STREAM)["exposure_rulings"] == [owners_ruling]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(runtime_serving_log_line(0, 0))
    assert tatolabd.local_api_client().list_streams() == [stopped_listing]
    restarted_record = read_kept_stream_record(tatolabd, ALPHA_STREAM)
    assert restarted_record["stopped"] is True
    assert restarted_record["exposure_rulings"] == [owners_ruling]
    refused_stop = run_tatolab("stop", ALPHA_STREAM, working_directory=project_directory)
    assert refused_stop.returncode == 1, refused_stop.stderr
    assert "already stopped" in refused_stop.stderr, refused_stop.stderr

    removed = assert_tatolab_succeeded(run_tatolab("rm", ALPHA_STREAM, working_directory=project_directory))
    assert removed == f"{ALPHA_STREAM} removed (forgotten)\n"
    assert not kept_stream_record_path(tatolabd.machine_directories, ALPHA_STREAM).exists()
    assert assert_tatolab_succeeded(
        run_tatolab("streams", working_directory=project_directory)
    ) == "No streams in this runtime.\n"
    refused_rm = run_tatolab("rm", ALPHA_STREAM, working_directory=project_directory)
    assert refused_rm.returncode == 1, refused_rm.stderr

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(runtime_serving_log_line(0, 0))
    assert tatolabd.local_api_client().list_streams() == []
    stop_tatolabd_cleanly(tatolabd)


# --- with started streams ------------------------------------------------------


@pytest.mark.requires_gpu
def test_two_projects_kept_streams_run_come_back_after_a_restart_and_stop_start_rm_hold(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM)
    bravo_project = make_project_running_a_pattern_stream(make_tatolab_project, BRAVO_STREAM)
    tatolabd = start_tatolabd()

    for stream_name, project_directory in ((ALPHA_STREAM, alpha_project), (BRAVO_STREAM, bravo_project)):
        kept = assert_tatolab_succeeded(run_tatolab("run", "-d", working_directory=project_directory))
        assert kept == f"{stream_name} kept (project {project_directory})\n"
    alpha_kept = listing_of(ALPHA_STREAM, "kept", alpha_project, PROJECT_STREAM_NODE_COUNT)
    bravo_kept = listing_of(BRAVO_STREAM, "kept", bravo_project, PROJECT_STREAM_NODE_COUNT)

    def assert_both_kept_streams_run(tatolabd: TatolabdUnderTest) -> None:
        local_api = tatolabd.local_api_client()
        for stream_name in (ALPHA_STREAM, BRAVO_STREAM):
            local_api.await_every_node_running(
                stream=stream_name, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
            )
        assert listed_streams_by_name(local_api) == {
            ALPHA_STREAM: alpha_kept,
            BRAVO_STREAM: bravo_kept,
        }
        assert stream_names_in_the_machine_graph(local_api) == [ALPHA_STREAM, BRAVO_STREAM]
        for stream_name in (ALPHA_STREAM, BRAVO_STREAM):
            assert_frames_flow_out_of_the_pattern(local_api, run_tatolab, stream_name, alpha_project)

    assert_both_kept_streams_run(tatolabd)
    table_rows = [
        row.split()
        for row in assert_tatolab_succeeded(
            run_tatolab("streams", working_directory=alpha_project)
        ).splitlines()[1:]
    ]
    assert sorted(table_rows) == [
        [ALPHA_STREAM, "kept", str(PROJECT_STREAM_NODE_COUNT), str(alpha_project)],
        [BRAVO_STREAM, "kept", str(PROJECT_STREAM_NODE_COUNT), str(bravo_project)],
    ]
    for stream_name, project_directory in ((ALPHA_STREAM, alpha_project), (BRAVO_STREAM, bravo_project)):
        record = read_kept_stream_record(tatolabd, stream_name)
        assert (record["project_directory"], record["stopped"]) == (str(project_directory), False)

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(
        runtime_serving_log_line(2, 0), timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert_both_kept_streams_run(tatolabd)

    stopped = assert_tatolab_succeeded(run_tatolab("stop", ALPHA_STREAM, working_directory=alpha_project))
    assert stopped == (
        f"{ALPHA_STREAM} stopped; it stays stopped across restarts until "
        f"`tatolab start {ALPHA_STREAM}`\n"
    )
    alpha_stopped = listing_of(ALPHA_STREAM, "stopped", alpha_project, None)
    local_api = tatolabd.local_api_client()
    assert listed_streams_by_name(local_api) == {ALPHA_STREAM: alpha_stopped, BRAVO_STREAM: bravo_kept}
    assert stream_names_in_the_machine_graph(local_api) == [BRAVO_STREAM]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(
        runtime_serving_log_line(1, 0), timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert f"the kept stream `{ALPHA_STREAM}` is stopped, so it is not re-loaded" in tatolabd.stderr_text
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=BRAVO_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_name(local_api) == {ALPHA_STREAM: alpha_stopped, BRAVO_STREAM: bravo_kept}
    assert stream_names_in_the_machine_graph(local_api) == [BRAVO_STREAM]

    started = assert_tatolab_succeeded(run_tatolab("start", ALPHA_STREAM, working_directory=alpha_project))
    assert started == f"{ALPHA_STREAM} started ({PROJECT_STREAM_NODE_COUNT} nodes)\n"
    assert_both_kept_streams_run(tatolabd)
    assert read_kept_stream_record(tatolabd, ALPHA_STREAM)["stopped"] is False

    removed = assert_tatolab_succeeded(run_tatolab("rm", ALPHA_STREAM, working_directory=alpha_project))
    assert removed == f"{ALPHA_STREAM} removed (unloaded and forgotten)\n"
    assert not kept_stream_record_path(tatolabd.machine_directories, ALPHA_STREAM).exists()
    assert listed_streams_by_name(local_api) == {BRAVO_STREAM: bravo_kept}
    assert stream_names_in_the_machine_graph(local_api) == [BRAVO_STREAM]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(
        runtime_serving_log_line(1, 0), timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=BRAVO_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_name(local_api) == {BRAVO_STREAM: bravo_kept}
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_an_attached_run_unloads_on_ctrl_c_and_on_a_killed_cli_while_a_kept_stream_runs_on(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM)
    bravo_project = make_project_running_a_pattern_stream(make_tatolab_project, BRAVO_STREAM)
    tatolabd = start_tatolabd()
    assert_tatolab_succeeded(run_tatolab("run", "-d", working_directory=bravo_project))
    local_api = tatolabd.local_api_client()
    bravo_kept = listing_of(BRAVO_STREAM, "kept", bravo_project, PROJECT_STREAM_NODE_COUNT)
    alpha_attached = listing_of(ALPHA_STREAM, "attached", alpha_project, PROJECT_STREAM_NODE_COUNT)

    def attach_alpha() -> AttachedTatolabRun:
        attached = tatolabd.run_stream_attached(TatolabRunOfAProject(working_directory=alpha_project))
        loaded = attached.await_loaded(timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
        assert (loaded["stream_name"], int(loaded["node_count"])) == (
            ALPHA_STREAM,
            PROJECT_STREAM_NODE_COUNT,
        )
        local_api.await_every_node_running(stream=ALPHA_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
        assert listed_streams_by_name(local_api) == {
            ALPHA_STREAM: alpha_attached,
            BRAVO_STREAM: bravo_kept,
        }
        return attached

    attached_ended_by_ctrl_c = attach_alpha()
    exposed_live_only = assert_tatolab_succeeded(
        run_tatolab(
            "expose",
            ALPHA_STREAM,
            EXPOSED_NODE,
            EXPOSED_PORT,
            "--remove",
            working_directory=alpha_project,
        )
    )
    assert exposed_live_only == (
        f"{ALPHA_STREAM}/{EXPOSED_NODE}/{EXPOSED_PORT} is internal (live only: an attached stream "
        f"keeps no record)\n"
    )
    assert local_api.graph(ALPHA_STREAM)["exposed"] == exposure_at("internal")
    assert not kept_stream_record_path(tatolabd.machine_directories, ALPHA_STREAM).exists()
    attached_ended_by_ctrl_c.interrupt()
    assert attached_ended_by_ctrl_c.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == 0, (
        attached_ended_by_ctrl_c.recent_stderr()
    )
    tatolabd.await_stderr_containing(stream_stopped_log_line(ALPHA_STREAM))
    assert listed_streams_by_name(local_api) == {BRAVO_STREAM: bravo_kept}

    attached_ended_by_sigkill = attach_alpha()
    assert local_api.graph(ALPHA_STREAM)["exposed"] == exposure_at("public"), (
        "an attached stream's live level is never recorded: the next run takes the function's"
    )
    os.kill(attached_ended_by_sigkill.pid, signal.SIGKILL)
    assert attached_ended_by_sigkill.await_exit() == -signal.SIGKILL
    assert await_stream_unlisted(local_api, ALPHA_STREAM) == {BRAVO_STREAM: bravo_kept}
    tatolabd.await_stderr_containing(stream_stopped_log_line(ALPHA_STREAM), occurrence=2)
    local_api.await_every_node_running(stream=BRAVO_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert stream_names_in_the_machine_graph(local_api) == [BRAVO_STREAM]
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_expose_sets_a_running_streams_port_internal_private_or_public_live_and_nothing_restarts(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM)
    tatolabd = start_tatolabd()
    assert_tatolab_succeeded(run_tatolab("run", "-d", working_directory=alpha_project))
    local_api = tatolabd.local_api_client()
    running_graph = local_api.await_every_node_running(
        stream=ALPHA_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert running_graph["exposed"] == exposure_at("public")
    node_ids_while_running = sorted(node["id"] for node in running_graph["nodes"])
    start_line = f"[start] Starting the stream `{ALPHA_STREAM}`"
    assert tatolabd.stderr_text.count(start_line) == 1, tatolabd.recent_stderr()

    for flags, level in (((), "private"), (("--remove",), "internal"), (("--public",), "public")):
        exposed = assert_tatolab_succeeded(
            run_tatolab(
                "expose",
                ALPHA_STREAM,
                EXPOSED_NODE,
                EXPOSED_PORT,
                *flags,
                working_directory=alpha_project,
            )
        )
        assert exposed == (
            f"{ALPHA_STREAM}/{EXPOSED_NODE}/{EXPOSED_PORT} is {level} (recorded; it holds across "
            f"restarts)\n"
        )
        graph_right_after = local_api.graph(ALPHA_STREAM)
        assert graph_right_after["exposed"] == exposure_at(level), (level, graph_right_after["exposed"])
        assert sorted(node["id"] for node in graph_right_after["nodes"]) == node_ids_while_running, (
            "expose must change the level live, never reload the stream"
        )
        cli_graph = json.loads(
            assert_tatolab_succeeded(
                run_tatolab("graph", "--stream", ALPHA_STREAM, working_directory=alpha_project)
            )
        )
        assert cli_graph["exposed"] == exposure_at(level), (level, cli_graph["exposed"])
        assert read_kept_stream_record(tatolabd, ALPHA_STREAM)["exposure_rulings"] == [
            {"node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": level}
        ]

    local_api.await_every_node_running(stream=ALPHA_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert tatolabd.stderr_text.count(start_line) == 1, (
        f"expose restarted the stream:\n{tatolabd.recent_stderr()}"
    )
    assert stream_stopped_log_line(ALPHA_STREAM) not in tatolabd.stderr_text
    assert listed_streams_by_name(local_api) == {
        ALPHA_STREAM: listing_of(ALPHA_STREAM, "kept", alpha_project, PROJECT_STREAM_NODE_COUNT)
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
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM)
    tatolabd = start_tatolabd()
    assert_tatolab_succeeded(run_tatolab("run", "-d", working_directory=alpha_project))
    tatolabd.local_api_client().await_every_node_running(
        stream=ALPHA_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    assert_tatolab_succeeded(
        run_tatolab("expose", ALPHA_STREAM, EXPOSED_NODE, EXPOSED_PORT, working_directory=alpha_project)
    )
    record = read_kept_stream_record(tatolabd, ALPHA_STREAM)
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
        start_tatolabd, ALPHA_STREAM
    )
    assert_the_first_rendering_carries(first_rendering, renderings_before_it, "private")
    local_api = tatolabd.local_api_client()
    assert local_api.await_every_node_running(
        stream=ALPHA_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )["exposed"] == exposure_at("private")
    assert "kept and not applied" not in tatolabd.stderr_text, tatolabd.recent_stderr()

    assert_tatolab_succeeded(
        run_tatolab(
            "expose",
            ALPHA_STREAM,
            EXPOSED_NODE,
            EXPOSED_PORT,
            "--remove",
            working_directory=alpha_project,
        )
    )
    assert local_api.graph(ALPHA_STREAM)["exposed"] == exposure_at("internal")
    crash_tatolabd(tatolabd)
    tatolabd, first_rendering, renderings_before_it = first_rendering_of_the_stream_once_tatolabd_serves(
        start_tatolabd, ALPHA_STREAM
    )
    assert_the_first_rendering_carries(first_rendering, renderings_before_it, "internal")
    local_api = tatolabd.local_api_client()
    assert local_api.await_every_node_running(
        stream=ALPHA_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )["exposed"] == exposure_at("internal")
    assert listed_streams_by_name(local_api) == {
        ALPHA_STREAM: listing_of(ALPHA_STREAM, "kept", alpha_project, PROJECT_STREAM_NODE_COUNT)
    }
    assert read_kept_stream_record(tatolabd, ALPHA_STREAM)["graph"]["exposed"] == exposure_at("public")
    assert_frames_flow_out_of_the_pattern(local_api, run_tatolab, ALPHA_STREAM, alpha_project)
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_an_agent_does_all_of_it_over_tatolab_mcp(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    start_mcp_session_over_the_verb: "Callable[[], McpJsonRpcClientOverTheVerb]",
):
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM)
    bravo_project = make_project_running_a_pattern_stream(make_tatolab_project, BRAVO_STREAM)
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()
    alpha_kept = listing_of(ALPHA_STREAM, "kept", alpha_project, PROJECT_STREAM_NODE_COUNT)
    bravo_kept = listing_of(BRAVO_STREAM, "kept", bravo_project, PROJECT_STREAM_NODE_COUNT)

    def listed_through(agent: McpJsonRpcClientOverTheVerb) -> "dict[str, dict[str, Any]]":
        return {listed["name"]: listed for listed in agent.call_tool("list_streams")["streams"]}

    def run_stream(
        agent: McpJsonRpcClientOverTheVerb, project_directory: Path, *, keep: bool
    ) -> "dict[str, Any]":
        return agent.call_tool(
            "run_stream", {"project_directory": str(project_directory), "keep": keep}
        )

    # Two projects kept, both running; the owner restricts one port live.
    agent = start_mcp_session_over_the_verb()
    for stream_name, project_directory in ((ALPHA_STREAM, alpha_project), (BRAVO_STREAM, bravo_project)):
        assert run_stream(agent, project_directory, keep=True) == {
            "stream": stream_name,
            "kept": True,
            "project_directory": str(project_directory),
            "node_count": PROJECT_STREAM_NODE_COUNT,
            "replaced_the_kept_record": False,
        }
        local_api.await_every_node_running(stream=stream_name, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_through(agent) == {ALPHA_STREAM: alpha_kept, BRAVO_STREAM: bravo_kept}
    assert sorted(
        stream_graph["stream"] for stream_graph in agent.call_tool("graph")["streams"]
    ) == [ALPHA_STREAM, BRAVO_STREAM]
    assert agent.call_tool(
        "expose_port",
        {"stream": ALPHA_STREAM, "node": EXPOSED_NODE, "port": EXPOSED_PORT, "level": "private"},
    ) == {
        "stream": ALPHA_STREAM,
        "node": EXPOSED_NODE,
        "port": EXPOSED_PORT,
        "level": "private",
        "recorded": True,
    }
    assert agent.call_tool("graph", {"stream": ALPHA_STREAM})["exposed"] == exposure_at("private")
    assert agent.call_tool("stop_stream", {"stream": BRAVO_STREAM}) == {
        "stream": BRAVO_STREAM,
        "stopped": True,
        "kept": True,
    }
    assert agent.close_standard_input() == 0, agent.standard_error_text
    assert listed_streams_by_name(local_api) == {
        ALPHA_STREAM: alpha_kept,
        BRAVO_STREAM: listing_of(BRAVO_STREAM, "stopped", bravo_project, None),
    }, "a kept stream outlives the connection that ran it"

    # Across a restart: the kept stream comes back with the owner's level over
    # the function's public, the stopped one stays stopped.
    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    tatolabd.await_stderr_containing(
        runtime_serving_log_line(1, 0), timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=ALPHA_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    agent = start_mcp_session_over_the_verb()
    assert listed_through(agent) == {
        ALPHA_STREAM: alpha_kept,
        BRAVO_STREAM: listing_of(BRAVO_STREAM, "stopped", bravo_project, None),
    }
    assert agent.call_tool("graph", {"stream": ALPHA_STREAM})["exposed"] == exposure_at("private")
    assert agent.call_tool("start_stream", {"stream": BRAVO_STREAM}) == {
        "stream": BRAVO_STREAM,
        "node_count": PROJECT_STREAM_NODE_COUNT,
    }
    local_api.await_every_node_running(stream=BRAVO_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_through(agent) == {ALPHA_STREAM: alpha_kept, BRAVO_STREAM: bravo_kept}
    assert agent.call_tool("remove_stream", {"stream": BRAVO_STREAM}) == {
        "stream": BRAVO_STREAM,
        "unloaded": True,
        "forgotten": True,
    }
    assert listed_through(agent) == {ALPHA_STREAM: alpha_kept}
    assert not kept_stream_record_path(tatolabd.machine_directories, BRAVO_STREAM).exists()

    # Attached to the agent's connection: gone once its stdin closes.
    assert run_stream(agent, bravo_project, keep=False)["kept"] is False
    local_api.await_every_node_running(stream=BRAVO_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_through(agent)[BRAVO_STREAM] == listing_of(
        BRAVO_STREAM, "attached", bravo_project, PROJECT_STREAM_NODE_COUNT
    )
    assert agent.close_standard_input() == 0, agent.standard_error_text
    assert await_stream_unlisted(local_api, BRAVO_STREAM) == {ALPHA_STREAM: alpha_kept}

    # Attached to a connection whose host crashed: gone once the verb is killed.
    crashing_agent = start_mcp_session_over_the_verb()
    assert run_stream(crashing_agent, bravo_project, keep=False)["kept"] is False
    local_api.await_every_node_running(stream=BRAVO_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert crashing_agent.kill() == -signal.SIGKILL
    assert await_stream_unlisted(local_api, BRAVO_STREAM) == {ALPHA_STREAM: alpha_kept}
    assert not kept_stream_record_path(tatolabd.machine_directories, BRAVO_STREAM).exists()

    local_api.await_every_node_running(stream=ALPHA_STREAM, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    stop_tatolabd_cleanly(tatolabd)
