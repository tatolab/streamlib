# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A JSON-RPC client for a running node's control plane.

The MCP tool set is the control vocabulary, and this is a pure client of it:
every verb marshals its arguments into one `tools/call` against the node's
`POST {url}/mcp` and prints the tool result. There is no second dispatch and no
local runtime — the control plane exists to observe nodes that are already
running.

Stdlib only, deliberately: the wheel must not grow a dependency to let a user
look at their own pipeline.
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from typing import Any, Optional

__all__ = [
    "ControlPlaneError",
    "control_plane_answers",
    "resolve_control_url",
    "call_tool",
]

#: Bearer token forwarded when the node has auth enabled. Absent by default —
#: a node runs locally with full permission unless its config opted in.
BEARER_TOKEN_ENVIRONMENT_VARIABLE = "STREAMLIB_MCP_TOKEN"

#: Bounds a liveness probe so a hung reused port cannot stall a registry scan.
REACHABILITY_PROBE_TIMEOUT_SECONDS = 1.5

#: Bounds a real verb. Generous: `tap` and `logs` collect a bounded sample
#: server-side and can legitimately take a moment to fill it.
CONTROL_VERB_TIMEOUT_SECONDS = 30.0


class ControlPlaneError(Exception):
    """A control-plane call that failed, with a message shaped for a terminal.

    `server_answered` separates "the node replied, with a status I did not want"
    from "nothing is listening there". A liveness probe treats the first as
    alive — an auth `401` still proves a control plane is up.
    """

    def __init__(self, message: str, *, server_answered: bool = False) -> None:
        super().__init__(message)
        self.server_answered = server_answered


def _mcp_endpoint(url: str) -> str:
    return f"{url.rstrip('/')}/mcp"


def _post_jsonrpc(url: str, body: str, timeout_seconds: float) -> str:
    """POST one JSON-RPC body to `{url}/mcp` and return the response body.

    Raises [`ControlPlaneError`] on a transport failure or a non-2xx status. A
    `202` (a notification ack) yields an empty string.
    """
    request = urllib.request.Request(
        _mcp_endpoint(url),
        data=body.encode("utf-8"),
        method="POST",
        headers={"content-type": "application/json"},
    )
    bearer_token = os.environ.get(BEARER_TOKEN_ENVIRONMENT_VARIABLE)
    if bearer_token:
        request.add_header("authorization", f"Bearer {bearer_token}")

    try:
        with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
            return response.read().decode("utf-8")
    except urllib.error.HTTPError as http_failure:
        detail = http_failure.read().decode("utf-8", errors="replace").strip()
        raise ControlPlaneError(
            f"control plane at {url} answered {http_failure.code}"
            + (f": {detail}" if detail else ""),
            server_answered=True,
        ) from http_failure
    except (urllib.error.URLError, OSError, TimeoutError) as transport_failure:
        raise ControlPlaneError(
            f"no control plane reachable at {url} ({transport_failure})"
        ) from transport_failure


def control_plane_answers(url: str) -> bool:
    """Whether the control plane at `url` answers its `POST {url}/mcp` at all.

    Any HTTP status counts as alive, including an auth `401` — the server is up
    and something answered. Only a transport failure is dead. This is what a
    registry scan needs: "can a control verb reach it", not "may I call it".
    """
    probe = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "graph", "arguments": {}},
        }
    )
    try:
        _post_jsonrpc(url, probe, REACHABILITY_PROBE_TIMEOUT_SECONDS)
    except ControlPlaneError as failure:
        return failure.server_answered
    return True


def resolve_control_url(
    requested_url: "Optional[str]", requested_node: "Optional[str]"
) -> str:
    """The control-plane URL a verb targets.

    `--url` wins outright, registered or not. Otherwise `--node <runtime_id>`
    resolves that node's URL from the registry. Otherwise the sole live node,
    which is the zero-ceremony case. Zero live nodes, or more than one with
    neither flag given, is an error that lists what it found.
    """
    if requested_url:
        return requested_url

    from ._node_registry import live_nodes

    nodes = live_nodes()

    if requested_node:
        for node in nodes:
            if node.runtime_id == requested_node:
                return node.control_url
        raise ControlPlaneError(
            f"no live node with runtime_id `{requested_node}`."
            + _live_node_hint(nodes)
        )

    if len(nodes) == 1:
        return nodes[0].control_url

    if not nodes:
        raise ControlPlaneError(
            "no running StreamLib nodes found.\n"
            "Start one with `streamlib dev`, or point at a node with `--url`."
        )

    raise ControlPlaneError(
        f"{len(nodes)} live nodes — pick one with `--node <runtime_id>` or "
        f"`--url <url>`." + _live_node_hint(nodes)
    )


def _live_node_hint(nodes: "list[Any]") -> str:
    """A trailing ` Live nodes: ...` fragment for a resolver error message."""
    if not nodes:
        return ""
    listed = ", ".join(f"{node.runtime_id} -> {node.control_url}" for node in nodes)
    return f" Live nodes: {listed}"


def call_tool(url: str, tool_name: str, arguments: "dict[str, Any]") -> str:
    """Drive one `tools/call` and return the tool result's text content.

    Covers the four ways a call can fail — a non-2xx status and a transport
    error (both from the POST), a top-level JSON-RPC `error` returned inside an
    HTTP 200, and a tool-level `result.isError` — so a caller that gets a string
    back has a real result rather than an error rendered as one.
    """
    request_body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": arguments},
        }
    )
    response_body = _post_jsonrpc(url, request_body, CONTROL_VERB_TIMEOUT_SECONDS)

    try:
        response = json.loads(response_body)
    except ValueError as decode_failure:
        raise ControlPlaneError(
            f"control plane returned a non-JSON response: {response_body}"
        ) from decode_failure

    if "error" in response:
        message = response["error"].get("message", "unknown JSON-RPC error")
        raise ControlPlaneError(f"{tool_name} failed: {message}")

    result = response.get("result")
    if result is None:
        raise ControlPlaneError(
            f"control plane response missing `result`: {response_body}"
        )

    content = result.get("content") or [{}]
    text = content[0].get("text", "")
    if result.get("isError", False):
        raise ControlPlaneError(f"{tool_name} failed: {text}")
    return text
