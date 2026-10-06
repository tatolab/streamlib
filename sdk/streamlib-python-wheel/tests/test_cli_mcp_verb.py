# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib mcp`, driven against a scripted local API socket.

The verb is a byte pipe, so its contract is about bytes and exits: what it
sends to open the stream, that it copies both ways untouched, how stdin's end
and the runtime's end each finish it, and what it says on stderr. The runtime
side of the stream is `runtime/streamlib-api-server/src/mcp_stdio_upgrade.rs`'s
to prove; a real node through the verb is `test_cli_mcp_verb_live.py`'s.
"""

from __future__ import annotations

import io
import os
import socket
import tempfile
import threading
from typing import Callable, Iterator

import pytest

from streamlib import _node_registry, cli
from streamlib._local_api_mcp_stdio_pipe import (
    MCP_STDIO_UPGRADE_REQUEST,
    pipe_stdio_to_the_runtimes_mcp_server,
)
from streamlib._node_registry import NodeRegistryEntry

SCRIPTED_RUNTIME_NAME = "scripted-runtime"
SCRIPTED_RUNTIME_ID = "Rscripted"
SWITCHING_PROTOCOLS_HEAD = (
    b"HTTP/1.1 101 Switching Protocols\r\nconnection: upgrade\r\nupgrade: mcp-stdio\r\n\r\n"
)
SCRIPT_TIMEOUT_SECONDS = 10.0


def read_until(connection: socket.socket, terminator: bytes) -> bytes:
    received = b""
    while terminator not in received:
        chunk = connection.recv(4096)
        if not chunk:
            break
        received += chunk
    return received


def read_to_end(connection: socket.socket) -> bytes:
    received = b""
    while chunk := connection.recv(4096):
        received += chunk
    return received


class ScriptedLocalApiSocket:
    """A Unix socket whose one connection is handed to `script`, which plays the runtime."""

    def __init__(self, script: Callable[[socket.socket], None]) -> None:
        self._socket_directory = tempfile.TemporaryDirectory(prefix="sl-mcp-")
        self.local_api_socket_path = os.path.join(self._socket_directory.name, "local-api.sock")
        self._listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._listener.bind(self.local_api_socket_path)
        self._listener.listen(1)
        self.script_failure: "BaseException | None" = None
        self._script_thread = threading.Thread(target=self._serve, args=(script,), daemon=True)
        self._script_thread.start()

    def _serve(self, script: Callable[[socket.socket], None]) -> None:
        try:
            connection, _ = self._listener.accept()
        except OSError:
            # Shut down with nothing connected: a test that never opened the stream.
            return
        connection.settimeout(SCRIPT_TIMEOUT_SECONDS)
        try:
            script(connection)
        except BaseException as failure:  # surfaced by `finish`
            self.script_failure = failure
        finally:
            connection.close()

    def finish(self) -> None:
        self._script_thread.join(SCRIPT_TIMEOUT_SECONDS)
        assert not self._script_thread.is_alive(), "the scripted runtime never finished"
        if self.script_failure is not None:
            raise self.script_failure

    def close(self) -> None:
        # Close alone does not wake an `accept` already blocked on Linux.
        try:
            self._listener.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self._listener.close()
        self._socket_directory.cleanup()


@pytest.fixture
def scripted_live_runtime(monkeypatch) -> Iterator[Callable[[Callable[[socket.socket], None]], ScriptedLocalApiSocket]]:
    """Register one live runtime whose local API socket `script` plays."""
    opened: "list[ScriptedLocalApiSocket]" = []

    def open_scripted(script: Callable[[socket.socket], None]) -> ScriptedLocalApiSocket:
        scripted = ScriptedLocalApiSocket(script)
        opened.append(scripted)
        entry = NodeRegistryEntry(
            schema_version=3,
            runtime_id=SCRIPTED_RUNTIME_ID,
            runtime_name=SCRIPTED_RUNTIME_NAME,
            local_api_socket_path=scripted.local_api_socket_path,
            pid=os.getpid(),
            hint="",
        )
        monkeypatch.setattr(_node_registry, "live_nodes", lambda: [entry])
        return scripted

    yield open_scripted
    for scripted in opened:
        scripted.close()


class PipedMcpHost:
    """The MCP host's side of the verb: a pipe for its stdin, a buffer for its stdout."""

    def __init__(self) -> None:
        self._stdin_read_fd, self._stdin_write_fd = os.pipe()
        self.stdout = io.BytesIO()
        self.exit_code: "int | None" = None
        self._verb_thread = threading.Thread(target=self._run_the_verb, daemon=True)

    def _run_the_verb(self) -> None:
        self.exit_code = pipe_stdio_to_the_runtimes_mcp_server(
            None, mcp_host_input_fd=self._stdin_read_fd, mcp_host_output=self.stdout
        )

    def start(self) -> "PipedMcpHost":
        self._verb_thread.start()
        return self

    def write_stdin(self, data: bytes) -> None:
        os.write(self._stdin_write_fd, data)

    def close_stdin(self) -> None:
        os.close(self._stdin_write_fd)
        self._stdin_write_fd = -1

    def wait_for_exit(self) -> int:
        self._verb_thread.join(SCRIPT_TIMEOUT_SECONDS)
        assert not self._verb_thread.is_alive(), "the verb never exited"
        assert self.exit_code is not None
        return self.exit_code

    def close(self) -> None:
        if self._stdin_write_fd != -1:
            os.close(self._stdin_write_fd)
        os.close(self._stdin_read_fd)


