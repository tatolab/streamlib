# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The interpreter-lifecycle contract, proven against a real `python app.py`.

The arrangement under test is the wheel's: CPython starts, imports the engine,
and drives it in-process. Every assertion here is made from outside that
process, because the failures being ruled out — a surviving process, a hang at
interpreter finalization, a non-zero exit — are only visible to a parent.
"""

import os
import queue
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

APP_UNDER_TEST = Path(__file__).parent / "interpreter_lifecycle_app.py"

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

    def __init__(self, process: subprocess.Popen):
        self.process = process
        self.output_lines: list[str] = []
        self._incoming: queue.Queue[str | None] = queue.Queue()
        self._reached_end_of_output = False
        threading.Thread(target=self._pump_output, daemon=True).start()

    def _pump_output(self) -> None:
        for line in self.process.stdout:
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


@pytest.fixture
def app_under_test():
    """Hands out apps and kills their process groups no matter how a test ends."""
    started: list[AppUnderTest] = []

    def start(scenario: str) -> AppUnderTest:
        app = start_app(scenario)
        started.append(app)
        return app

    try:
        yield start
    finally:
        for app in started:
            app.kill_process_group()


def start_app(scenario: str) -> AppUnderTest:
    process = subprocess.Popen(
        [sys.executable, str(APP_UNDER_TEST), scenario],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        # Its own process group, so a SIGINT aimed at the app cannot reach the
        # test runner that spawned it — and so the group can be checked for
        # survivors after it exits.
        start_new_session=True,
    )
    return AppUnderTest(process)


def run_scenario_to_completion(start, scenario: str) -> AppUnderTest:
    """Run a scenario that ends on its own."""
    return start(scenario).await_clean_exit()


def run_scenario_interrupted_once_running(start, scenario: str) -> AppUnderTest:
    """Boot the scenario, wait for the engine's ready line, then Ctrl-C it."""
    app = start(scenario)
    app.await_engine_ready()
    app.interrupt()
    return app.await_clean_exit()


