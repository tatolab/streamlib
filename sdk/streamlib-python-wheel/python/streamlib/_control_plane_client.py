# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A client for a running node's control plane.

The MCP tool set is the control vocabulary, and this is a pure client of it:
every verb marshals its arguments into one `tools/call` against the node's
`POST /mcp` and prints the tool result. There is no second dispatch and no
local runtime — the control plane exists to observe nodes that are already
running.

A node found through the registry is reached through its local API socket, a
Unix socket only its own user can open. A `--url` still reaches a node's TCP
port.

One operation has a second spelling this also drives: the surface exchange
serves the exact frame as a binary `image/png` over REST, where the MCP tool
serves a downscaled block sized for a model's eyes. A caller writing evidence to
disk wants the exact bytes, so it takes the REST route.

Stdlib only, deliberately: the wheel must not grow a dependency to let a user
look at their own pipeline.
"""

from __future__ import annotations

import email.message
import http.client
import json
import os
import socket
import urllib.error
import urllib.parse
import urllib.request
from typing import TYPE_CHECKING, Any, NamedTuple, Optional, Union

if TYPE_CHECKING:
    from ._node_registry import NodeRegistryEntry

__all__ = [
    "ControlPlaneError",
    "SurfaceImageExchangeRefusal",
    "ExchangedSurfaceImage",
    "LocalApiSocket",
    "ControlPlaneEndpoint",
    "control_plane_answers",
    "resolve_control_plane_endpoint",
    "call_tool",
    "fetch_surface_image_png_bytes",
]

#: Bearer token forwarded when the node has auth enabled. Absent by default —
#: a node runs locally with full permission unless its config opted in.
BEARER_TOKEN_ENVIRONMENT_VARIABLE = "STREAMLIB_MCP_TOKEN"

#: Bounds a liveness probe so a socket that accepts but never answers cannot
#: stall a registry scan.
REACHABILITY_PROBE_TIMEOUT_SECONDS = 1.5

#: Bounds a real verb. Generous: `tap` and `logs` collect a bounded sample
#: server-side and can legitimately take a moment to fill it.
CONTROL_VERB_TIMEOUT_SECONDS = 30.0


class LocalApiSocket(NamedTuple):
    """A runtime's local API, reached through the Unix socket it serves it on."""

    local_api_socket_path: str


#: Where a verb sends its requests: a runtime's local API socket, or a
#: control-plane base URL given with `--url`.
ControlPlaneEndpoint = Union[LocalApiSocket, str]


def describe_control_plane_endpoint(endpoint: ControlPlaneEndpoint) -> str:
    """The endpoint as a terminal message names it: the socket's path, or the URL."""
    if isinstance(endpoint, LocalApiSocket):
        return endpoint.local_api_socket_path
    return endpoint


class ControlPlaneError(Exception):
    """A control-plane call that failed, with a message shaped for a terminal.

    `server_answered` separates "the node replied, with a status I did not want"
    from "nothing is listening there". A liveness probe treats the first as
    alive — an auth `401` still proves a control plane is up.
    """

    def __init__(self, message: str, *, server_answered: bool = False) -> None:
        super().__init__(message)
        self.server_answered = server_answered


#: The MCP endpoint's path on every control plane.
MCP_ENDPOINT_PATH = "/mcp"


def _refuse_a_non_http_url(url: str) -> None:
    """Refuse a URL `urlopen` would dispatch to a non-HTTP handler.

    `urlopen` dispatches on the scheme, so an unchecked `--url file:///etc/passwd`
    would select a handler that is not HTTP at all.
    """
    if urllib.parse.urlparse(url).scheme not in ("http", "https"):
        raise ControlPlaneError(f"control-plane URL must be http or https; got `{url}`")


class _ControlPlaneHttpResponse(NamedTuple):
    """One answered request: its status, headers and whole body, whatever the status."""

    status: int
    headers: email.message.Message
    body: bytes


class _LocalApiSocketHttpConnection(http.client.HTTPConnection):
    """An HTTP/1.1 connection whose transport is a Unix socket rather than TCP."""

    def __init__(self, local_api_socket_path: str, timeout_seconds: float) -> None:
        # The host only fills the `Host` header; the socket path is the address.
        super().__init__("localhost", timeout=timeout_seconds)
        self._local_api_socket_path = local_api_socket_path

    def connect(self) -> None:
        unix_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        unix_socket.settimeout(self.timeout)
        try:
            unix_socket.connect(self._local_api_socket_path)
        except BaseException:
            unix_socket.close()
            raise
        self.sock = unix_socket


