# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A client of one running `tatolabd`'s local API, from the suite venv alone.

Nothing here imports `tatolab.runtime`: the plain HTTP routes (`/health`,
`/api/graph`, `/api/registry`) are read with the standard library over the Unix
socket, and MCP is the official MCP Python SDK's streamable-HTTP client over the
same socket. A graph's nodes carry their state at `components["state"]`, its
links at `state`.
"""

from __future__ import annotations

import asyncio
import http.client
import json
import socket
import time
from collections.abc import Awaitable, Callable, Iterable
from pathlib import Path
from typing import Any, TypeVar

import httpx2
from mcp.client.client import Client
from mcp.client.streamable_http import streamable_http_client
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
    """One `tatolabd`'s local API, reached at its Unix socket."""

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

    def graph(self) -> "dict[str, Any]":
        """`GET /api/graph`: the live graph, its nodes, links, exposures and runtime name."""
        return self.get_json("/api/graph")

    def registry(self) -> Any:
        """`GET /api/registry`: the node catalog this runtime can add."""
        return self.get_json("/api/registry")

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

    def await_every_node_running(
        self,
        *,
        expected_node_names: "Iterable[str] | None" = None,
        timeout: float = EVERY_NODE_RUNNING_TIMEOUT_SECONDS,
    ) -> "dict[str, Any]":
        """Poll `/api/graph` until every node — and each of `expected_node_names` — is Running.

        Returns the graph that satisfied it; past the timeout raises naming each
        node's last state.
        """
        expected = set(expected_node_names or ())
        deadline = time.monotonic() + timeout
        node_states: "dict[str, Any]" = {}
        while True:
            graph = self.graph()
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
        self, *, timeout: float = EVERY_NODE_RUNNING_TIMEOUT_SECONDS
    ) -> "dict[str, Any]":
        """Poll `/api/graph` until no node is still before setup, and return each node's state.

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
                for node in self.graph()["nodes"]
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
        self, link_id: str, wanted_state: str, *, timeout: float = LINK_STATE_TIMEOUT_SECONDS
    ) -> str:
        """Poll `/api/graph` until the link reaches `wanted_state`, and report what it reached.

        A link that reaches `error` is returned as `error (<reason>)`, so the
        caller's assertion carries the refusal; a timeout returns `still <state>`.
        """
        deadline = time.monotonic() + timeout
        link: "dict[str, Any] | None" = None
        while time.monotonic() < deadline:
            link = next((each for each in self.graph()["links"] if each["id"] == link_id), None)
            if link is not None and link["state"] in (wanted_state, LINK_ERROR_STATE):
                return link["state"] + (
                    f" ({link['error_reason']})" if link.get("error_reason") else ""
                )
            time.sleep(GRAPH_POLL_INTERVAL_SECONDS)
        return f"still {link['state'] if link else 'absent'} after {timeout}s"
