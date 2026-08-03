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

# Held at module scope on purpose: a Runtime still reachable from a module
# global when the interpreter starts finalizing is the case `Drop` alone does
# not obviously cover, and the one the `atexit` hook exists for.
RUNTIME_HELD_AT_MODULE_SCOPE: "streamlib.Runtime | None" = None


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


def scenario_still_referenced_at_exit() -> None:
    """A Runtime alive at interpreter exit must still tear the engine down.

    Nothing here drops the reference, so teardown has to come from interpreter
    shutdown — the `atexit` hook, or module-global clearing reaching `Drop`.
    """
    global RUNTIME_HELD_AT_MODULE_SCOPE
    RUNTIME_HELD_AT_MODULE_SCOPE = streamlib.Runtime()
    marker("STILL_REFERENCED_AT_EXIT")


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


SCENARIOS = {
    "ctrl_c": scenario_ctrl_c,
    "gil_released": scenario_gil_released_while_running,
    "sigint_handed_back": scenario_sigint_handed_back_to_cpython,
    "exception_in_context_manager": scenario_exception_inside_context_manager,
    "never_run": scenario_never_run,
    "still_referenced_at_exit": scenario_still_referenced_at_exit,
    "second_run_refused": scenario_second_run_is_refused,
    "shutdown_from_another_thread": scenario_shutdown_from_another_thread,
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()
    marker("CLEAN_EXIT")