def _request_over_the_local_api_socket(
    local_api_socket: LocalApiSocket,
    *,
    method: str,
    path: str,
    body: "Optional[bytes]",
    headers: "dict[str, str]",
    timeout_seconds: float,
) -> _ControlPlaneHttpResponse:
    """One request over the socket. No bearer token: the socket's file mode is the gate."""
    connection = _LocalApiSocketHttpConnection(
        local_api_socket.local_api_socket_path, timeout_seconds
    )
    try:
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        return _ControlPlaneHttpResponse(response.status, response.msg, response.read())
    except (http.client.HTTPException, OSError) as transport_failure:
        raise ControlPlaneError(
            f"no control plane reachable at {local_api_socket.local_api_socket_path} "
            f"({transport_failure})"
        ) from transport_failure
    finally:
        connection.close()


def _request_over_a_control_plane_url(
    url: str,
    *,
    method: str,
    path: str,
    body: "Optional[bytes]",
    headers: "dict[str, str]",
    timeout_seconds: float,
) -> _ControlPlaneHttpResponse:
    """One request to a `--url`, carrying the node's bearer token when one is configured."""
    _refuse_a_non_http_url(url)
    request = urllib.request.Request(
        f"{url.rstrip('/')}{path}", data=body, method=method, headers=headers
    )
    bearer_token = os.environ.get(BEARER_TOKEN_ENVIRONMENT_VARIABLE)
    if bearer_token:
        request.add_header("authorization", f"Bearer {bearer_token}")
    try:
        with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
            return _ControlPlaneHttpResponse(response.status, response.headers, response.read())
    except urllib.error.HTTPError as http_failure:
        return _ControlPlaneHttpResponse(
            http_failure.code, http_failure.headers, http_failure.read()
        )
    except (urllib.error.URLError, OSError, TimeoutError) as transport_failure:
        raise ControlPlaneError(
            f"no control plane reachable at {url} ({transport_failure})"
        ) from transport_failure


def _request_to_control_plane_endpoint(
    endpoint: ControlPlaneEndpoint,
    *,
    method: str,
    path: str,
    body: "Optional[bytes]" = None,
    headers: "Optional[dict[str, str]]" = None,
    timeout_seconds: float,
) -> _ControlPlaneHttpResponse:
    """One request to `endpoint`, answered with any status; only a transport failure raises."""
    if isinstance(endpoint, LocalApiSocket):
        return _request_over_the_local_api_socket(
            endpoint,
            method=method,
            path=path,
            body=body,
            headers=headers or {},
            timeout_seconds=timeout_seconds,
        )
    return _request_over_a_control_plane_url(
        endpoint,
        method=method,
        path=path,
        body=body,
        headers=headers or {},
        timeout_seconds=timeout_seconds,
    )


def _post_jsonrpc(endpoint: ControlPlaneEndpoint, body: str, timeout_seconds: float) -> str:
    """POST one JSON-RPC body to the endpoint's `/mcp` and return the response body.

    Raises [`ControlPlaneError`] on a transport failure or a non-2xx status. A
    `202` (a notification ack) yields an empty string.
    """
    answered = _request_to_control_plane_endpoint(
        endpoint,
        method="POST",
        path=MCP_ENDPOINT_PATH,
        body=body.encode("utf-8"),
        headers={"content-type": "application/json"},
        timeout_seconds=timeout_seconds,
    )
    if not 200 <= answered.status < 300:
        detail = answered.body.decode("utf-8", errors="replace").strip()
        raise ControlPlaneError(
            f"control plane at {describe_control_plane_endpoint(endpoint)} answered "
            f"{answered.status}" + (f": {detail}" if detail else ""),
            server_answered=True,
        )
    return answered.body.decode("utf-8")


