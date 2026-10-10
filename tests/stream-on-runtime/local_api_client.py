# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A client of the machine's runtime's local API, from the suite venv alone.

Nothing here imports `tatolab.runtime`: the plain HTTP routes (`/health`,
`/api/graph`, `/api/registry`) are read with the standard library over the Unix
socket, and MCP is the official MCP Python SDK's streamable-HTTP client over the
same socket. One runtime holds every stream loaded into it, so a route or wait
about one stream's graph names that stream; `/api/graph` without one answers
`{"runtime_name", "streams": [...]}`. A graph's nodes carry their state at
`components["state"]`, its links at `state`.
"""

from __future__ import annotations

import asyncio
import http.client
import json
import socket
import time
import urllib.parse
from collections.abc import Awaitable, Callable, Iterable
from pathlib import Path
from typing import Any, TypeVar

import httpx2
from mcp.client.client import Client
from mcp.client.streamable_http import streamable_http_client
from mcp.shared.exceptions import MCPError
from mcp.types import TextContent

#: The MCP endpoint; it fills `Host`, and the socket path is the address.
LOCAL_API_MCP_URL = "http://localhost/mcp"

LOCAL_API_REQUEST_TIMEOUT_SECONDS = 30.0
EVERY_NODE_RUNNING_TIMEOUT_SECONDS = 90.0
LINK_STATE_TIMEOUT_SECONDS = 15.0
GRAPH_POLL_INTERVAL_SECONDS = 0.05

RUNNING_NODE_STATE = "Running"
#: The states a node passes through before its `setup` has run, as the engine renders them.
NODE_STATES_BEFORE_SETUP_COMPLETED = ("Pending", "Idle")
LINK_ERROR_STATE = "error"

McpAnswer = TypeVar("McpAnswer")


class LocalApiRefusedTheRequest(Exception):
    """A local API route answered with a status other than 200."""


def mcp_error_raised_in(raised: BaseException) -> MCPError:
    """The protocol error an MCP client raised, out of any exception group anyio wrapped it in."""
    if isinstance(raised, MCPError):
        return raised
    for inner in getattr(raised, "exceptions", ()):
        try:
            return mcp_error_raised_in(inner)
        except AssertionError:
            continue
    raise AssertionError(f"no protocol error in {raised!r}")


class _HttpConnectionOverUnixSocket(http.client.HTTPConnection):
    def __init__(self, local_api_socket_path: Path, timeout: float) -> None:
        super().__init__("localhost", timeout=timeout)
        self._local_api_socket_path = local_api_socket_path

    def connect(self) -> None:
        unix_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        unix_socket.settimeout(self.timeout)
        unix_socket.connect(str(self._local_api_socket_path))
        self.sock = unix_socket


class LocalApiClient:
    """The machine's runtime's local API, reached at its Unix socket."""

    def __init__(self, local_api_socket_path: Path) -> None:
        self.local_api_socket_path = Path(local_api_socket_path)

    def get_text(self, route: str) -> str:
        """GET `route` and return its body as text; any status but 200 raises."""
        connection = _HttpConnectionOverUnixSocket(
            self.local_api_socket_path, LOCAL_API_REQUEST_TIMEOUT_SECONDS
        )
        try:
            connection.request("GET", route)
            response = connection.getresponse()
            body = response.read().decode(errors="replace")
        finally:
            connection.close()
        if response.status != 200:
            raise LocalApiRefusedTheRequest(f"GET {route} answered {response.status}: {body}")
        return body

    def get_json(self, route: str) -> Any:
        """GET `route` and return its JSON body; any status but 200 raises."""
        return json.loads(self.get_text(route))

    def health(self) -> str:
        """`GET /health`: `ok` from a local API that answers."""
        return self.get_text("/health")

    def graph(self, stream: "str | None" = None) -> "dict[str, Any]":
        """`GET /api/graph`: the stream's live graph — its nodes, links, exposures
        and the runtime's name — or, with no stream, `{"runtime_name", "streams"}`."""
        if stream is None:
            return self.get_json("/api/graph")
        return self.get_json(f"/api/graph?stream={urllib.parse.quote(stream, safe='')}")

    def list_streams(self) -> "list[dict[str, Any]]":
        """MCP `list_streams`: each stream the runtime holds, `{name, state, project_directory, node_count}`."""
        return self.call_tool("list_streams")["streams"]

    def registry(self, stream: "str | None" = None) -> Any:
        """`GET /api/registry`: the node types the stream can add — the native
        ones, then those described in its own interpreter — or, with no stream,
        `{"nodes", "streams"}`: the native types, then each loaded stream's own."""
        if stream is None:
            return self.get_json("/api/registry")
        return self.get_json(f"/api/registry?stream={urllib.parse.quote(stream, safe='')}")

    def answer_over_mcp(
        self,
        operation: "Callable[[Client], Awaitable[McpAnswer]]",
        *,
        mode: str = "auto",
    ) -> McpAnswer:
        """Run one operation on an MCP client connected for it alone."""

        async def connected_operation() -> McpAnswer:
            async with httpx2.AsyncClient(
                transport=httpx2.AsyncHTTPTransport(uds=str(self.local_api_socket_path)),
                timeout=LOCAL_API_REQUEST_TIMEOUT_SECONDS,
            ) as http_client, Client(
                streamable_http_client(LOCAL_API_MCP_URL, http_client=http_client), mode=mode
            ) as client:
                return await operation(client)

        return asyncio.run(connected_operation())

    def call_tool(self, tool_name: str, arguments: "dict[str, Any] | None" = None) -> Any:
        """MCP `tools/call`, its first text content parsed as JSON; a tool error raises."""
        result = self.answer_over_mcp(
            lambda client: client.call_tool(tool_name, arguments or {})
        )
        assert result.is_error is False, f"`{tool_name}` failed: {result.content}"
        stated = result.content[0]
        assert isinstance(stated, TextContent), result.content
        return json.loads(stated.text)

    def call_tool_refusal(self, tool_name: str, arguments: "dict[str, Any] | None" = None) -> str:
        """MCP `tools/call` that must fail; returns why, and a success raises.

        The reason is the protocol error's message when the server refused the
        request, else the text of the tool error it answered with.
        """
        try:
            result = self.answer_over_mcp(
                lambda client: client.call_tool(tool_name, arguments or {})
            )
        except Exception as raised:
            return str(mcp_error_raised_in(raised))
        assert result.is_error is True, f"`{tool_name}` succeeded: {result.content}"
        return "\n".join(
            content.text for content in result.content if isinstance(content, TextContent)
        )

    def await_every_node_running(
        self,
        *,
        stream: str,
        expected_node_names: "Iterable[str] | None" = None,
        timeout: float = EVERY_NODE_RUNNING_TIMEOUT_SECONDS,
    ) -> "dict[str, Any]":
        """Poll the stream's graph until every node — and each of `expected_node_names` — is Running.

        Returns the graph that satisfied it; past the timeout raises naming each
        node's last state.
        """
        expected = set(expected_node_names or ())
        deadline = time.monotonic() + timeout
        node_states: "dict[str, Any]" = {}
        while True:
            graph = self.graph(stream)
            node_states = {
                node["name"]: node.get("components", {}).get("state") for node in graph["nodes"]
            }
            if (
                node_states
                and expected <= node_states.keys()
                and all(state == RUNNING_NODE_STATE for state in node_states.values())
            ):
                return graph
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"not every node was {RUNNING_NODE_STATE} within {timeout}s "
                    f"(expected {sorted(expected)}): {node_states}"
                )
            time.sleep(GRAPH_POLL_INTERVAL_SECONDS)

    def await_every_node_past_setup(
        self, *, stream: str, timeout: float = EVERY_NODE_RUNNING_TIMEOUT_SECONDS
    ) -> "dict[str, Any]":
        """Poll the stream's graph until no node is still before setup, and return each node's state.

        `Running` is a setup that returned and `Error` one that refused, so a
        caller asserting a refusal reads it here without waiting out a timeout
        for a `Running` that will never come. Past the timeout raises naming
        each node's last state.
        """
        deadline = time.monotonic() + timeout
        node_states: "dict[str, Any]" = {}
        while True:
            node_states = {
                node["name"]: node.get("components", {}).get("state")
                for node in self.graph(stream)["nodes"]
            }
            if node_states and not any(
                state in NODE_STATES_BEFORE_SETUP_COMPLETED for state in node_states.values()
            ):
                return node_states
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"a node was still before setup after {timeout}s: {node_states}"
                )
            time.sleep(GRAPH_POLL_INTERVAL_SECONDS)

    def await_link_state(
        self,
        link_id: str,
        wanted_state: str,
        *,
        stream: str,
        timeout: float = LINK_STATE_TIMEOUT_SECONDS,
    ) -> str:
        """Poll the stream's graph until the link reaches `wanted_state`, and report what it reached.

        A link that reaches `error` is returned as `error (<reason>)`, so the
        caller's assertion carries the refusal; a timeout returns `still <state>`.
        """
        deadline = time.monotonic() + timeout
        link: "dict[str, Any] | None" = None
        while time.monotonic() < deadline:
            link = next(
                (each for each in self.graph(stream)["links"] if each["id"] == link_id), None
            )
            if link is not None and link["state"] in (wanted_state, LINK_ERROR_STATE):
                return link["state"] + (
                    f" ({link['error_reason']})" if link.get("error_reason") else ""
                )
            time.sleep(GRAPH_POLL_INTERVAL_SECONDS)
        return f"still {link['state'] if link else 'absent'} after {timeout}s"
