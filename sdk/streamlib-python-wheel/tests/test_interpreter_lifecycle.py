# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The interpreter-lifecycle contract, proven against a real `python app.py`.

The arrangement under test is the wheel's: CPython starts, imports the engine,
and drives it in-process. Every assertion here is made from outside that
process, because the failures being ruled out — a zombie, a hang at
interpreter finalization, a non-zero exit — are only visible to a parent.
"""

import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

APP_UNDER_TEST = Path(__file__).parent / "interpreter_lifecycle_app.py"

# Generous: a cold engine boot stands up an iceoryx2 node, a surface-sharing
# socket, and a GPU context. A real hang blows through it anyway.
ENGINE_READY_TIMEOUT_SECONDS = 60.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0

ENGINE_READY_LOG_LINE = "[start] Runtime started"
# Emitted early inside `start()` — after the run loop has taken shutdown-signal
# ownership, but well before the graph is up.
ENGINE_STARTING_LOG_LINE = "[start] Initializing GPU context"


class AppUnderTest:
    """A running `python app.py`, with its stdout captured line by line."""

    def __init__(self, process: subprocess.Popen, output_lines: list[str]):
        self.process = process
        self.output_lines = output_lines

    @property
    def output(self) -> str:
        return "".join(self.output_lines)

    def markers(self) -> set[str]:
        return {
            line.strip().removeprefix("MARKER:")
            for line in self.output_lines
            if line.startswith("MARKER:")
        }


def start_app(scenario: str) -> subprocess.Popen:
    return subprocess.Popen(
        [sys.executable, str(APP_UNDER_TEST), scenario],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        # Its own process group, so a SIGINT aimed at the app cannot reach the
        # test runner that spawned it.
        start_new_session=True,
    )


def read_until_log_line(process: subprocess.Popen, awaited_line: str) -> list[str]:
    """Collect output until the engine logs `awaited_line`.

    Waiting for the engine's own log line rather than sleeping pins exactly
    which point of the lifecycle a signal is delivered at.
    """
    collected: list[str] = []
    deadline = time.monotonic() + ENGINE_READY_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if not line:
            raise AssertionError(
                f"the app exited before logging {awaited_line!r}; "
                f"output:\n{''.join(collected)}"
            )
        collected.append(line)
        if awaited_line in line:
            return collected
    raise AssertionError(
        f"the engine never logged {awaited_line!r} within {ENGINE_READY_TIMEOUT_SECONDS}s; "
        f"output:\n{''.join(collected)}"
    )


def read_until_engine_ready(process: subprocess.Popen) -> list[str]:
    return read_until_log_line(process, ENGINE_READY_LOG_LINE)


def await_clean_exit(process: subprocess.Popen, already_read: list[str]) -> AppUnderTest:
    """Drain the app's remaining output and require a clean, timely exit."""
    try:
        remaining_output, _ = process.communicate(timeout=CLEAN_EXIT_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        remaining_output, _ = process.communicate()
        raise AssertionError(
            f"the app did not exit within {CLEAN_EXIT_TIMEOUT_SECONDS}s — engine teardown "
            f"hung, or the interpreter hung at finalization; output:\n"
            f"{''.join(already_read)}{remaining_output}"
        )

    app = AppUnderTest(process, already_read + remaining_output.splitlines(keepends=True))
    assert process.returncode == 0, (
        f"expected a clean exit, got returncode {process.returncode}; output:\n{app.output}"
    )
    assert "CLEAN_EXIT" in app.markers(), (
        f"the app did not reach the end of its scenario; output:\n{app.output}"
    )
    return app


def run_scenario_to_completion(scenario: str, *, starts_engine: bool = True) -> AppUnderTest:
    """Run a scenario that ends on its own.

    `starts_engine=False` for scenarios that construct a Runtime but never call
    `run()` — there is no ready line to wait for, and waiting would just read to
    EOF.
    """
    process = start_app(scenario)
    already_read = read_until_engine_ready(process) if starts_engine else []
    return await_clean_exit(process, already_read)


@pytest.mark.requires_gpu
def test_ctrl_c_exits_cleanly():
    """The demo: Ctrl-C on a running pipeline returns from `run()` and exits 0.

    This is the arrangement the #1702 spike never ran — there, Rust embedded
    CPython; here CPython imports the engine.
    """
    process = start_app("ctrl_c")
    already_read = read_until_engine_ready(process)

    process.send_signal(signal.SIGINT)

    app = await_clean_exit(process, already_read)
    assert "RUN_RETURNED" in app.markers(), (
        f"run() must return on Ctrl-C rather than raising; output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_ctrl_c_during_startup_still_exits_cleanly():
    """Ctrl-C while the graph is still coming up must still exit cleanly.

    Regression lock on a real hang: signal ownership used to begin inside the
    wait loop, so a SIGINT during `start()` landed on CPython's handler. With
    the GIL released for the whole of `run()`, the flag CPython sets can never
    be turned into a `KeyboardInterrupt` — the app blocked forever rather than
    shutting down. Mental-revert: moving ownership back inside the wait loop
    (`start()` then `wait_for_signal()`) hangs here until the timeout.
    """
    process = start_app("ctrl_c")
    already_read = read_until_log_line(process, ENGINE_STARTING_LOG_LINE)

    process.send_signal(signal.SIGINT)

    app = await_clean_exit(process, already_read)
    assert "RUN_RETURNED" in app.markers(), (
        f"run() must return on a Ctrl-C taken during startup; output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_no_zombie_is_left_behind():
    """A clean exit must also mean a reaped process, not a zombie."""
    process = start_app("ctrl_c")
    already_read = read_until_engine_ready(process)
    process.send_signal(signal.SIGINT)
    await_clean_exit(process, already_read)

    with pytest.raises(ChildProcessError):
        os.waitpid(process.pid, 0)


@pytest.mark.requires_gpu
def test_the_gil_is_released_while_run_blocks():
    """A Python thread must keep running while `run()` blocks.

    Mental-revert: dropping the `python.detach` around the run loop pins the
    GIL for the whole run and leaves the counter at zero.
    """
    app = run_scenario_to_completion("gil_released")

    ticks = next(
        int(marker.removeprefix("PYTHON_THREAD_TICKS="))
        for marker in app.markers()
        if marker.startswith("PYTHON_THREAD_TICKS=")
    )
    assert ticks > 0, (
        f"a Python thread made no progress while run() blocked — the GIL was held; "
        f"output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_sigint_is_handed_back_to_cpython():
    """After `run()` returns, Ctrl-C must raise KeyboardInterrupt again.

    Mental-revert: leaving the engine's handler installed swallows the second
    SIGINT and the app reports SIGINT_WAS_SWALLOWED instead.
    """
    app = run_scenario_to_completion("sigint_handed_back")
    assert "KEYBOARD_INTERRUPT_RAISED" in app.markers(), (
        f"SIGINT after run() must reach CPython's handler; output:\n{app.output}"
    )


def test_an_exception_still_tears_the_engine_down():
    """The exception path keeps the teardown guarantee and re-raises."""
    app = run_scenario_to_completion("exception_in_context_manager", starts_engine=False)
    assert "EXCEPTION_PROPAGATED" in app.markers(), (
        f"__exit__ must not suppress the exception; output:\n{app.output}"
    )


def test_a_runtime_that_never_runs_does_not_hang_at_exit():
    """The `atexit` half of the guarantee: a booted-but-unrun engine still
    shuts down before the interpreter finalizes."""
    app = run_scenario_to_completion("never_run", starts_engine=False)
    assert "CONSTRUCTED_WITHOUT_RUNNING" in app.markers(), (
        f"the scenario did not run; output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_a_second_run_is_refused():
    """`run()` drops the engine to keep its teardown promise, so the handle is
    single-use and says so."""
    app = run_scenario_to_completion("second_run_refused")
    assert "SECOND_RUN_REFUSED" in app.markers(), (
        f"a second run() must raise rather than silently do nothing; output:\n{app.output}"
    )