@pytest.mark.requires_gpu
def test_ctrl_c_exits_cleanly(app_under_test):
    """The demo: Ctrl-C on a running pipeline returns from `run()` and exits 0.

    This is the arrangement the #1702 spike never ran — there, Rust embedded
    CPython; here CPython imports the engine.
    """
    app = run_scenario_interrupted_once_running(app_under_test, "ctrl_c")
    assert "RUN_RETURNED" in app.markers(), (
        f"run() must return on Ctrl-C rather than raising; output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_ctrl_c_during_startup_still_exits_cleanly(app_under_test):
    """Ctrl-C while the graph is still coming up must still exit cleanly.

    Regression lock on a real hang: signal ownership used to begin inside the
    wait loop, so a SIGINT during `start()` landed on CPython's handler. With
    the GIL released for the whole of `run()`, the flag CPython sets can never
    be turned into a `KeyboardInterrupt` — the app blocked forever rather than
    shutting down. Mental-revert: moving ownership back inside the wait loop
    (`start()` then `wait_for_signal()`) hangs here until the timeout.
    """
    app = app_under_test("ctrl_c")
    app.await_output_containing(ENGINE_STARTING_LOG_LINE, "the engine to begin starting")
    app.interrupt()

    app.await_clean_exit()
    assert "RUN_RETURNED" in app.markers(), (
        f"run() must return on a Ctrl-C taken during startup; output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_no_survivors_are_left_in_the_process_group(app_under_test):
    """The app's whole process group must be gone, not just its leader.

    A `waitpid` check cannot show this — the parent has already reaped the
    child, so it raises `ChildProcessError` for any exited process whatsoever.
    The engine spawns threads rather than children today, so this is a forward
    lock for #1714, which places processors in child interpreters.
    """
    app = app_under_test("ctrl_c")
    process_group = os.getpgid(app.process.pid)
    app.await_engine_ready()
    app.interrupt()
    app.await_clean_exit()

    # Signal 0 delivers nothing; it only asks whether the group still exists.
    with pytest.raises(ProcessLookupError):
        os.killpg(process_group, 0)


@pytest.mark.requires_gpu
def test_the_gil_is_released_while_run_blocks(app_under_test):
    """A Python thread must keep running while `run()` blocks.

    Mental-revert: dropping the `python.detach` around the run loop pins the
    GIL for the whole run and leaves the counter at zero.
    """
    app = run_scenario_interrupted_once_running(app_under_test, "gil_released")

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
def test_sigint_is_handed_back_to_cpython(app_under_test):
    """After `run()` returns, Ctrl-C must raise KeyboardInterrupt again.

    Mental-revert: leaving the engine's handler installed swallows the second
    SIGINT and the app reports SIGINT_WAS_SWALLOWED instead.
    """
    app = run_scenario_interrupted_once_running(app_under_test, "sigint_handed_back")
    assert "KEYBOARD_INTERRUPT_RAISED" in app.markers(), (
        f"SIGINT after run() must reach CPython's handler; output:\n{app.output}"
    )


def test_an_exception_still_tears_the_engine_down(app_under_test):
    """The exception path keeps the teardown guarantee and re-raises."""
    app = run_scenario_to_completion(app_under_test, "exception_in_context_manager")
    assert "EXCEPTION_PROPAGATED" in app.markers(), (
        f"__exit__ must not suppress the exception; output:\n{app.output}"
    )


def test_a_runtime_that_never_runs_does_not_hang_at_exit(app_under_test):
    """A booted-but-unrun engine shuts down without hanging the interpreter.

    This is the `Drop` path: the scenario leaves the Runtime unbound, so it is
    refcounted to zero on the spot. The at-exit case is the test below.
    """
    app = run_scenario_to_completion(app_under_test, "never_run")
    assert "CONSTRUCTED_WITHOUT_RUNNING" in app.markers(), (
        f"the scenario did not run; output:\n{app.output}"
    )


def test_a_runtime_held_by_a_live_thread_is_torn_down_at_exit(app_under_test):
    """The `atexit` hook's lock: a Runtime CPython will never collect.

    The reference lives in a parked daemon thread's frame, which interpreter
    shutdown does not unwind — so unlike the never-run case, `Drop` never fires
    on its own. Asserting the engine's own teardown line rather than a scenario
    marker is what makes this a lock: a clean exit code alone is also what you
    get when no teardown runs at all.

    Mental-revert: removing `@atexit.register` from `streamlib/__init__.py`
    leaves the shutdown line absent entirely.
    """
    app = run_scenario_to_completion(app_under_test, "held_by_a_live_thread_at_exit")
    assert "HELD_BY_LIVE_THREAD_AT_EXIT" in app.markers(), (
        f"the scenario did not run; output:\n{app.output}"
    )
    assert ENGINE_STOPPED_LOG_LINE in app.output, (
        f"the engine was never torn down — its threads outlived interpreter "
        f"finalization; output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_a_second_pipeline_in_one_process_still_blocks(app_under_test):
    """Two run loops in one interpreter: the second must not inherit the first's
    shutdown request.

    Regression lock on a real defect. The shutdown-request latch is
    process-global and first-observer-wins, so a `shutdown()` issued once the
    engine was already torn down — which `__exit__` does on every
    `with streamlib.Runtime()` block — left it set, and the next `run()`
    returned immediately having run nothing.

    The second run is timed by the app against a monotonic clock rather than
    waited on via the engine's log, because the engine only initializes its
    logging pathway once per process: a second `Runner` in the same process
    emits nothing at all.

    Mental-revert: dropping the `run_loop_is_blocking` guard in `shutdown()`
    makes the second run return in milliseconds and fails the threshold below.
    """
    app = app_under_test("two_pipelines_in_one_process")

    app.await_engine_ready()
    app.interrupt()
    app.await_marker("PIPELINE_1_RETURNED")

    # The second engine is silent, so the app announces its own readiness.
    app.await_marker("PIPELINE_2_RUNNING")
    # Held open deliberately: `run()` also spends time in `start()`, so only a
    # span longer than a boot distinguishes "blocked until interrupted" from
    # "returned on a stale request as soon as the graph was up".
    time.sleep(SECOND_PIPELINE_OBSERVATION_WINDOW_SECONDS)
    app.interrupt()

    app.await_clean_exit()

    blocked_milliseconds = next(
        int(marker.removeprefix("PIPELINE_2_BLOCKED_MS="))
        for marker in app.markers()
        if marker.startswith("PIPELINE_2_BLOCKED_MS=")
    )
    assert blocked_milliseconds >= SECOND_PIPELINE_MINIMUM_BLOCKED_MILLISECONDS, (
        f"the second run loop returned after only {blocked_milliseconds}ms — it inherited "
        f"the first pipeline's shutdown request instead of blocking; output:\n{app.output}"
    )


# How long the second pipeline is left running before it is interrupted, and
# the floor its measured span must clear. The floor sits well above a cold
# engine boot so a run that returned on a stale shutdown request cannot reach
# it, and well below the window so ordinary scheduling jitter cannot fail it.
SECOND_PIPELINE_OBSERVATION_WINDOW_SECONDS = 3.0
SECOND_PIPELINE_MINIMUM_BLOCKED_MILLISECONDS = 2000


@pytest.mark.requires_gpu
def test_shutdown_spun_across_the_run_loop_exit_does_not_poison_the_next(app_under_test):
    """A `shutdown()` racing the run loop's exit must not reach the next loop.

    The defect this covers is real and was reproduced independently at ~350ms in
    2 of 3 trials: a check-then-act guard lets a worker read "still running",
    release the GIL, and issue its request after the run loop stopped observing,
    so the next pipeline inherits it.

    Honest scope: this is a concurrent smoke test, NOT a regression lock. The
    fix is structural — the request is issued under the same lock `run()` takes
    to transition and clear the latch, so the interleaving cannot occur — and
    reintroducing the racy shape does not make this test red on this rig (3 of 3
    green). Treat a failure here as real; do not read a pass as proof.
    """
    app = app_under_test("shutdown_spun_across_the_run_loop_exit")
    app.await_engine_ready()

    # Starts the spinner; it keeps calling shutdown() straight through the exit.
    app.process.stdin.close()
    app.process.stdin = None

    app.await_marker("PIPELINE_1_RETURNED")
    app.await_marker("PIPELINE_2_RUNNING")
    time.sleep(SECOND_PIPELINE_OBSERVATION_WINDOW_SECONDS)
    app.interrupt()

    app.await_clean_exit()

    blocked_milliseconds = next(
        int(marker.removeprefix("PIPELINE_2_BLOCKED_MS="))
        for marker in app.markers()
        if marker.startswith("PIPELINE_2_BLOCKED_MS=")
    )
    assert blocked_milliseconds >= SECOND_PIPELINE_MINIMUM_BLOCKED_MILLISECONDS, (
        f"the second run loop returned after only {blocked_milliseconds}ms — a shutdown() "
        f"racing the first loop's exit escaped into it; output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_a_second_run_is_refused(app_under_test):
    """`run()` drops the engine to keep its teardown promise, so the handle is
    single-use and says so."""
    app = run_scenario_interrupted_once_running(app_under_test, "second_run_refused")
    assert "SECOND_RUN_REFUSED" in app.markers(), (
        f"a second run() must raise rather than silently do nothing; output:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_shutdown_from_another_thread_ends_a_blocking_run(app_under_test):
    """`shutdown()` must end a running pipeline, from any thread.

    The programmatic stop, with no signal involved. Two ways this regresses:
    marking the pyclass `unsendable` makes the cross-thread call raise
    `PanicException`, and letting `shutdown()` return early when `run()` owns
    the engine makes it a silent no-op that never ends the run.
    """
    app = app_under_test("shutdown_from_another_thread")
    app.await_engine_ready()

    # Closing stdin is the readiness handshake the app waits on, so the
    # shutdown request cannot land before the engine is up.
    app.process.stdin.close()

    app.await_clean_exit()
    assert "STOPPED_FROM_ANOTHER_THREAD" in app.markers(), (
        f"shutdown() from a worker thread must end run(); output:\n{app.output}"
    )
