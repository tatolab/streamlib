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


def marker(text: str) -> None:
    print(f"MARKER:{text}", flush=True)


def raise_sigint_after(delay_seconds: float) -> None:
    """Deliver a real SIGINT to this process from a background thread."""

    def deliver() -> None:
        time.sleep(delay_seconds)
        os.kill(os.getpid(), signal.SIGINT)

    threading.Thread(target=deliver, daemon=True).start()


def scenario_ctrl_c() -> None:
    """The demo: boot, block, and exit cleanly when the driver sends Ctrl-C."""
    runtime = streamlib.Runtime()
    runtime.run()
    marker("RUN_RETURNED")


def scenario_gil_released_while_running() -> None:
    """A Python thread must keep making progress while `run()` blocks.

    If `run()` held the GIL, this counter would be stuck at zero.
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
    raise_sigint_after(2.0)
    runtime.run()

    keep_counting.clear()
    counting_thread.join(timeout=5.0)
    marker(f"PYTHON_THREAD_TICKS={progress['ticks']}")


def scenario_sigint_handed_back_to_cpython() -> None:
    """After `run()` returns, SIGINT must reach CPython again."""
    runtime = streamlib.Runtime()
    raise_sigint_after(2.0)
    runtime.run()
    marker("RUN_RETURNED")

    try:
        os.kill(os.getpid(), signal.SIGINT)
        # CPython's handler sets a flag that the next bytecode check turns into
        # KeyboardInterrupt; sleeping gives it that check.
        time.sleep(2.0)
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
    """A Runtime that is constructed and never run must not hang at exit."""
    streamlib.Runtime()
    marker("CONSTRUCTED_WITHOUT_RUNNING")


def scenario_second_run_is_refused() -> None:
    """`run()` consumes the engine, so a second call must fail loudly."""
    runtime = streamlib.Runtime()
    raise_sigint_after(2.0)
    runtime.run()
    marker("RUN_RETURNED")
    try:
        runtime.run()
    except RuntimeError:
        marker("SECOND_RUN_REFUSED")
    else:
        marker("SECOND_RUN_WAS_ALLOWED")


SCENARIOS = {
    "ctrl_c": scenario_ctrl_c,
    "gil_released": scenario_gil_released_while_running,
    "sigint_handed_back": scenario_sigint_handed_back_to_cpython,
    "exception_in_context_manager": scenario_exception_inside_context_manager,
    "never_run": scenario_never_run,
    "second_run_refused": scenario_second_run_is_refused,
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()
    marker("CLEAN_EXIT")