def control_plane_answers(endpoint: ControlPlaneEndpoint) -> bool:
    """Whether the control plane at `endpoint` answers its `POST /mcp` at all.

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
        _post_jsonrpc(endpoint, probe, REACHABILITY_PROBE_TIMEOUT_SECONDS)
    except ControlPlaneError as failure:
        return failure.server_answered
    return True


def resolve_control_plane_endpoint(
    requested_url: "Optional[str]", requested_node: "Optional[str]"
) -> ControlPlaneEndpoint:
    """The endpoint a verb targets.

    `--url` wins outright, registered or not. Otherwise `--node` resolves that
    node's local API socket from the registry, matching a runtime name first and
    a runtime_id second — the name is the one an app chooses and keeps across
    runs. Otherwise the sole live node, which is the zero-ceremony case. Zero
    live nodes, more than one matching `--node`, or more than one live node with
    neither flag given, is an error that lists what it found.
    """
    if requested_url:
        return requested_url

    from ._node_registry import live_nodes

    nodes = live_nodes()

    if requested_node:
        return _sole_node_matching(nodes, requested_node)

    if len(nodes) == 1:
        return LocalApiSocket(nodes[0].local_api_socket_path)

    if not nodes:
        raise ControlPlaneError(
            "no running StreamLib nodes found.\n"
            "Start one with `streamlib dev`, or point at a node with `--url`."
        )

    raise ControlPlaneError(
        f"{len(nodes)} live nodes — pick one with `--node <runtime name or id>`."
        + _live_node_hint(nodes)
    )


def _sole_node_matching(
    nodes: "list[NodeRegistryEntry]", requested_node: str
) -> LocalApiSocket:
    """The local API socket of the one live node `--node` names, or an error.

    Names are matched before ids because a name is what an app chose. Nothing
    makes a name unique, so a tie names every match and refuses rather than
    picking one.
    """
    for matching in (
        [node for node in nodes if node.runtime_name == requested_node],
        [node for node in nodes if node.runtime_id == requested_node],
    ):
        if len(matching) == 1:
            return LocalApiSocket(matching[0].local_api_socket_path)
        if matching:
            raise ControlPlaneError(
                f"{len(matching)} live nodes answer to `{requested_node}` — pick one by "
                f"runtime_id with `--node <runtime_id>`."
                + _live_node_hint(matching)
            )
    raise ControlPlaneError(
        f"no live node named `{requested_node}`, and none with that runtime_id."
        + _live_node_hint(nodes)
    )


def _live_node_hint(nodes: "list[NodeRegistryEntry]") -> str:
    """A trailing ` Live nodes: ...` fragment for a resolver error message."""
    if not nodes:
        return ""
    listed = ", ".join(
        f"{node.runtime_name} ({node.runtime_id}) -> {node.local_api_socket_path}"
        for node in nodes
    )
    return f" Live nodes: {listed}"


def call_tool(
    endpoint: ControlPlaneEndpoint, tool_name: str, arguments: "dict[str, Any]"
) -> str:
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
    response_body = _post_jsonrpc(endpoint, request_body, CONTROL_VERB_TIMEOUT_SECONDS)

    try:
        response = json.loads(response_body)
    except ValueError as decode_failure:
        raise ControlPlaneError(
            f"control plane returned a non-JSON response: {response_body}"
        ) from decode_failure

    # Every member below is checked for shape, not just presence: a server that
    # answers 200 with a differently-shaped body would otherwise surface as an
    # AttributeError traceback rather than as the failure it is.
    if not isinstance(response, dict):
        raise ControlPlaneError(
            f"control plane returned a non-object JSON-RPC response: {response_body}"
        )

    if "error" in response:
        error = response["error"]
        message = (
            error.get("message", "unknown JSON-RPC error")
            if isinstance(error, dict)
            else error
        )
        raise ControlPlaneError(f"{tool_name} failed: {message}")

    result = response.get("result")
    if not isinstance(result, dict):
        raise ControlPlaneError(
            f"control plane response missing a `result` object: {response_body}"
        )

    content = result.get("content")
    first_block = content[0] if isinstance(content, list) and content else None
    text = first_block.get("text") if isinstance(first_block, dict) else None

    if result.get("isError", False):
        raise ControlPlaneError(f"{tool_name} failed: {text or 'no detail given'}")
    if not isinstance(text, str):
        # Succeeded, but carries nothing readable. Returning "" here would print
        # a blank line and exit 0, which reads as "the node has nothing" rather
        # than "this response made no sense".
        raise ControlPlaneError(
            f"{tool_name} returned no text content: {response_body}"
        )
    return text


#: The REST spelling of the exchange, as the api-server serves it. Kept as the
#: template rather than a formatted string so the one place that fills it is
#: also the one place that percent-encodes the id.
SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE = "/api/surfaces/{surface_id}/image"

#: Headers stating the surface's own extent, which differs from the returned
#: image's whenever a downscale cap applied.
SOURCE_SURFACE_PIXEL_WIDTH_HEADER = "x-streamlib-surface-pixel-width"
SOURCE_SURFACE_PIXEL_HEIGHT_HEADER = "x-streamlib-surface-pixel-height"

#: The status the exchange answers when the id named a frame whose pool slot has
#: since been recycled. Its own answer, distinct from a `404`: the id was
#: well-formed and the frame is simply gone, so the caller taps a newer bag
#: rather than concluding the surface never existed.
RECYCLED_FRAME_HTTP_STATUS = 410


class ExchangedSurfaceImage(NamedTuple):
    """One frame's pixels, plus the extent of the surface they came from."""

    png_image_bytes: bytes
    source_surface_pixel_width: "Optional[int]"
    source_surface_pixel_height: "Optional[int]"