@pytest.fixture
def piped_mcp_host() -> Iterator[Callable[[], PipedMcpHost]]:
    hosts: "list[PipedMcpHost]" = []

    def start_host() -> PipedMcpHost:
        host = PipedMcpHost().start()
        hosts.append(host)
        return host

    yield start_host
    for host in hosts:
        host.close()


def test_the_verb_opens_the_stream_with_one_upgrade_and_copies_both_ways_untouched(
    scripted_live_runtime, piped_mcp_host
):
    # Neither line is valid MCP: the verb must not read what it carries.
    host_line = b'{"not": "inspected", "bytes": "\\u00e9"}\n'
    runtime_bytes_with_the_head = b'{"sent": "with the 101"}\n'
    runtime_line = b"\x00\xffnot even json\n"
    received_by_the_runtime: "list[bytes]" = []

    def runtime(connection: socket.socket) -> None:
        request_head = read_until(connection, b"\r\n\r\n")
        received_by_the_runtime.append(request_head)
        connection.sendall(SWITCHING_PROTOCOLS_HEAD + runtime_bytes_with_the_head)
        received_by_the_runtime.append(read_until(connection, b"\n"))
        connection.sendall(runtime_line)
        received_by_the_runtime.append(read_to_end(connection))

    scripted = scripted_live_runtime(runtime)
    host = piped_mcp_host()
    host.write_stdin(host_line)
    host.close_stdin()
    assert host.wait_for_exit() == 0
    scripted.finish()

    request_head, first_host_bytes, after_the_first_line = received_by_the_runtime
    assert request_head == MCP_STDIO_UPGRADE_REQUEST
    assert first_host_bytes + after_the_first_line == host_line
    assert host.stdout.getvalue() == runtime_bytes_with_the_head + runtime_line


def test_stdin_ending_half_closes_the_stream_and_the_verb_exits_when_the_runtime_closes(
    scripted_live_runtime, piped_mcp_host
):
    answer_owed_after_stdin_ended = b'{"jsonrpc":"2.0","id":1,"result":{}}\n'

    def runtime(connection: socket.socket) -> None:
        read_until(connection, b"\r\n\r\n")
        connection.sendall(SWITCHING_PROTOCOLS_HEAD)
        assert read_to_end(connection) == b'{"id":1}\n', "stdin's end reaches the runtime"
        connection.sendall(answer_owed_after_stdin_ended)

    scripted = scripted_live_runtime(runtime)
    host = piped_mcp_host()
    host.write_stdin(b'{"id":1}\n')
    host.close_stdin()

    assert host.wait_for_exit() == 0
    scripted.finish()
    assert host.stdout.getvalue() == answer_owed_after_stdin_ended


def test_the_runtime_closing_while_stdin_is_open_exits_non_zero_naming_the_runtime(
    scripted_live_runtime, piped_mcp_host, capfd
):
    def runtime(connection: socket.socket) -> None:
        read_until(connection, b"\r\n\r\n")
        connection.sendall(SWITCHING_PROTOCOLS_HEAD)

    scripted = scripted_live_runtime(runtime)
    host = piped_mcp_host()

    assert host.wait_for_exit() == 1
    scripted.finish()
    stderr_lines = capfd.readouterr().err.splitlines()
    assert len(stderr_lines) == 1, stderr_lines
    assert SCRIPTED_RUNTIME_NAME in stderr_lines[0]
    assert SCRIPTED_RUNTIME_ID in stderr_lines[0]
    assert host.stdout.getvalue() == b""


def test_a_runtime_refusing_the_upgrade_exits_non_zero_naming_it_and_its_answer(
    scripted_live_runtime, piped_mcp_host, capfd
):
    def runtime(connection: socket.socket) -> None:
        read_until(connection, b"\r\n\r\n")
        connection.sendall(b"HTTP/1.1 426 Upgrade Required\r\ncontent-length: 0\r\n\r\n")

    scripted = scripted_live_runtime(runtime)
    host = piped_mcp_host()

    assert host.wait_for_exit() == 1
    scripted.finish()
    stderr_lines = capfd.readouterr().err.splitlines()
    assert len(stderr_lines) == 1, stderr_lines
    assert SCRIPTED_RUNTIME_NAME in stderr_lines[0]
    assert "HTTP/1.1 426 Upgrade Required" in stderr_lines[0]
    assert host.stdout.getvalue() == b""


def test_no_live_runtime_is_a_one_line_refusal_naming_streamlib_nodes(monkeypatch, capfd):
    monkeypatch.setattr(_node_registry, "live_nodes", lambda: [])

    assert cli.main(["mcp"]) == 1

    captured = capfd.readouterr()
    assert captured.out == ""
    stderr_lines = captured.err.splitlines()
    assert len(stderr_lines) == 1, stderr_lines
    assert "`streamlib nodes`" in stderr_lines[0]


def test_no_live_runtime_is_the_same_refusal_when_node_names_one(monkeypatch, capfd):
    monkeypatch.setattr(_node_registry, "live_nodes", lambda: [])

    assert cli.main(["mcp", "--node", "absent-runtime"]) == 1

    captured = capfd.readouterr()
    assert captured.out == ""
    stderr_lines = captured.err.splitlines()
    assert len(stderr_lines) == 1, stderr_lines
    assert "`streamlib nodes`" in stderr_lines[0]


def test_a_node_flag_matching_no_live_runtime_is_refused_naming_it(
    scripted_live_runtime, capfd
):
    scripted_live_runtime(lambda connection: None)

    assert cli.main(["mcp", "--node", "absent-runtime"]) == 1

    captured = capfd.readouterr()
    assert captured.out == ""
    assert "absent-runtime" in captured.err
    assert SCRIPTED_RUNTIME_NAME in captured.err, "the refusal lists the live runtimes"
