# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`tatolabd` hosts the machine's streams, and every verb that manages them
reaches it through the real CLI.

Each test drives the runtime unit's own `tatolabd` and `tatolab` under the
test's machine root, as a user at a terminal does: every verb with no runtime,
`describe_runtime` across a restart, and an agent doing it all over
`tatolab mcp` with a JSON-RPC client speaking over the verb's stdin and stdout.

A started stream needs a GPU context, so the tests that start one are
`requires_gpu`; their streams are a test pattern into a Python node that reads
its frames, so no camera and no display is needed.
"""

from __future__ import annotations

import json
import queue
import signal
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

from conftest import (
    PrivateMachineDirectories,
    TatolabdUnderTest,
)
from local_api_client import (
    LocalApiClient,
    LocalApiRefusedTheRequest,
    call_operation_tool_call,
)
from runtime_process_under_test import ENGINE_GRACEFUL_STOP_LOG_LINE
from runtime_unit_under_test import RuntimeUnitUnderTest
from tatolabd_pattern_stream_projects import (
    ALPHA_STREAM_FUNCTION,
    BRAVO_STREAM_FUNCTION,
    PROJECT_STREAM_NODE_COUNT,
    STREAM_RUNNING_TIMEOUT_SECONDS,
    TATOLABD_EXIT_TIMEOUT_SECONDS,
    await_the_reloads_queued_at_the_start,
    listed_streams_by_id,
    listing_of,
    make_project_running_a_pattern_stream,
    restart_tatolabd,
    stop_tatolabd_cleanly,
    stream_id_of_the_pattern_project,
    stream_record_path,
)


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


#: How every verb refuses when nothing answers at the socket, before it names the socket.
NO_RUNTIME_REFUSAL_PREFIX = "no runtime is running on this machine: nothing answers at "
NO_RUNTIME_REFUSAL_REMEDY = "Start one by opening Tato Studio, or by running `tatolabd` in a terminal."


def stream_stopped_log_line(stream_id: str) -> str:
    """The engine's line once a stream's graceful stop has finished."""
    return f"{ENGINE_GRACEFUL_STOP_LOG_LINE}{stream_id}` stopped"


def crash_tatolabd(tatolabd: TatolabdUnderTest) -> None:
    """SIGKILL `tatolabd` alone, as a crash ends it: no teardown, its socket file left behind."""
    tatolabd.send_signal(signal.SIGKILL)
    assert tatolabd.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == -signal.SIGKILL
    assert tatolabd.local_api_socket_path.exists(), "a crash leaves the socket file behind"


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
        """The operation `tool_name` run through `call_operation`, its text content parsed as JSON.

        A tool error raises `McpToolCallRefused`.
        """
        call_operation, call_operation_arguments = call_operation_tool_call(tool_name, arguments)
        result = self._request(
            "tools/call", {"name": call_operation, "arguments": call_operation_arguments}
        )
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
        ("open",),
        ("open", "--play"),
        ("play",),
        ("close",),
        ("reload", "--dir", str(project_directory)),
        ("dev",),
        ("stop",),
        ("stop", "--dir", str(project_directory)),
        ("rm",),
        ("rm", "--dir", str(project_directory)),
        ("streams",),
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

    def the_one_stream_of_the_project_action_once_done(
        agent: McpJsonRpcClientOverTheVerb, project_directory: Path, tool_name: str
    ) -> "dict[str, Any]":
        """The one stream the action `tool_name` queued of `project_directory` took in, once
        `describe_stream_action` says the action is done."""
        queued = agent.call_tool(tool_name, {"project_directory": str(project_directory)})
        assert queued["project_directory"] == str(project_directory), queued
        deadline = time.monotonic() + STREAM_RUNNING_TIMEOUT_SECONDS
        while (
            described := agent.call_tool("describe_stream_action", {"action": queued["action"]})
        )["state"] not in ("done", "failed"):
            assert time.monotonic() < deadline, described
            time.sleep(0.1)
        assert described["state"] == "done", described
        [stream] = described["streams"]
        return stream

    def open_and_play_project(
        agent: McpJsonRpcClientOverTheVerb, project_directory: Path
    ) -> "dict[str, Any]":
        """The one stream of `project_directory` once `open_project` holds it stopped and
        `start_playback` plays it."""
        opened = the_one_stream_of_the_project_action_once_done(agent, project_directory, "open_project")
        assert opened["state"] == "stopped", opened
        return the_one_stream_of_the_project_action_once_done(agent, project_directory, "start_playback")

    # Two projects run, both running.
    agent = start_mcp_session_over_the_verb()
    for stream_id, stream_function_name, project_directory in (
        (alpha_stream_id, ALPHA_STREAM_FUNCTION, alpha_project),
        (bravo_stream_id, BRAVO_STREAM_FUNCTION, bravo_project),
    ):
        assert open_and_play_project(agent, project_directory) == {
            "stream": stream_id,
            "display_name": stream_function_name,
            "state": "running",
        }
        local_api.await_every_node_running(stream=stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_through(agent) == {alpha_stream_id: alpha_running, bravo_stream_id: bravo_running}
    assert sorted(
        stream_graph["stream"] for stream_graph in agent.call_tool("graph")["streams"]
    ) == [alpha_stream_id, bravo_stream_id]
    alpha_start_line = f"[start] Starting the stream `{alpha_stream_id}`"
    assert tatolabd.stderr_text.count(alpha_start_line) == 1, tatolabd.recent_stderr()
    assert stream_stopped_log_line(alpha_stream_id) not in tatolabd.stderr_text
    bravo_stopped = listing_of(
        bravo_stream_id, BRAVO_STREAM_FUNCTION, "stopped", bravo_project, PROJECT_STREAM_NODE_COUNT
    )
    bravo_stop = agent.call_tool("stop_playback", {"project_directory": str(bravo_project)})
    assert bravo_stop["project_directory"] == str(bravo_project), bravo_stop
    assert local_api.stream_action_once_ended(bravo_stop["action"])["state"] == "done"
    assert agent.close_standard_input() == 0, agent.standard_error_text
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: bravo_stopped,
    }, "every stream outlives the connection that ran it"

    # Across a crash's restart: the running stream comes back, and the stopped
    # one is held stopped.
    crash_tatolabd(tatolabd)
    tatolabd, _, _ = first_rendering_of_the_stream_once_tatolabd_serves(
        start_tatolabd, alpha_stream_id
    )
    await_the_reloads_queued_at_the_start(tatolabd, 2)
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    agent = start_mcp_session_over_the_verb()
    assert listed_through(agent) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: bravo_stopped,
    }
    bravo_start = agent.call_tool("start_playback", {"project_directory": str(bravo_project)})
    assert bravo_start["project_directory"] == str(bravo_project), bravo_start
    bravo_start_once_ended = local_api.stream_action_once_ended(bravo_start["action"])
    assert [stream["state"] for stream in bravo_start_once_ended["streams"]] == ["running"], (
        bravo_start_once_ended
    )
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_through(agent) == {alpha_stream_id: alpha_running, bravo_stream_id: bravo_running}
    bravo_removal = agent.call_tool("remove_project", {"project_directory": str(bravo_project)})
    assert bravo_removal["project_directory"] == str(bravo_project), bravo_removal
    assert local_api.stream_action_once_ended(bravo_removal["action"])["state"] == "done"
    assert listed_through(agent) == {alpha_stream_id: alpha_running}
    assert not stream_record_path(tatolabd.machine_directories, bravo_stream_id).exists()

    # Run again by the agent, then the agent quits: the stream runs on, recorded.
    assert open_and_play_project(agent, bravo_project) == {
        "stream": bravo_stream_id,
        "display_name": BRAVO_STREAM_FUNCTION,
        "state": "running",
    }
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
    assert the_one_stream_of_the_project_action_once_done(
        crashing_agent, bravo_project, "reload_project"
    ) == {
        "stream": bravo_stream_id,
        "display_name": BRAVO_STREAM_FUNCTION,
        "state": "running",
    }, "the run replaces the recorded stream under its id"
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
