# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The interpreter-lifecycle contract, proven against a real `python app.py`.

The arrangement under test is the wheel's: CPython starts, imports the engine,
and drives it in-process. Every assertion here is made from outside that
process, because the failures being ruled out — a surviving process, a hang at
interpreter finalization, a non-zero exit — are only visible to a parent.
"""

import contextlib
import os
import re
import signal
import sys
import threading
import time
from pathlib import Path

import pytest

from app_under_test import (
    ENGINE_STARTING_LOG_LINE,
    ENGINE_STOPPED_LOG_LINE,
    AppUnderTest,
)
from interpreter_lifecycle_processors import TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE

APP_UNDER_TEST = Path(__file__).parent / "interpreter_lifecycle_app.py"


@pytest.fixture
def app_under_test(start_app_under_test):
    """Starts this suite's app; the shared fixture owns the cleanup."""
    return lambda scenario: start_app_under_test(APP_UNDER_TEST, scenario)


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
    It reaches only what the app process itself started inside its own group:
    every helper process leads a group of its own, and its survival is asserted
    by pid in `test_helper_placement.py`.
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


@pytest.mark.linux_only_capability(reason="only Linux hands SIGINT back when a run ends")
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

    Regression lock on a real defect. The shutdown escalation is process-global
    and taken only when a run ends, so a `shutdown()` issued once the
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
    to transition, and the escalation is cleared only after that transition, so
    the interleaving cannot occur — and
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
def test_a_readiness_wait_does_not_break_the_teardown_contract(app_under_test):
    """The readiness wait is reachable while `run()` blocks, and `run()` still
    returns clean afterwards.

    The wait has to reach the engine the run loop owns, which is the shape the
    teardown contract forbids outliving the run. What keeps it legal is that
    the binding upgrades its reference only long enough to take the processor
    states and drops it before waiting. Mentally hold that `Arc` across the
    wait instead and the hazard is back: teardown's `Arc::into_inner` can find
    the engine still borrowed and `run()` raises rather than returning. This
    test does not reproduce that interleaving — it pins the everyday path,
    that reaching the engine mid-run leaves teardown intact.
    """
    app = app_under_test("readiness_wait_across_teardown")
    app.await_engine_ready()

    # The readiness wait cannot begin until `run()` holds the engine, so the
    # driver opens the gate only once the engine reports itself up.
    app.process.stdin.close()

    app.await_clean_exit()
    assert "GRAPH_READY" in app.markers(), (
        f"the readiness wait must return while run() blocks; output:\n{app.output}"
    )
    assert "RUN_RETURNED" in app.markers(), (
        f"run() must return normally after a readiness wait; output:\n{app.output}"
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


# How long an app with a helper asleep in its callback may take to end after a
# Ctrl-C: the one-second callback budget, the helper's own `stop()` and
# `teardown()`, and the engine's drop. The plan's "about two seconds or less",
# with room for a loaded rig.
ASLEEP_HELPER_EXIT_BUDGET_SECONDS = 3.0

HELPER_STARTED_MARKER = re.compile(r"helper process started: pid=(\d+)")
ASLEEP_HELPER_PID_MARKER = re.compile(r"MARKER:ASLEEP_IN_PROCESS (\d+)")
TEARDOWN_WORKER_PID_MARKER = re.compile(r"MARKER:TEARDOWN_WORKER_PID (\d+)")
SURVIVOR_PID_MARKER = re.compile(r"MARKER:SURVIVOR_PID=(\d+)")


def a_pid_is_gone_within(pid: int, budget_seconds: float) -> bool:
    """Whether `pid` has stopped existing inside `budget_seconds`.

    Signal 0 rather than a wait: the processes this suite looks for are not this
    test's children.
    """
    deadline = time.monotonic() + budget_seconds
    while True:
        try:
            os.kill(pid, 0)
        except (ProcessLookupError, PermissionError):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


def matched_pid(pattern: "re.Pattern[str]", app: AppUnderTest) -> int:
    match = pattern.search(app.output)
    assert match is not None, f"the app never reported {pattern.pattern!r}:\n{app.output}"
    return int(match.group(1))


@pytest.mark.requires_gpu
def test_ctrl_c_with_a_processor_asleep_in_its_callback_exits_in_about_two_seconds(
    app_under_test,
):
    """A helper asleep in `process()` costs one interrupt, and its `teardown()` runs."""
    app = app_under_test("a_processor_asleep_in_its_callback")
    app.await_output_containing("MARKER:ASLEEP_IN_PROCESS", "the processor to park")
    interrupted_at = time.monotonic()
    app.interrupt()
    app.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert "MARKER:ASLEEP_PROBE_TORE_DOWN" in app.output, (
        f"`teardown()` did not run after the interrupt:\n{app.output}"
    )
    assert ended_in < ASLEEP_HELPER_EXIT_BUDGET_SECONDS, (
        f"the app took {ended_in:.1f}s to end after Ctrl-C:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_three_helpers_slow_to_stop_cost_about_one_ladder(app_under_test):
    """Every helper walks its ladder at the same time.

    Each takes the one-second callback budget and three seconds of teardown, so
    one after another is over twelve seconds and at once is about four.
    """
    app = app_under_test("three_processors_slow_to_tear_down")
    for _ in range(3):
        app.await_output_containing(
            "MARKER:SLOW_TO_TEAR_DOWN_ASLEEP", "every helper to park in its callback"
        )
    interrupted_at = time.monotonic()
    app.interrupt()
    app.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert app.output.count("MARKER:SLOW_TEARDOWN_FINISHED") == 3, (
        f"every helper's `teardown()` must still run:\n{app.output}"
    )
    assert ended_in < 8.0, (
        f"three helpers took {ended_in:.1f}s to stop, which is one after another:\n"
        f"{app.output}"
    )


@pytest.mark.requires_gpu
def test_a_second_ctrl_c_forces_the_shutdown_past_a_long_teardown(app_under_test):
    """The second interrupt terminates a helper still inside its `teardown()`,
    and `run()` returns normally, having abandoned nothing."""
    app = app_under_test("a_teardown_only_a_forced_shutdown_cuts_short")
    app.await_output_containing("MARKER:TEARDOWN_WORKER_PID", "the helper to fork its worker")
    app.interrupt()
    app.await_output_containing("MARKER:LONG_TEARDOWN_BEGAN", "the helper's teardown to begin")
    forced_at = time.monotonic()
    app.interrupt()
    app.await_clean_exit()
    ended_in = time.monotonic() - forced_at

    assert "RUN_RETURNED" in app.markers(), (
        f"a forced shutdown that abandoned nothing must return normally:\n{app.output}"
    )
    assert "MARKER:LONG_TEARDOWN_FINISHED" not in app.output, (
        f"the teardown ran to its end, so the second interrupt forced nothing:\n{app.output}"
    )
    assert ended_in < 3.0, (
        f"the app took {ended_in:.1f}s to end after the second Ctrl-C:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_a_third_ctrl_c_kills_every_helper_process_group_and_exits_130(app_under_test):
    """The third interrupt exits at once, taking the helper's group with it.

    The helper and its worker both ignore SIGTERM, so the forced ladder waits
    out its half-second grace before it sends the group SIGKILL. The third
    interrupt lands well inside that grace, so the worker being gone is the third
    interrupt's doing: the kernel's parent-death signal reaches the helper, never
    a process the helper forked.
    """
    app = app_under_test("a_teardown_only_a_forced_shutdown_cuts_short")
    app.await_output_containing("MARKER:TEARDOWN_WORKER_PID", "the helper to fork its worker")
    app.interrupt()
    app.await_output_containing("MARKER:LONG_TEARDOWN_BEGAN", "the helper's teardown to begin")
    app.interrupt()
    time.sleep(0.05)
    app.interrupt()
    exit_status = app.await_exit_status()

    assert exit_status == 130, (
        f"a third Ctrl-C must exit with status 130, got {exit_status}:\n{app.output}"
    )
    assert "RUN_RETURNED" not in app.markers(), (
        f"`run()` returned, so the process did not exit at once:\n{app.output}"
    )
    worker_pid = matched_pid(TEARDOWN_WORKER_PID_MARKER, app)
    assert a_pid_is_gone_within(worker_pid, 2.0), (
        f"the helper's SIGTERM-deaf worker outlived the third interrupt:\n{app.output}"
    )


@pytest.mark.linux_only_capability(reason="only Linux owns SIGHUP")
@pytest.mark.requires_gpu
def test_sighup_tears_the_graph_down_gracefully(app_under_test):
    """A closed terminal is a graceful shutdown, `teardown()` included."""
    app = app_under_test("a_processor_asleep_in_its_callback_with_hangups_not_ignored")
    app.await_output_containing("MARKER:ASLEEP_IN_PROCESS", "the processor to park")
    app.process.send_signal(signal.SIGHUP)
    app.await_clean_exit()

    assert "RUN_RETURNED" in app.markers(), f"`run()` did not return:\n{app.output}"
    assert "MARKER:ASLEEP_PROBE_TORE_DOWN" in app.output, (
        f"`teardown()` did not run after SIGHUP:\n{app.output}"
    )


#: How long a helper may outlive an app killed outright. Linux's parent-death
#: signal ends it at once; a macOS helper walks the engine's ladder itself — a
#: second for its callback, five for `teardown()`, half a second to leave — and
#: the rest is room for a loaded rig.
HELPER_OUTLIVING_A_KILLED_APP_BUDGET_SECONDS = 10.0


def kill_an_app_holding_two_helpers_asleep_in_their_callbacks(
    app_under_test, teardown_record_directory: Path, monkeypatch
) -> "list[int]":
    """SIGKILL the app, never its helpers, and name the helpers that were alive."""
    monkeypatch.setenv(
        TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE, str(teardown_record_directory)
    )
    app = app_under_test("two_processors_asleep_in_their_callbacks_recording_their_teardown")
    for _ in range(2):
        app.await_output_containing("MARKER:ASLEEP_IN_PROCESS", "each helper to park")
    helper_pids = sorted(
        {int(pid) for pid in ASLEEP_HELPER_PID_MARKER.findall(app.output)}
    )
    assert len(helper_pids) == 2, f"expected two helpers asleep:\n{app.output}"
    app.process.send_signal(signal.SIGKILL)
    app.process.wait()
    return helper_pids


@pytest.mark.requires_gpu
def test_no_helper_outlives_an_app_killed_outright(app_under_test, tmp_path, monkeypatch):
    """A `SIGKILL`ed app runs no teardown, and still leaves no helper behind."""
    helper_pids = kill_an_app_holding_two_helpers_asleep_in_their_callbacks(
        app_under_test, tmp_path, monkeypatch
    )

    survivors = [
        pid
        for pid in helper_pids
        if not a_pid_is_gone_within(pid, HELPER_OUTLIVING_A_KILLED_APP_BUDGET_SECONDS)
    ]
    assert not survivors, (
        f"helper(s) {survivors} outlived the app by "
        f"{HELPER_OUTLIVING_A_KILLED_APP_BUDGET_SECONDS}s"
    )


@pytest.mark.skipif(
    sys.platform != "darwin",
    reason="Linux's parent-death signal is SIGKILL, which runs no teardown()",
)
@pytest.mark.requires_gpu
def test_a_helper_whose_app_was_killed_still_runs_its_teardown_on_macos(
    app_under_test, tmp_path, monkeypatch
):
    """The macOS watch ends the channel, so the helper runs the `teardown()` the
    engine can no longer ask for — its callback interrupted first."""
    helper_pids = kill_an_app_holding_two_helpers_asleep_in_their_callbacks(
        app_under_test, tmp_path, monkeypatch
    )
    for pid in helper_pids:
        a_pid_is_gone_within(pid, HELPER_OUTLIVING_A_KILLED_APP_BUDGET_SECONDS)

    tore_down = sorted(int(record.name) for record in tmp_path.iterdir())
    assert tore_down == helper_pids, (
        f"helpers {helper_pids} were asleep; only {tore_down} ran `teardown()`"
    )


@pytest.mark.requires_gpu
def test_a_process_the_app_started_never_holds_the_apps_output_past_its_exit(
    app_under_test,
):
    """Whatever the app starts inherits no copy of the app's own output.

    The survivor sleeps thirty seconds holding every descriptor it could
    inherit. Reading the app's output to its end must finish within a second of
    the app's own exit.

    Fail-without-fix: the stdio interceptor's copies of the app's stdout were
    inheritable, so the survivor held this pipe for its whole thirty seconds;
    and its hold on the intercept pipe made the interceptor's unbounded join
    hang the app's own teardown for as long.
    """
    app = app_under_test("a_process_the_app_started_outlives_it")
    app.await_output_containing("MARKER:SURVIVOR_PID", "the app to start its survivor")
    survivor_pid = matched_pid(SURVIVOR_PID_MARKER, app)
    exited_at: "list[float]" = []

    def record_when_the_app_exits() -> None:
        app.process.wait()
        exited_at.append(time.monotonic())

    waiter = threading.Thread(target=record_when_the_app_exits, daemon=True)
    try:
        app.await_engine_ready()
        waiter.start()
        app.interrupt()
        output_ended_at = app.await_end_of_output()
        waiter.join(timeout=CLEAN_EXIT_BUDGET_AFTER_OUTPUT_ENDS_SECONDS)

        assert exited_at, f"the app never exited:\n{app.output}"
        assert app.process.returncode == 0, (
            f"the app exited with {app.process.returncode}:\n{app.output}"
        )
        assert output_ended_at - exited_at[0] < 1.0, (
            f"the app's output ended {output_ended_at - exited_at[0]:.1f}s after it exited — "
            f"something it started held it open:\n{app.output}"
        )
    finally:
        with contextlib.suppress(ProcessLookupError):
            os.kill(survivor_pid, signal.SIGKILL)


# How long the app may take to be reaped once its output has ended.
CLEAN_EXIT_BUDGET_AFTER_OUTPUT_ENDS_SECONDS = 10.0


@pytest.mark.requires_gpu
def test_ctrl_c_while_a_helper_is_still_importing_exits_promptly(app_under_test):
    """A helper thirty seconds into importing its processor holds the app for
    one interrupt, not for its import."""
    app = app_under_test("a_helper_still_importing_its_processor")
    app.await_output_containing("helper process started", "the helper to start importing")
    time.sleep(1.0)
    interrupted_at = time.monotonic()
    app.interrupt()
    app.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert ended_in < 5.0, (
        f"the app took {ended_in:.1f}s to end, which is the import rather than the "
        f"interrupt:\n{app.output}"
    )
