# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that prove where a Python processor actually runs.

Run as a real `python app.py` because the claim is about processes: only a
parent can see that the app's interpreter never loaded a second copy of the
processor's module, and that the pid a bag was produced in is not the app's.
"""

import os
import sys

import streamlib
from helper_placement_processors import (
    ReportsItsOwnProcessSource,
    ReportsUpstreamProcessSink,
)

MARKER_PREFIX = "MARKER:"


def marker(name: str) -> None:
    print(f"{MARKER_PREFIX}{name}", flush=True)


def scenario_the_app_never_hosts_the_processor() -> None:
    """`rt.add` loads nothing into the app's own `sys.modules`.

    The app's own `from helper_placement_processors import …` is the
    registration import and the only parent-side load there is. What must not
    happen is the engine loading anything more to host an instance —
    `streamlib._helper` constructs the class, and it lives in another process.
    """
    modules_before_add = set(sys.modules)
    runtime = streamlib.Runtime()
    runtime.add(ReportsItsOwnProcessSource, config={"label": "first"})
    marker(f"MODULES_ADDED_BY_ADD={sorted(set(sys.modules) - modules_before_add)}")
    marker(f"HELPER_MODULE_IN_APP={'streamlib._helper' in sys.modules}")
    marker("CLEAN_EXIT")


def scenario_a_bag_is_produced_in_another_process() -> None:
    """A source and a sink are two children, and the app is neither."""
    runtime = streamlib.Runtime()
    source = runtime.add(ReportsItsOwnProcessSource, config={"label": "only"})
    sink = runtime.add(ReportsUpstreamProcessSink)
    runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    marker(f"APP_PID={os.getpid()}")
    # Runs until the test has seen what it came for and interrupts — the
    # children report in milliseconds, so a timer here would only be a guess
    # about how slow the machine is.
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_two_instances_of_one_class_get_two_processes() -> None:
    """Two `rt.add` calls on one class are two children, not two objects."""
    runtime = streamlib.Runtime()
    for label in ("first", "second"):
        source = runtime.add(ReportsItsOwnProcessSource, config={"label": label})
        sink = runtime.add(ReportsUpstreamProcessSink, display_name=f"{label}Sink")
        runtime.connect(
            source.output("frames_to_downstream"), sink.input("frames_from_upstream")
        )
    marker(f"APP_PID={os.getpid()}")
    # Runs until the test has seen what it came for and interrupts — the
    # children report in milliseconds, so a timer here would only be a guess
    # about how slow the machine is.
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_every_child_is_reaped() -> None:
    """`rt.run()` returning means no helper outlived it.

    The spawn host reports each child's pid as it starts one; the test is what
    checks those pids are gone once the app has exited.
    """
    runtime = streamlib.Runtime()
    source = runtime.add(ReportsItsOwnProcessSource, config={"label": "reaped"})
    sink = runtime.add(ReportsUpstreamProcessSink)
    runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    runtime.run()
    marker("CLEAN_EXIT")


if __name__ == "__main__":
    globals()[f"scenario_{sys.argv[1]}"]()
