# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib mcp`: an MCP host's stdin and stdout, piped to a runtime's MCP server.

The verb sends one `Upgrade: mcp-stdio` request over the runtime's local API
socket and then only copies bytes, stdin → socket and socket → stdout. It
parses no message, so the protocol revision is the runtime's alone. stdout
carries nothing but what the runtime wrote.
"""

from __future__ import annotations

import os
import socket
import sys
import threading
from typing import BinaryIO, Optional

from ._control_plane_client import NoLiveNodeError, resolve_requested_live_node

__all__ = ["pipe_stdio_to_the_runtimes_mcp_server"]

MCP_STDIO_UPGRADE_REQUEST = (
    b"GET /mcp/stdio HTTP/1.1\r\n"
    b"Host: localhost\r\n"
    b"Connection: Upgrade\r\n"
    b"Upgrade: mcp-stdio\r\n"
    b"\r\n"
)

#: A response head longer than this is not the runtime's `101`.
MAX_UPGRADE_RESPONSE_HEAD_BYTES = 16 * 1024

PIPE_CHUNK_BYTES = 64 * 1024


def pipe_stdio_to_the_runtimes_mcp_server(
    requested_node: Optional[str],
    *,
    mcp_host_input_fd: Optional[int] = None,
    mcp_host_output: Optional[BinaryIO] = None,
) -> int:
    """Pipe the MCP host's stdio to the runtime `requested_node` names, and return the exit code."""
    try:
        node = resolve_requested_live_node(requested_node)
    except NoLiveNodeError:
        print(
            "error: no StreamLib runtime is live on this machine for `streamlib mcp` to reach; "
            "`streamlib nodes` lists the live ones.",
            file=sys.stderr,
        )
        return 1
    runtime_named = f"runtime `{node.runtime_name}` ({node.runtime_id})"
    if mcp_host_input_fd is None:
        mcp_host_input_fd = sys.stdin.fileno()
    if mcp_host_output is None:
        mcp_host_output = sys.stdout.buffer

    local_api_stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        try:
            local_api_stream.connect(node.local_api_socket_path)
            local_api_stream.sendall(MCP_STDIO_UPGRADE_REQUEST)
            first_streamed_bytes = _bytes_after_the_switching_protocols_head(local_api_stream)
        except (OSError, _UpgradeRefused) as failure:
            print(f"error: {runtime_named} did not open its MCP stream: {failure}", file=sys.stderr)
            return 1

        mcp_host_input_ended = threading.Event()
        threading.Thread(
            target=_copy_mcp_host_input_to_the_runtime,
            args=(mcp_host_input_fd, local_api_stream, mcp_host_input_ended),
            name="streamlib-mcp-stdin-to-runtime",
            daemon=True,
        ).start()

        try:
            _copy_the_runtime_to_mcp_host_output(
                local_api_stream, first_streamed_bytes, mcp_host_output
            )
        except BrokenPipeError:
            # The host stopped reading; nobody is left to answer.
            return 0
        if mcp_host_input_ended.is_set():
            return 0
        print(f"error: {runtime_named} closed its MCP stream.", file=sys.stderr)
        return 1
    finally:
        local_api_stream.close()


class _UpgradeRefused(Exception):
    """The runtime answered the upgrade request with something other than `101`."""


def _bytes_after_the_switching_protocols_head(local_api_stream: socket.socket) -> bytes:
    """Read the runtime's answer to the upgrade; return what it streamed past the head."""
    received = b""
    while b"\r\n\r\n" not in received:
        if len(received) > MAX_UPGRADE_RESPONSE_HEAD_BYTES:
            raise _UpgradeRefused("its answer to the upgrade has no end to its head")
        chunk = local_api_stream.recv(PIPE_CHUNK_BYTES)
        if not chunk:
            raise _UpgradeRefused("it closed the connection before answering the upgrade")
        received += chunk
    head, _, streamed = received.partition(b"\r\n\r\n")
    status_line = head.split(b"\r\n", 1)[0].decode("latin-1")
    status_fields = status_line.split(" ", 2)
    if len(status_fields) < 2 or status_fields[1] != "101":
        raise _UpgradeRefused(f"it answered `{status_line}`")
    return streamed


def _copy_mcp_host_input_to_the_runtime(
    mcp_host_input_fd: int,
    local_api_stream: socket.socket,
    mcp_host_input_ended: threading.Event,
) -> None:
    try:
        while True:
            chunk = os.read(mcp_host_input_fd, PIPE_CHUNK_BYTES)
            if not chunk:
                break
            local_api_stream.sendall(chunk)
    except OSError:
        # The runtime's side is gone; the copy the other way reports it.
        return
    mcp_host_input_ended.set()
    try:
        local_api_stream.shutdown(socket.SHUT_WR)
    except OSError:
        pass


def _copy_the_runtime_to_mcp_host_output(
    local_api_stream: socket.socket, first_streamed_bytes: bytes, mcp_host_output: BinaryIO
) -> None:
    chunk = first_streamed_bytes
    while True:
        if chunk:
            mcp_host_output.write(chunk)
            mcp_host_output.flush()
        try:
            chunk = local_api_stream.recv(PIPE_CHUNK_BYTES)
        except ConnectionResetError:
            return
        if not chunk:
            return
