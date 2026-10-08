# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A started `tatolabd` or `tatolab`, driven from a test with every wait bounded.

The process runs in a session of its own, so a signal aimed at it cannot reach
the test runner, and its process group can be killed whatever the test did.
Standard output and standard error are pumped by two reader threads into two
line lists, so neither pipe can fill and wedge the runtime while a test waits.

`tatolabd` writes every log line to standard error and nothing to standard
output. A node reports with `log.info("MARKER:<NAME> <json>")`; the line reaches
`tatolabd`'s standard error through the helper log drain, wrapped in a log
record, and is matched anywhere in the line. A `tatolab` run passes its
`tatolabd`'s standard error through, so the same waits read both.

A node's registry entry is found by the pid of the `tatolabd` hosting it —
`tatolabd` itself, or a `tatolab`'s child — never as "the only entry", because
on macOS the runtime directory is shared with every other runtime on the machine.
"""

from __future__ import annotations

import json
import os
import re
import signal
import subprocess
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from local_api_client import LocalApiClient

#: How long a wait on output, a registry entry or an exit lasts unless a test
#: names its own. A cold start stands up a GPU context, an iceoryx2 node and a
#: socket; a real hang blows through it anyway.
DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS = 90.0

MARKER_PREFIX = "MARKER:"

#: The engine's last line of a graceful stop.
ENGINE_GRACEFUL_STOP_LOG_LINE = "[stop] Graceful shutdown complete"

#: The engine's line once every processor has started.
ENGINE_STARTED_LOG_LINE = "[start] Runtime started"

#: The line `tatolabd` logs once the graph loaded, before the engine starts.
STREAM_LOADED_LOG_LINE_PATTERN = re.compile(
    r"the stream(?: `(?P<stream_name>[^`]+)`)? loaded with (?P<stream_node_count>\d+) nodes"
)

#: How `tatolabd` ends every refusal: a final line on standard error.
TATOLABD_REFUSAL_LINE_PREFIX = "tatolabd: "

RECENT_OUTPUT_CHARACTERS = 6000

REGISTRY_POLL_INTERVAL_SECONDS = 0.05

READER_THREAD_JOIN_TIMEOUT_SECONDS = 5.0


class RuntimeProcessUnderTest:
    """One started `tatolabd` or `tatolab`, its output pumped and its waits bounded."""

    def __init__(
        self,
        process: "subprocess.Popen[str]",
        *,
        command_description: str,
        streamlib_runtime_directory: Path,
        hosting_tatolabd_is_a_child: bool,
    ) -> None:
        assert process.stdout is not None and process.stderr is not None, (
            "the process was started without its output piped"
        )
        self.process = process
        self.command_description = command_description
        self.streamlib_runtime_directory = streamlib_runtime_directory
        self._hosting_tatolabd_is_a_child = hosting_tatolabd_is_a_child
        self.stdout_lines: "list[str]" = []
        self.stderr_lines: "list[str]" = []
        # Reentrant, so a failure message can read the output while a wait holds it.
        self._output_arrived = threading.Condition(threading.RLock())
        self._stdout_ended = False
        self._stderr_ended = False
        self._reader_threads = [
            threading.Thread(
                target=self._pump, args=(process.stdout, self.stdout_lines, "stdout"), daemon=True
            ),
            threading.Thread(
                target=self._pump, args=(process.stderr, self.stderr_lines, "stderr"), daemon=True
            ),
        ]
        for reader_thread in self._reader_threads:
            reader_thread.start()

    def _pump(self, pipe: Any, lines: "list[str]", stream_name: str) -> None:
        for line in pipe:
            with self._output_arrived:
                lines.append(line)
                self._output_arrived.notify_all()
        with self._output_arrived:
            if stream_name == "stdout":
                self._stdout_ended = True
            else:
                self._stderr_ended = True
            self._output_arrived.notify_all()

    @property
    def pid(self) -> int:
        """The started process's own pid."""
        return self.process.pid

    @property
    def stdout_text(self) -> str:
        """Everything read off standard output so far."""
        with self._output_arrived:
            return "".join(self.stdout_lines)

    @property
    def stderr_text(self) -> str:
        """Everything read off standard error so far."""
        with self._output_arrived:
            return "".join(self.stderr_lines)

    def recent_stderr(self) -> str:
        """The tail of standard error, for a failure message."""
        stderr_text = self.stderr_text
        if len(stderr_text) <= RECENT_OUTPUT_CHARACTERS:
            return stderr_text
        return (
            f"[first {len(stderr_text) - RECENT_OUTPUT_CHARACTERS} characters elided]\n"
            f"{stderr_text[-RECENT_OUTPUT_CHARACTERS:]}"
        )

    def _await_stderr_lines_satisfying(
        self,
        stderr_lines_satisfy: "Callable[[list[str]], Any]",
        awaited_description: str,
        timeout: float,
    ) -> Any:
        deadline = time.monotonic() + timeout
        with self._output_arrived:
            while True:
                satisfied = stderr_lines_satisfy(self.stderr_lines)
                if satisfied:
                    return satisfied
                if self._stderr_ended:
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError(
                        f"`{self.command_description}` did not write {awaited_description} "
                        f"within {timeout}s; standard error:\n{self.recent_stderr()}"
                    )
                self._output_arrived.wait(min(remaining, 0.5))
        raise AssertionError(
            f"`{self.command_description}` ended its standard error before "
            f"{awaited_description} (exit status {self.process.poll()}); standard error:\n"
            f"{self.recent_stderr()}"
        )

    def await_stderr_containing(
        self,
        awaited_text: str,
        *,
        timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS,
        occurrence: int = 1,
    ) -> str:
        """Wait until `occurrence` lines of standard error contain `awaited_text`; return that line.

        Lines are counted from the start of the process, so the order two awaits
        are made in implies no order of the lines they wait for.
        """

        def the_awaited_line(stderr_lines: "list[str]") -> "str | None":
            matching_lines = [line for line in stderr_lines if awaited_text in line]
            return matching_lines[occurrence - 1] if len(matching_lines) >= occurrence else None

        return self._await_stderr_lines_satisfying(
            the_awaited_line, f"`{awaited_text}` (occurrence {occurrence})", timeout
        )

    def marker_payloads(self, marker_name: str) -> "list[Any]":
        """The payload of each `MARKER:<marker_name>` line so far, in order.

        A payload is the JSON value following the marker's name, or `None` for a
        marker carrying none.
        """
        with self._output_arrived:
            return _marker_payloads_in(self.stderr_lines, marker_name)

    def await_marker(
        self,
        marker_name: str,
        *,
        timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS,
        occurrence: int = 1,
    ) -> Any:
        """Wait for the `occurrence`-th `MARKER:<marker_name>`, and return its payload."""

        def the_awaited_payload(stderr_lines: "list[str]") -> "list[Any] | None":
            payloads = _marker_payloads_in(stderr_lines, marker_name)
            return [payloads[occurrence - 1]] if len(payloads) >= occurrence else None

        (payload,) = self._await_stderr_lines_satisfying(
            the_awaited_payload, f"{MARKER_PREFIX}{marker_name} (occurrence {occurrence})", timeout
        )
        return payload

    def await_every_marker(
        self, *marker_names: str, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS
    ) -> "dict[str, Any]":
        """Wait until each named marker has been seen, in any order; return each one's first payload."""

        def every_first_payload(stderr_lines: "list[str]") -> "dict[str, Any] | None":
            first_payloads = {}
            for marker_name in marker_names:
                payloads = _marker_payloads_in(stderr_lines, marker_name)
                if not payloads:
                    return None
                first_payloads[marker_name] = payloads[0]
            return first_payloads

        return self._await_stderr_lines_satisfying(
            every_first_payload, f"every one of {sorted(marker_names)}", timeout
        )

    def await_stream_loaded(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS
    ) -> "re.Match[str]":
        """Wait for `tatolabd`'s load line; its groups are `stream_name` and `stream_node_count`."""

        def the_load_line(stderr_lines: "list[str]") -> "re.Match[str] | None":
            for line in stderr_lines:
                loaded = STREAM_LOADED_LOG_LINE_PATTERN.search(line)
                if loaded is not None:
                    return loaded
            return None

        return self._await_stderr_lines_satisfying(the_load_line, "the stream's load line", timeout)

    def send_signal(self, signal_number: int) -> None:
        """Signal the started process itself."""
        self.process.send_signal(signal_number)

    def interrupt(self) -> None:
        """SIGINT, as a terminal's Ctrl-C delivers it."""
        self.send_signal(signal.SIGINT)

    def terminate(self) -> None:
        """SIGTERM."""
        self.send_signal(signal.SIGTERM)

    def await_exit(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> int:
        """Wait for the process to exit and its output to end; return its status.

        A death by signal N is returned as `-N`, as `subprocess` reports it.
        """
        try:
            exit_status = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            raise AssertionError(
                f"`{self.command_description}` did not exit within {timeout}s; standard "
                f"error:\n{self.recent_stderr()}"
            ) from None
        for reader_thread in self._reader_threads:
            reader_thread.join(READER_THREAD_JOIN_TIMEOUT_SECONDS)
        return exit_status

    def await_clean_exit(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> None:
        """Require exit status 0 and the engine's graceful-stop line."""
        exit_status = self.await_exit(timeout=timeout)
        assert exit_status == 0, (
            f"`{self.command_description}` exited {exit_status}, not 0; standard error:\n"
            f"{self.recent_stderr()}"
        )
        assert ENGINE_GRACEFUL_STOP_LOG_LINE in self.stderr_text, (
            f"`{self.command_description}` exited 0 without the engine's "
            f"`{ENGINE_GRACEFUL_STOP_LOG_LINE}`; standard error:\n{self.recent_stderr()}"
        )

    def refusal(self) -> "str | None":
        """`tatolabd`'s refusal: its last `tatolabd: <reason>` line, without the prefix,
        through the end of standard error — a reason quoting an interpreter's
        traceback runs over several lines."""
        with self._output_arrived:
            refusal_line_indexes = [
                index
                for index, line in enumerate(self.stderr_lines)
                if line.startswith(TATOLABD_REFUSAL_LINE_PREFIX)
            ]
            if not refusal_line_indexes:
                return None
            refusal_text = "".join(self.stderr_lines[refusal_line_indexes[-1] :])
        return refusal_text[len(TATOLABD_REFUSAL_LINE_PREFIX) :].rstrip("\n")

    def hosting_tatolabd_process_ids(self) -> "set[int]":
        """The pids a registry entry of this run names: `tatolabd`'s own, or a `tatolab`'s children."""
        if not self._hosting_tatolabd_is_a_child:
            return {self.pid}
        return set(_child_process_ids(self.pid))

    def registry_entry_path(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> Path:
        """Wait for the registry entry naming this run's `tatolabd`, and return its file."""
        deadline = time.monotonic() + timeout
        while True:
            hosting_process_ids = self.hosting_tatolabd_process_ids()
            for entry_path, entry in _registry_entries_in(self.streamlib_runtime_directory):
                if entry.get("pid") in hosting_process_ids:
                    return entry_path
            if self.process.poll() is not None:
                raise AssertionError(
                    f"`{self.command_description}` exited {self.process.returncode} before "
                    f"publishing a registry entry; standard error:\n{self.recent_stderr()}"
                )
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"no registry entry named this run's tatolabd ({sorted(hosting_process_ids)}) "
                    f"in {self.streamlib_runtime_directory / 'nodes'} within {timeout}s; "
                    f"standard error:\n{self.recent_stderr()}"
                )
            time.sleep(REGISTRY_POLL_INTERVAL_SECONDS)

    def registry_entry(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> "dict[str, Any]":
        """Wait for the registry entry naming this run's `tatolabd`, and return it decoded."""
        return json.loads(self.registry_entry_path(timeout=timeout).read_text(encoding="utf-8"))

    def local_api_socket_path(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> Path:
        """The local API socket this run's registry entry names."""
        return Path(self.registry_entry(timeout=timeout)["local_api_socket_path"])

    def local_api_client(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> LocalApiClient:
        """A client of this run's local API, once its socket exists."""
        local_api_socket_path = self.local_api_socket_path(timeout=timeout)
        deadline = time.monotonic() + timeout
        while not local_api_socket_path.exists():
            if time.monotonic() >= deadline:
                raise AssertionError(f"{local_api_socket_path} never appeared")
            time.sleep(REGISTRY_POLL_INTERVAL_SECONDS)
        return LocalApiClient(local_api_socket_path)

    def kill_every_process_it_started(self) -> None:
        """SIGKILL the process, its process group, and every descendant's group.

        A `tatolab` starts `tatolabd` in a process group of its own, and a
        processor interpreter may lead its own, so the group of every descendant
        is killed too. Leaves nothing holding a GPU context, a device or a socket.
        """
        descendant_process_ids = _descendant_process_ids(self.pid)
        for process_id in [self.pid, *descendant_process_ids]:
            try:
                os.killpg(os.getpgid(process_id), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            try:
                os.kill(process_id, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        try:
            self.process.wait(timeout=READER_THREAD_JOIN_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            pass
        for pipe in (self.process.stdout, self.process.stderr, self.process.stdin):
            if pipe is not None and not pipe.closed:
                try:
                    pipe.close()
                except OSError:
                    pass


def _marker_payloads_in(stderr_lines: "list[str]", marker_name: str) -> "list[Any]":
    marker_pattern = re.compile(re.escape(MARKER_PREFIX + marker_name) + r"(?=\s|$)")
    payloads = []
    for line in stderr_lines:
        marker = marker_pattern.search(line)
        if marker is None:
            continue
        after_the_name = line[marker.end() :].lstrip(" ")
        if not after_the_name.strip():
            payloads.append(None)
            continue
        try:
            payload, _ = json.JSONDecoder().raw_decode(after_the_name)
        except json.JSONDecodeError:
            payload = after_the_name.rstrip("\n")
        payloads.append(payload)
    return payloads


def registry_entry_paths_in(streamlib_runtime_directory: Path) -> "list[Path]":
    """Every node registry file under `<runtime directory>/nodes`."""
    nodes_directory = streamlib_runtime_directory / "nodes"
    if not nodes_directory.is_dir():
        return []
    return sorted(nodes_directory.glob("*.json"))


def _registry_entries_in(streamlib_runtime_directory: Path) -> "list[tuple[Path, dict[str, Any]]]":
    entries = []
    for entry_path in registry_entry_paths_in(streamlib_runtime_directory):
        try:
            entries.append((entry_path, json.loads(entry_path.read_text(encoding="utf-8"))))
        except (OSError, ValueError):
            # Half-written: the writer is mid-publish.
            continue
    return entries


def _child_process_ids(parent_process_id: int) -> "list[int]":
    listed = subprocess.run(
        ["pgrep", "-P", str(parent_process_id)], capture_output=True, text=True, check=False
    )
    return [int(process_id) for process_id in listed.stdout.split()]


def _descendant_process_ids(ancestor_process_id: int) -> "list[int]":
    descendants: "list[int]" = []
    frontier = [ancestor_process_id]
    while frontier:
        children = _child_process_ids(frontier.pop())
        descendants.extend(children)
        frontier.extend(children)
    return descendants
