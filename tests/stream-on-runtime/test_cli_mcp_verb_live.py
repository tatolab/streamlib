# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab mcp` against the machine's runtime, launched the way an MCP host launches it.

The verb is the runtime unit's `bin/tatolab mcp`, which takes no flags: the
machine has one runtime, at one socket. The client is the official MCP Python
SDK's stdio client: it spawns the verb and speaks over the verb's stdin and
stdout, exactly as a host configured to run the verb does.
"""

from __future__ import annotations

import asyncio
import json
import subprocess
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from mcp.client.client import Client
from mcp.client.stdio import StdioServerParameters
from mcp.types import TextContent, TextResourceContents
from mcp_types.version import LATEST_PROTOCOL_VERSION

from conftest import PrivateMachineDirectories, TatolabdUnderTest
from runtime_unit_under_test import RuntimeUnitUnderTest
from stream_runs_on_tatolabd import TatolabRunOfAProject
from test_cli_launch import NODE_READY_TIMEOUT_SECONDS, STREAM_WITH_ONE_NATIVE_SOURCE

pytestmark = pytest.mark.requires_gpu

#: Closing stdin with nothing in flight ends the verb as soon as the runtime closes its side.
VERB_EXIT_AFTER_STDIN_CLOSES_TIMEOUT_SECONDS = 5.0
VERB_EXIT_AFTER_THE_RUNTIME_DIES_TIMEOUT_SECONDS = 10.0
MCP_SESSION_TIMEOUT_SECONDS = 60.0

CONTROL_TOOL_NAMES = {
    "graph",
    "logs",
    "add_node",
    "remove_node",
    "connect",
    "disconnect",
    "run_project",
    "describe_stream_action",
    "cancel_stream_action",
    "list_project_streams",
    "stop_stream",
    "start_stream",
    "remove_project",
    "rename_stream",
    "list_streams",
    "expose_port",
    "add_relay",
    "configure_relay",
    "remove_relay",
    "list_relays",
    "describe_runtime",
}
NODE_CATALOG_RESOURCE_URI = "streamlib://node-catalog"
LIVE_GRAPH_RESOURCE_URI = "streamlib://graph"
RELAY_ADAPTER_CATALOG_RESOURCE_URI = "streamlib://relay-adapter-catalog"


def mcp_verb_command(runtime_unit: RuntimeUnitUnderTest) -> "list[str]":
    return [str(runtime_unit.tatolab_executable), "mcp"]


def a_runtime_running_one_stream(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
) -> TatolabdUnderTest:
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    tatolabd = start_tatolabd()
    tatolabd.run_stream(TatolabRunOfAProject(working_directory=app_directory)).await_loaded(
        timeout=NODE_READY_TIMEOUT_SECONDS
    )
    return tatolabd


def start_the_verb(
    runtime_unit: RuntimeUnitUnderTest, environment: "dict[str, str]"
) -> "subprocess.Popen[bytes]":
    return subprocess.Popen(
        mcp_verb_command(runtime_unit),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
    )


def test_a_stdio_client_through_the_verb_reaches_the_runtimes_tools_resources_and_prompts(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = a_runtime_running_one_stream(make_tatolab_project, start_tatolabd)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    verb_command = mcp_verb_command(runtime_unit)
    verb = StdioServerParameters(
        command=verb_command[0],
        args=verb_command[1:],
        env=private_machine_directories.environment,
    )

    async def session_through_the_verb() -> "dict[str, Any]":
        async with Client(verb) as client:
            tools = await client.list_tools()
            graph_result = await client.call_tool("graph", {})
            graph_text = graph_result.content[0]
            assert isinstance(graph_text, TextContent), graph_result.content
            machine_graph = json.loads(graph_text.text)
            (stream_graph,) = machine_graph["streams"]
            source_name = next(
                graph_node["name"]
                for graph_node in stream_graph["nodes"]
                if graph_node["name"].startswith("testpatternsource")
            )
            resources = {
                uri: (await client.read_resource(uri)).contents
                for uri in (
                    NODE_CATALOG_RESOURCE_URI,
                    LIVE_GRAPH_RESOURCE_URI,
                    RELAY_ADAPTER_CATALOG_RESOURCE_URI,
                )
            }
            prompt = await client.get_prompt(
                "fan_output_to_another_consumer",
                {
                    "stream": stream_id,
                    "from_node": source_name,
                    "from_port": "video",
                    "type": "nodes.effect:Effect",
                },
            )
            return {
                "protocol_version": client.protocol_version,
                "tool_names": {tool.name for tool in tools.tools},
                "machine_graph": machine_graph,
                "stream_graph": stream_graph,
                "source_name": source_name,
                "resources": resources,
                "prompt": prompt,
            }

    answered = asyncio.run(
        asyncio.wait_for(session_through_the_verb(), MCP_SESSION_TIMEOUT_SECONDS)
    )

    assert answered["protocol_version"] == LATEST_PROTOCOL_VERSION
    assert answered["tool_names"] == CONTROL_TOOL_NAMES
    assert answered["stream_graph"]["stream"] == stream_id
    assert answered["machine_graph"]["machine"], answered["machine_graph"]
    for uri, contents in answered["resources"].items():
        (document,) = contents
        assert isinstance(document, TextResourceContents), (uri, document)
        json.loads(document.text)
    (prompt_message,) = answered["prompt"].messages
    assert isinstance(prompt_message.content, TextContent), prompt_message
    assert answered["source_name"] in prompt_message.content.text


def test_closing_stdin_ends_the_verb_promptly(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    a_runtime_running_one_stream(make_tatolab_project, start_tatolabd)
    verb = start_the_verb(runtime_unit, private_machine_directories.environment)

    started = time.monotonic()
    stdout, stderr = verb.communicate(b"", timeout=VERB_EXIT_AFTER_STDIN_CLOSES_TIMEOUT_SECONDS)

    assert verb.returncode == 0, stderr.decode()
    assert stdout == b""
    assert time.monotonic() - started < VERB_EXIT_AFTER_STDIN_CLOSES_TIMEOUT_SECONDS


def test_the_runtime_dying_ends_the_verb_non_zero_with_one_line_naming_its_socket(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = a_runtime_running_one_stream(make_tatolab_project, start_tatolabd)
    verb = start_the_verb(runtime_unit, private_machine_directories.environment)
    # Wait until the verb holds an open stream, so the kill lands on a live pipe.
    assert verb.stdin is not None
    verb.stdin.write(b'{"jsonrpc":"2.0","id":1,"method":"ping"}\n')
    verb.stdin.flush()
    assert verb.stdout is not None
    assert json.loads(verb.stdout.readline())["id"] == 1

    tatolabd.kill_every_process_it_started()
    try:
        verb.wait(timeout=VERB_EXIT_AFTER_THE_RUNTIME_DIES_TIMEOUT_SECONDS)
    finally:
        verb.kill()
    assert verb.stderr is not None
    stderr_lines = verb.stderr.read().decode().splitlines()

    assert verb.returncode not in (0, None)
    assert len(stderr_lines) == 1, stderr_lines
    assert str(tatolabd.local_api_socket_path) in stderr_lines[0]
