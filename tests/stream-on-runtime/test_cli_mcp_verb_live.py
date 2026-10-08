# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab mcp` against a stream on `tatolabd`, launched the way an MCP host launches it.

The verb is the runtime unit's `bin/tatolab mcp`. The client is the official
MCP Python SDK's stdio client: it spawns the verb and speaks over the verb's
stdin and stdout, exactly as a host configured to run the verb does.
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

from conftest import PrivateRuntimeDirectories
from runtime_process_under_test import ENGINE_STARTED_LOG_LINE, RuntimeProcessUnderTest
from runtime_unit_under_test import RuntimeUnitUnderTest
from test_cli_launch import NODE_READY_TIMEOUT_SECONDS, STREAM_WITH_ONE_NATIVE_SOURCE

pytestmark = pytest.mark.requires_gpu

#: Closing stdin with nothing in flight ends the verb as soon as the node closes its side.
VERB_EXIT_AFTER_STDIN_CLOSES_TIMEOUT_SECONDS = 5.0
VERB_EXIT_AFTER_THE_NODE_DIES_TIMEOUT_SECONDS = 10.0
MCP_SESSION_TIMEOUT_SECONDS = 60.0

CONTROL_TOOL_NAMES = {
    "graph",
    "tap",
    "logs",
    "exchange",
    "shutdown",
    "add_node",
    "remove_node",
    "connect",
    "disconnect",
}
NODE_CATALOG_RESOURCE_URI = "streamlib://node-catalog"
LIVE_GRAPH_RESOURCE_URI = "streamlib://graph"


def mcp_verb_command(runtime_unit: RuntimeUnitUnderTest, runtime_name: str) -> "list[str]":
    return [str(runtime_unit.tatolab_executable), "mcp", "--node", runtime_name]


def launch_a_ready_node(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
) -> "tuple[RuntimeProcessUnderTest, str]":
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    tatolab = start_tatolab("run", working_directory=app_directory)
    entry = tatolab.registry_entry(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    return tatolab, entry["runtime_name"]


def start_the_verb(
    runtime_unit: RuntimeUnitUnderTest, environment: "dict[str, str]", runtime_name: str
) -> "subprocess.Popen[bytes]":
    return subprocess.Popen(
        mcp_verb_command(runtime_unit, runtime_name),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
    )


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_stdio_client_through_the_verb_reaches_the_nodes_tools_resources_and_prompts(
    runtime_unit: RuntimeUnitUnderTest,
    private_runtime_directories: PrivateRuntimeDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    _tatolab, runtime_name = launch_a_ready_node(make_tatolab_project, start_tatolab)
    verb_command = mcp_verb_command(runtime_unit, runtime_name)
    verb = StdioServerParameters(
        command=verb_command[0],
        args=verb_command[1:],
        env=private_runtime_directories.environment,
    )

    async def session_through_the_verb() -> "dict[str, Any]":
        async with Client(verb) as client:
            tools = await client.list_tools()
            graph_result = await client.call_tool("graph", {})
            graph_text = graph_result.content[0]
            assert isinstance(graph_text, TextContent), graph_result.content
            graph = json.loads(graph_text.text)
            source_name = next(
                graph_node["name"]
                for graph_node in graph["nodes"]
                if graph_node["name"].startswith("testpatternsource")
            )
            resources = {
                uri: (await client.read_resource(uri)).contents
                for uri in (NODE_CATALOG_RESOURCE_URI, LIVE_GRAPH_RESOURCE_URI)
            }
            prompt = await client.get_prompt(
                "look_at_what_a_channel_carries",
                {"from_node": source_name, "from_port": "video"},
            )
            return {
                "protocol_version": client.protocol_version,
                "tool_names": {tool.name for tool in tools.tools},
                "graph": graph,
                "source_name": source_name,
                "resources": resources,
                "prompt": prompt,
            }

    answered = asyncio.run(
        asyncio.wait_for(session_through_the_verb(), MCP_SESSION_TIMEOUT_SECONDS)
    )

    assert answered["protocol_version"] == LATEST_PROTOCOL_VERSION
    assert answered["tool_names"] == CONTROL_TOOL_NAMES
    assert answered["graph"]["runtime_name"] == runtime_name
    for uri, contents in answered["resources"].items():
        (document,) = contents
        assert isinstance(document, TextResourceContents), (uri, document)
        json.loads(document.text)
    (prompt_message,) = answered["prompt"].messages
    assert isinstance(prompt_message.content, TextContent), prompt_message
    assert answered["source_name"] in prompt_message.content.text


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_closing_stdin_ends_the_verb_promptly(
    runtime_unit: RuntimeUnitUnderTest,
    private_runtime_directories: PrivateRuntimeDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    _tatolab, runtime_name = launch_a_ready_node(make_tatolab_project, start_tatolab)
    verb = start_the_verb(runtime_unit, private_runtime_directories.environment, runtime_name)

    started = time.monotonic()
    stdout, stderr = verb.communicate(b"", timeout=VERB_EXIT_AFTER_STDIN_CLOSES_TIMEOUT_SECONDS)

    assert verb.returncode == 0, stderr.decode()
    assert stdout == b""
    assert time.monotonic() - started < VERB_EXIT_AFTER_STDIN_CLOSES_TIMEOUT_SECONDS


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_the_node_dying_ends_the_verb_non_zero_with_one_line_naming_the_runtime(
    runtime_unit: RuntimeUnitUnderTest,
    private_runtime_directories: PrivateRuntimeDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    tatolab, runtime_name = launch_a_ready_node(make_tatolab_project, start_tatolab)
    verb = start_the_verb(runtime_unit, private_runtime_directories.environment, runtime_name)
    # Wait until the verb holds an open stream, so the kill lands on a live pipe.
    assert verb.stdin is not None
    verb.stdin.write(b'{"jsonrpc":"2.0","id":1,"method":"ping"}\n')
    verb.stdin.flush()
    assert verb.stdout is not None
    assert json.loads(verb.stdout.readline())["id"] == 1

    tatolab.kill_every_process_it_started()
    try:
        verb.wait(timeout=VERB_EXIT_AFTER_THE_NODE_DIES_TIMEOUT_SECONDS)
    finally:
        verb.kill()
    assert verb.stderr is not None
    stderr_lines = verb.stderr.read().decode().splitlines()

    assert verb.returncode not in (0, None)
    assert len(stderr_lines) == 1, stderr_lines
    assert runtime_name in stderr_lines[0]