class SurfaceImageExchangeRefusal(ControlPlaneError):
    """An exchange the node answered and refused, carrying the status it used.

    The status is the whole point: a recycled frame composes as a retry against
    a newer bag, while a `404` or a `501` will refuse identically forever and
    must stop the caller instead.
    """

    def __init__(self, message: str, *, http_status: int) -> None:
        super().__init__(message, server_answered=True)
        self.http_status = http_status

    @property
    def names_a_recycled_frame(self) -> bool:
        """Whether the frame existed and its pool slot has since been reused."""
        return self.http_status == RECYCLED_FRAME_HTTP_STATUS


def _surface_image_exchange_route_path(published_surface_id: str) -> str:
    """The exchange route's path for one surface id, ready to put on the wire.

    A pooled frame id is `<slot>#<generation>`, and a bare `#` would make the
    generation a URL fragment the server never sees — so the id is encoded down
    to RFC 3986's unreserved set, which is what `quote(safe="")` leaves alone.
    """
    return SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE.replace(
        "{surface_id}", urllib.parse.quote(published_surface_id, safe="")
    )


def _refusal_detail(body: bytes) -> str:
    """The message out of the route's `{"error": …}` body, or its raw text."""
    text = body.decode("utf-8", errors="replace").strip()
    try:
        decoded = json.loads(text)
    except ValueError:
        return text
    if isinstance(decoded, dict) and isinstance(decoded.get("error"), str):
        return decoded["error"]
    return text


def _header_pixel_extent(
    headers: email.message.Message, header_name: str
) -> "Optional[int]":
    """One extent header as an int, or `None` when it is absent or malformed.

    Absent rather than fatal: the extent annotates the image, and a node that
    stopped stating it would still have handed back real pixels.
    """
    raw = headers.get(header_name)
    if raw is None:
        return None
    try:
        return int(raw)
    except ValueError:
        return None


def fetch_surface_image_png_bytes(
    endpoint: ControlPlaneEndpoint,
    published_surface_id: str,
    *,
    timeout_seconds: float = CONTROL_VERB_TIMEOUT_SECONDS,
) -> ExchangedSurfaceImage:
    """Exchange one published surface id for that frame's exact PNG bytes.

    The full-resolution REST spelling, not the MCP tool's downscaled block: this
    is what gets written to disk as evidence.
    """
    answered = _request_to_control_plane_endpoint(
        endpoint,
        method="GET",
        path=_surface_image_exchange_route_path(published_surface_id),
        timeout_seconds=timeout_seconds,
    )
    if not 200 <= answered.status < 300:
        detail = _refusal_detail(answered.body)
        raise SurfaceImageExchangeRefusal(
            f"exchange of surface `{published_surface_id}` answered "
            f"{answered.status}" + (f": {detail}" if detail else ""),
            http_status=answered.status,
        )
    return ExchangedSurfaceImage(
        png_image_bytes=answered.body,
        source_surface_pixel_width=_header_pixel_extent(
            answered.headers, SOURCE_SURFACE_PIXEL_WIDTH_HEADER
        ),
        source_surface_pixel_height=_header_pixel_extent(
            answered.headers, SOURCE_SURFACE_PIXEL_HEIGHT_HEADER
        ),
    )
