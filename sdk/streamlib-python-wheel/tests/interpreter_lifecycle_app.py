# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The app under test — a real `python app.py`, driven by
`test_interpreter_lifecycle.py`.

Each scenario prints markers the driver asserts on. Markers are flushed as they
are printed because the engine writes its own logs to this same stdout from
Rust, and an unflushed Python buffer would reorder them.
"""

import os
import signal
import sys
import threading
import time

import streamlib


KEYBOARD_INTERRUPT_UPPER_BOUND_SECONDS = 5.0


def marker(text: str) -> None:
    print(f"MARKER:{text}", flush=True)


def scenario_ctrl_c() -> None:
    """The demo: boot, block, and exit cleanly when the driver sends Ctrl-C."""
    runtime = streamlib.Runtime()
    runtime.run()
    marker("RUN_RETURNED")


def scenario_gil_released_while_running() -> None:
    """A Python thread must keep making progress while `run()` blocks.

    The counter is sampled either side of `run()` rather than over the whole
    scenario, so the reported delta is progress made *during* the blocking call
    and nowhere else. The driver sends the SIGINT that ends it.
    """
    progress = {"ticks": 0}
    keep_counting = threading.Event()
    keep_counting.set()

    def count() -> None:
        while keep_counting.is_set():
            progress["ticks"] += 1
            time.sleep(0.001)

    counting_thread = threading.Thread(target=count, daemon=True)
    counting_thread.start()

    runtime = streamlib.Runtime()
    ticks_before_run = progress["ticks"]
    runtime.run()
    ticks_during_run = progress["ticks"] - ticks_before_run

    keep_counting.clear()
    counting_thread.join(timeout=5.0)
    marker(f"PYTHON_THREAD_TICKS={ticks_during_run}")


def scenario_sigint_handed_back_to_cpython() -> None:
    """After `run()` returns, SIGINT must reach CPython again.

    The driver sends the first SIGINT; this sends the second one itself,
    because the point is what happens to a signal raised *after* `run()` has
    handed the disposition back.
    """
    runtime = streamlib.Runtime()
    runtime.run()
    marker("RUN_RETURNED")

    try:
        os.kill(os.getpid(), signal.SIGINT)
        # CPython's handler sets a flag that the next bytecode check turns into
        # KeyboardInterrupt, which interrupts this sleep immediately. The
        # duration is an upper bound on that check, not a sequencing delay — a
        # swallowed signal is what makes it elapse in full.
        time.sleep(KEYBOARD_INTERRUPT_UPPER_BOUND_SECONDS)
    except KeyboardInterrupt:
        marker("KEYBOARD_INTERRUPT_RAISED")
    else:
        marker("SIGINT_WAS_SWALLOWED")


def scenario_exception_inside_context_manager() -> None:
    """An exception must not strand the engine, and must still propagate."""
    try:
        with streamlib.Runtime():
            raise ValueError("failure inside the with-block")
    except ValueError:
        marker("EXCEPTION_PROPAGATED")


def scenario_never_run() -> None:
    """A Runtime dropped without running must not hang at exit.

    The result is unbound, so CPython refcounts it to zero immediately and this
    exercises the pyclass `Drop` — not the `atexit` hook.
    """
    streamlib.Runtime()
    marker("CONSTRUCTED_WITHOUT_RUNNING")


def scenario_held_by_a_live_thread_at_exit() -> None:
    """A Runtime reachable only from a live daemon thread's frame at exit.

    This is the case the `atexit` hook is actually load-bearing for. A
    module-global reference is not: interpreter shutdown clears module globals
    and that reaches `Drop` on its own. Here the only reference lives in a
    parked daemon thread's frame, which CPython never unwinds — without the
    hook the engine's threads are simply alive when the interpreter finalizes,
    and no teardown runs at all.
    """
    holding_thread_is_parked = threading.Event()

    def hold_runtime_forever() -> None:
        _runtime_held_in_this_frame = streamlib.Runtime()  # noqa: F841
        holding_thread_is_parked.set()
        # Never returns, so the frame — and the only reference — stays alive.
        threading.Event().wait()

    threading.Thread(target=hold_runtime_forever, daemon=True).start()
    holding_thread_is_parked.wait(timeout=60.0)
    marker("HELD_BY_LIVE_THREAD_AT_EXIT")


def scenario_second_run_is_refused() -> None:
    """`run()` consumes the engine, so a second call must fail loudly."""
    runtime = streamlib.Runtime()
    runtime.run()
    marker("RUN_RETURNED")
    try:
        runtime.run()
    except RuntimeError:
        marker("SECOND_RUN_REFUSED")
    else:
        marker("SECOND_RUN_WAS_ALLOWED")


def scenario_shutdown_from_another_thread() -> None:
    """`shutdown()` from a worker thread must end a blocking `run()`.

    This is the programmatic stop — no signal involved. The worker waits for the
    engine's own ready line on the main thread's behalf via a barrier the
    driver cannot see, so it uses the same readiness the driver does: it is
    started only once `run()` is about to be entered.
    """
    runtime = streamlib.Runtime()

    stop_requested = threading.Event()

    def request_stop() -> None:
        # The driver signals readiness by closing stdin; until then the engine
        # may still be coming up.
        sys.stdin.read()
        stop_requested.set()
        runtime.shutdown()

    threading.Thread(target=request_stop, daemon=True).start()
    runtime.run()

    marker("RUN_RETURNED")
    if stop_requested.is_set():
        marker("STOPPED_FROM_ANOTHER_THREAD")


def scenario_readiness_wait_across_teardown() -> None:
    """A readiness wait taken while `run()` blocks must not break teardown.

    The wait reaches the engine the run loop owns, and the contract says every
    engine thread is joined before `run()` returns. A reference left behind by
    the wait makes that join find the engine still borrowed, and `run()` raises
    instead of returning — a non-zero exit the driver sees.
    """
    runtime = streamlib.Runtime()

    def wait_then_stop() -> None:
        # The same stdin handshake the cross-thread shutdown scenario uses:
        # until the driver closes it, `run()` may not yet hold the engine.
        sys.stdin.read()
        runtime.wait_until_every_processor_is_running(timeout=30.0)
        marker("GRAPH_READY")
        runtime.shutdown()

    threading.Thread(target=wait_then_stop, daemon=True).start()
    runtime.run()

    marker("RUN_RETURNED")


def scenario_shutdown_spun_across_the_run_loop_exit() -> None:
    """Hammer `shutdown()` from a worker across the first run loop's exit.

    The sequential `with`-block case only ever calls `shutdown()` once `run()`
    has fully returned. This one deliberately lands calls *during* the exit —
    the window where a request can be issued after the run loop stopped
    observing but before the handle is marked torn down. Any request that
    escapes is inherited by the second pipeline, which then returns instantly.
    """
    first_runtime = streamlib.Runtime()
    keep_requesting = threading.Event()
    keep_requesting.set()

    def spin_shutdown() -> None:
        while keep_requesting.is_set():
            first_runtime.shutdown()

    spinner = threading.Thread(target=spin_shutdown, daemon=True)

    def start_spinning_once_running() -> None:
        sys.stdin.read()
        spinner.start()

    threading.Thread(target=start_spinning_once_running, daemon=True).start()
    first_runtime.run()
    # Keeps spinning across the exit on purpose — stopped only once the second
    # pipeline is about to start.
    keep_requesting.clear()
    spinner.join(timeout=10.0)
    marker("PIPELINE_1_RETURNED")

    with streamlib.Runtime() as second_runtime:
        marker("PIPELINE_2_RUNNING")
        started = time.monotonic()
        second_runtime.run()
        blocked_milliseconds = round((time.monotonic() - started) * 1000)
    marker(f"PIPELINE_2_BLOCKED_MS={blocked_milliseconds}")


def scenario_two_pipelines_in_one_process() -> None:
    """Two run loops in turn, each through the context manager.

    The driver Ctrl-Cs each one. The second must block on its own account — if
    it inherits the first pipeline's shutdown request it returns immediately,
    which the reported monotonic span makes visible. The engine initializes its
    logging pathway once per process, so the second runtime emits no log lines
    of its own and this scenario announces its own readiness instead.
    """
    with streamlib.Runtime() as first_runtime:
        first_runtime.run()
    marker("PIPELINE_1_RETURNED")

    with streamlib.Runtime() as second_runtime:
        marker("PIPELINE_2_RUNNING")
        started = time.monotonic()
        second_runtime.run()
        blocked_milliseconds = round((time.monotonic() - started) * 1000)
    marker(f"PIPELINE_2_BLOCKED_MS={blocked_milliseconds}")


SCENARIOS = {
    "ctrl_c": scenario_ctrl_c,
    "gil_released": scenario_gil_released_while_running,
    "sigint_handed_back": scenario_sigint_handed_back_to_cpython,
    "exception_in_context_manager": scenario_exception_inside_context_manager,
    "never_run": scenario_never_run,
    "held_by_a_live_thread_at_exit": scenario_held_by_a_live_thread_at_exit,
    "second_run_refused": scenario_second_run_is_refused,
    "shutdown_from_another_thread": scenario_shutdown_from_another_thread,
    "readiness_wait_across_teardown": scenario_readiness_wait_across_teardown,
    "two_pipelines_in_one_process": scenario_two_pipelines_in_one_process,
    "shutdown_spun_across_the_run_loop_exit": scenario_shutdown_spun_across_the_run_loop_exit,
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()
    marker("CLEAN_EXIT")
