# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Driving `python app.py` from a test, with every wait bounded.

The failures this exists to catch — a surviving process, a hang at interpreter
finalization, a wedged GIL, a non-zero exit — are only visible to a parent, and
a hang is only a *failure* rather than a stuck test run if the parent is the one
holding the clock. Both out-of-process suites drive their apps through here.
"""

import os
import queue
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

# Generous: a cold engine boot stands up an iceoryx2 node, a surface-sharing
# socket, and a GPU context. A real hang blows through it anyway.
ENGINE_READY_TIMEOUT_SECONDS = 60.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0

MARKER_PREFIX = "MARKER:"

ENGINE_READY_LOG_LINE = "[start] Runtime started"
# Emitted early inside `start()` — after the run loop has taken shutdown-signal
# ownership, but well before the graph is up.
ENGINE_STARTING_LOG_LINE = "[start] Initializing GPU context"
ENGINE_STOPPED_LOG_LINE = "[stop] Graceful shutdown complete"


class AppUnderTest:
    """A running `python app.py`, with its output pumped off the pipe.

    Every wait here is bounded. A plain `readline()` blocks indefinitely when
    the app goes quiet, which makes a deadline checked between lines useless —
    that is how a hung app turns into a hung test run rather than a failure with
    a diagnostic.
    """

    def __init__(self, process: "subprocess.Popen[str]"):
        # Asserted rather than assumed: without a pipe the pump below raises
        # inside a daemon thread, and every wait in this class then fails on
        # its timeout instead of on the reason.
        assert process.stdout is not None, "the app was started without a stdout pipe"
        self.process = process
        self._output_pipe = process.stdout
        self.output_lines: list[str] = []
        self._incoming: queue.Queue[str | None] = queue.Queue()
        self._reached_end_of_output = False
        threading.Thread(target=self._pump_output, daemon=True).start()

    def _pump_output(self) -> None:
        for line in self._output_pipe:
            self._incoming.put(line)
        self._incoming.put(None)

    @property
    def output(self) -> str:
        return "".join(self.output_lines)

    def _next_line(self, deadline: float) -> str | None:
        """The next line, or None at end of output. Raises past the deadline."""
        while True:
            if self._reached_end_of_output:
                return None
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError
            try:
                line = self._incoming.get(timeout=min(0.5, remaining))
            except queue.Empty:
                continue
            if line is None:
                self._reached_end_of_output = True
                return None
            self.output_lines.append(line)
            return line

    def await_output_containing(self, awaited: str, what: str) -> None:
        """Wait for a line containing `awaited`.

        Sequencing on the app's or engine's own output rather than a sleep is
        what pins the point of the lifecycle a signal is delivered at.
        """
        deadline = time.monotonic() + ENGINE_READY_TIMEOUT_SECONDS
        while True:
            try:
                line = self._next_line(deadline)
            except TimeoutError:
                raise AssertionError(
                    f"timed out waiting for {what}; output:\n{self.output}"
                ) from None
            if line is None:
                raise AssertionError(f"the app exited before {what}; output:\n{self.output}")
            if awaited in line:
                return

    def await_engine_ready(self) -> None:
        self.await_output_containing(ENGINE_READY_LOG_LINE, "the engine to start")

    def await_marker(self, marker: str) -> None:
        self.await_output_containing(f"{MARKER_PREFIX}{marker}", f"marker {marker}")

    def await_every_marker(self, *markers: str) -> None:
        """Wait until each of `markers` has been seen, in any order.

        Every wait here scans forward only, so awaiting two markers one after
        the other silently requires them to arrive in that order — and two
        probes in two helper processes have no such contract. This consumes
        one stream of lines and ticks them off as they come.
        """
        awaited = {f"{MARKER_PREFIX}{marker}" for marker in markers}
        deadline = time.monotonic() + ENGINE_READY_TIMEOUT_SECONDS
        while awaited:
            what = f"markers {sorted(awaited)}"
            try:
                line = self._next_line(deadline)
            except TimeoutError:
                raise AssertionError(
                    f"timed out waiting for {what}; output:\n{self.output}"
                ) from None
            if line is None:
                raise AssertionError(
                    f"the app exited before {what}; output:\n{self.output}"
                )
            awaited -= {marker for marker in awaited if marker in line}

    def markers(self) -> set[str]:
        # Matched anywhere in the line, not just at its start: while the engine
        # is alive its stdio interceptor captures the app's `print()` and
        # re-emits it inside a tracing record, so a marker emitted before
        # teardown arrives as `… stdio_interceptor — MARKER:X`.
        found: set[str] = set()
        for line in self.output_lines:
            marker_start = line.find(MARKER_PREFIX)
            if marker_start != -1:
                found.add(line[marker_start + len(MARKER_PREFIX) :].strip())
        return found

    def interrupt(self) -> None:
        self.process.send_signal(signal.SIGINT)

    def await_clean_exit(self) -> "AppUnderTest":
        """Drain the remaining output and require a clean, timely exit."""
        deadline = time.monotonic() + CLEAN_EXIT_TIMEOUT_SECONDS
        try:
            while self._next_line(deadline) is not None:
                pass
        except TimeoutError:
            self.process.kill()
            raise AssertionError(
                f"the app did not exit within {CLEAN_EXIT_TIMEOUT_SECONDS}s — engine teardown "
                f"hung, or the interpreter hung at finalization; output:\n{self.output}"
            ) from None

        returncode = self.process.wait(timeout=max(0.0, deadline - time.monotonic()))
        assert returncode == 0, (
            f"expected a clean exit, got returncode {returncode}; output:\n{self.output}"
        )
        assert "CLEAN_EXIT" in self.markers(), (
            f"the app did not reach the end of its scenario; output:\n{self.output}"
        )
        return self


    def kill_process_group(self) -> None:
        """Leave nothing behind, however this app's test ended.

        A failed wait raises without touching the process, and the app runs in
        its own session — so without this an assertion failure strands a live
        engine holding a GPU context, an iceoryx2 node and a socket, silently
        contaminating every later run on the same rig.
        """
        try:
            os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        for pipe in (self.process.stdout, self.process.stdin):
            if pipe is not None and not pipe.closed:
                pipe.close()


def start_command(
    command: "list[str]", *, working_directory: "Path | None" = None
) -> AppUnderTest:
    """Spawn `command` in its own process group, output on one pipe.

    Its own group so a SIGINT aimed at the app cannot reach the test runner that
    spawned it — and so the group can be checked for survivors afterwards.

    `working_directory` stays unset unless an arrangement genuinely needs it: a
    helper child inherits the app's cwd, and cwd leads `sys.path` in the child's
    `-m` launch, so pointing it at the processor modules would import them for
    free and mask a break in the `PYTHONPATH` the spawn host sets.
    """
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        start_new_session=True,
        cwd=None if working_directory is None else str(working_directory),
    )
    return AppUnderTest(process)


def start_app(app_path: Path, *arguments: str) -> AppUnderTest:
    """Spawn `python <app_path> <arguments...>` — the arrangement the
    interpreter-lifecycle contract was proven against.

    A script path puts the script's own directory on `sys.path`, so this needs
    no working directory of its own.
    """
    return start_command([sys.executable, str(app_path), *arguments])


def start_app_as_module(app_path: Path, *arguments: str) -> AppUnderTest:
    """Spawn `python -m <app> <arguments...>` from the app's own directory.

    A different arrangement, not a different app. `-m` resolves the module
    against the *working directory* where a script path resolves against the
    file, which is the only reason this arm sets one — and why it is the only
    arm that does.
    """
    return start_command(
        [sys.executable, "-m", app_path.stem, *arguments],
        working_directory=app_path.parent,
    )


def start_app_under_the_streamlib_cli(app_path: Path, *arguments: str) -> AppUnderTest:
    """Spawn `streamlib dev -f <app_path>` — the launcher the MVP blesses.

    Reached as a module rather than through the console script so the app runs
    under the interpreter this test session is using, whether or not the venv's
    `bin` is on PATH. The launcher puts the entry file's own directory on
    `sys.path` itself, so this arm needs no working directory either.
    """
    return start_command(
        [sys.executable, "-m", "streamlib.cli", "dev", "-f", str(app_path), *arguments]
    )
