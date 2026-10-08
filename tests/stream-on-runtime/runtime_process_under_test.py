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

#: The ` key=value` fields the pretty log mirror appends to a record's message.
TRAILING_LOG_RECORD_FIELDS_PATTERN = re.compile(
    r"(?:^|\s+)[A-Za-z_]\w*=\S*(?:\s+[A-Za-z_]\w*=\S*)*\s*$"
)

#: What an observer of standard error returns until it has seen what it waits for.
NOT_YET_SEEN = object()

#: What `MarkerLineParser.payload_of` returns for a line without its marker.
NOT_A_MARKER_LINE = object()


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
        #: When the last of standard output and standard error ended, monotonic.
        self._output_ended_at: "float | None" = None
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
            if self._stdout_ended and self._stderr_ended:
                self._output_ended_at = time.monotonic()
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

    def _await_stderr_line_satisfying(
        self,
        observe_stderr_line: "Callable[[str], Any]",
        awaited_description: str,
        timeout: float,
    ) -> Any:
        """Feed each line of standard error, from the first, to `observe_stderr_line`
        until it returns anything but `NOT_YET_SEEN`, and return that.

        Each line is observed once and outside the lock, so a wait on a chatty
        run never holds up the reader threads appending to it.
        """
        deadline = time.monotonic() + timeout
        observed_line_count = 0
        while True:
            with self._output_arrived:
                unobserved_lines = self.stderr_lines[observed_line_count:]
                stderr_ended = self._stderr_ended
            observed_line_count += len(unobserved_lines)
            for line in unobserved_lines:
                observed = observe_stderr_line(line)
                if observed is not NOT_YET_SEEN:
                    return observed
            if stderr_ended:
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError(
                    f"`{self.command_description}` did not write {awaited_description} "
                    f"within {timeout}s; standard error:\n{self.recent_stderr()}"
                )
            with self._output_arrived:
                if len(self.stderr_lines) == observed_line_count and not self._stderr_ended:
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
        matching_line_count = 0

        def observe(line: str) -> Any:
            nonlocal matching_line_count
            if awaited_text not in line:
                return NOT_YET_SEEN
            matching_line_count += 1
            return line if matching_line_count == occurrence else NOT_YET_SEEN

        return self._await_stderr_line_satisfying(
            observe, f"`{awaited_text}` (occurrence {occurrence})", timeout
        )

    def marker_payloads(self, marker_name: str) -> "list[Any]":
        """The payload of each `MARKER:<marker_name>` line so far, in order.

        A payload is the JSON value following the marker's name, or `None` for a
        marker carrying none; see `MarkerLineParser`.
        """
        marker_line_parser = MarkerLineParser(marker_name)
        with self._output_arrived:
            stderr_lines = list(self.stderr_lines)
        return [
            payload
            for payload in map(marker_line_parser.payload_of, stderr_lines)
            if payload is not NOT_A_MARKER_LINE
        ]

    def await_marker(
        self,
        marker_name: str,
        *,
        timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS,
        occurrence: int = 1,
    ) -> Any:
        """Wait for the `occurrence`-th `MARKER:<marker_name>`, and return its payload."""
        marker_line_parser = MarkerLineParser(marker_name)
        marker_line_count = 0

        def observe(line: str) -> Any:
            nonlocal marker_line_count
            payload = marker_line_parser.payload_of(line)
            if payload is NOT_A_MARKER_LINE:
                return NOT_YET_SEEN
            marker_line_count += 1
            return payload if marker_line_count == occurrence else NOT_YET_SEEN

        return self._await_stderr_line_satisfying(
            observe, f"{MARKER_PREFIX}{marker_name} (occurrence {occurrence})", timeout
        )

    def await_every_marker(
        self, *marker_names: str, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS
    ) -> "dict[str, Any]":
        """Wait until each named marker has been seen, in any order; return each one's first payload."""
        marker_line_parsers = [MarkerLineParser(marker_name) for marker_name in marker_names]
        first_payloads: "dict[str, Any]" = {}

        def observe(line: str) -> Any:
            for marker_line_parser in marker_line_parsers:
                if marker_line_parser.marker_name in first_payloads:
                    continue
                payload = marker_line_parser.payload_of(line)
                if payload is not NOT_A_MARKER_LINE:
                    first_payloads[marker_line_parser.marker_name] = payload
            return first_payloads if len(first_payloads) == len(marker_names) else NOT_YET_SEEN

        return self._await_stderr_line_satisfying(
            observe, f"every one of {sorted(marker_names)}", timeout
        )

    def await_stream_loaded(
        self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS
    ) -> "re.Match[str]":
        """Wait for `tatolabd`'s load line; its groups are `stream_name` and `stream_node_count`."""

        def observe(line: str) -> Any:
            loaded = STREAM_LOADED_LOG_LINE_PATTERN.search(line)
            return NOT_YET_SEEN if loaded is None else loaded

        return self._await_stderr_line_satisfying(observe, "the stream's load line", timeout)

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

    def await_end_of_output(self, *, timeout: float = DEFAULT_RUNTIME_WAIT_TIMEOUT_SECONDS) -> float:
        """Wait until both standard output and standard error have ended; return when, monotonic.

        A pipe ends only once every process holding its write end has closed it,
        so this is later than the process's own exit when something it started
        still holds its output.
        """
        deadline = time.monotonic() + timeout
        with self._output_arrived:
            while self._output_ended_at is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError(
                        f"the output of `{self.command_description}` did not end within "
                        f"{timeout}s (exit status {self.process.poll()}); standard error:\n"
                        f"{self.recent_stderr()}"
                    )
                self._output_arrived.wait(min(remaining, 0.5))
            return self._output_ended_at

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


class MarkerLineParser:
    """Reads the payload off a `MARKER:<marker_name>` log line.

    The marker is matched anywhere in the line, with whitespace or the line's
    end after its name. The pretty log mirror ends a record with its structured
    fields — ` processor_id=<id>`, ` pipeline_id=<id>`, an attribute's
    ` key=value` — so the payload is the JSON value after the name, decoded up
    to where it ends; `None` when only those fields follow; or, when what
    follows is not JSON, that text with the trailing fields removed.
    """

    def __init__(self, marker_name: str) -> None:
        self.marker_name = marker_name
        self._marker_pattern = re.compile(re.escape(MARKER_PREFIX + marker_name) + r"(?=\s|$)")
        self._json_decoder = json.JSONDecoder()

    def payload_of(self, line: str) -> Any:
        """The marker's payload, or `NOT_A_MARKER_LINE` when the line carries no such marker."""
        marker = self._marker_pattern.search(line)
        if marker is None:
            return NOT_A_MARKER_LINE
        after_the_name = line[marker.end() :].strip()
        without_the_record_fields = TRAILING_LOG_RECORD_FIELDS_PATTERN.sub("", after_the_name)
        if not without_the_record_fields:
            return None
        try:
            payload, _ = self._json_decoder.raw_decode(after_the_name)
        except json.JSONDecodeError:
            return without_the_record_fields
        return payload


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
