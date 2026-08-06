# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The behavioural gate on where a Python processor runs.

Every `@processor` class runs in its own child process — own interpreter, own
GIL. That is the library's reason to exist, so it is asserted behaviourally
rather than trusted: the app's own interpreter never loads a second copy of the
processor's module, and the pid a bag was produced in is not the app's.

`cargo xtask check-no-in-process-placement` gates the vocabulary; this gates
the behaviour. A change that reintroduces in-process hosting without using any
of the banned words still turns these red.
"""

import os
import re
from pathlib import Path

import pytest

APP = Path(__file__).parent / "helper_placement_app.py"

SOURCE_PID_MARKER = re.compile(r"MARKER:SOURCE_PID (\S+) (\d+)")
SINK_PID_MARKER = re.compile(r"MARKER:SINK_PID (\d+) UPSTREAM_PID (\d+)")
APP_PID_MARKER = re.compile(r"MARKER:APP_PID=(\d+)")
HELPER_STARTED_MARKER = re.compile(r"helper process started: pid=(\d+)")


def run_scenario(start_app_under_test, scenario: str):
    """A scenario that never runs a graph — it reports and returns."""
    app = start_app_under_test(APP, scenario)
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    return app


def run_scenario_until(start_app_under_test, scenario: str, awaited: str, what: str):
    """Run a graph until `awaited` shows up, then Ctrl-C and require a clean exit.

    Sequencing on the marker rather than a timer is what keeps the wait as
    short as the event and as long as the machine needs — and the interrupt
    exercises the same teardown a terminal Ctrl-C does.
    """
    app = start_app_under_test(APP, scenario)
    app.await_output_containing(awaited, what)
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    return app


def matched_marker(pattern: "re.Pattern[str]", output: str) -> "re.Match[str]":
    """The marker `pattern` names, or a failure naming what was missing.

    A marker that never arrived means the scenario did not get where it was
    going, which is a more useful failure than an attribute error on `None`.
    """
    match = pattern.search(output)
    assert match is not None, (
        f"the app never reported {pattern.pattern!r}:\n{output}"
    )
    return match


def test_adding_a_processor_loads_nothing_into_the_app(start_app_under_test):
    """The registration import is the only parent-side load.

    Mentally restore an in-process host and this fails on the first line: the
    engine would import `streamlib._processor_hosting` to construct the class
    in this interpreter.
    """
    app = run_scenario(start_app_under_test, "the_app_never_hosts_the_processor")
    assert "MODULES_ADDED_BY_ADD=[]" in app.output, (
        f"`rt.add` loaded modules into the app's own interpreter:\n{app.output}"
    )
    assert "HELPER_MODULE_IN_APP=False" in app.output, (
        f"the app imported the helper runtime, which belongs in the child:\n{app.output}"
    )


def test_a_bag_is_produced_in_a_process_that_is_not_the_apps(start_app_under_test):
    """The pid rides in the bag, so the claim is about where `process` ran —
    not about what the engine logged it was going to do."""
    app = run_scenario_until(
        start_app_under_test,
        "a_bag_is_produced_in_another_process",
        "MARKER:SINK_PID",
        "the sink to report the process its bag was produced in",
    )

    app_pid = int(matched_marker(APP_PID_MARKER, app.output).group(1))
    sink_pid, upstream_pid = (
        int(group) for group in matched_marker(SINK_PID_MARKER, app.output).groups()
    )

    assert upstream_pid != app_pid, (
        f"the source produced its bag in the app's own process ({app_pid})"
    )
    assert sink_pid != app_pid, (
        f"the sink consumed the bag in the app's own process ({app_pid})"
    )
    assert sink_pid != upstream_pid, (
        f"both processors shared one process ({sink_pid}) — one processor, one helper"
    )


def test_two_instances_of_one_class_get_two_processes(start_app_under_test):
    """Registration is per class; placement is per instance."""
    # Awaited twice without naming a label: the two instances report in
    # whichever order they finish booting, and the forward scan finds the
    # second occurrence wherever it lands.
    app = start_app_under_test(APP, "two_instances_of_one_class_get_two_processes")
    for ordinal in ("first", "second"):
        app.await_output_containing(
            "MARKER:SOURCE_PID", f"the {ordinal} source instance to report its process"
        )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    reported_pids = {
        int(pid) for _, pid in SOURCE_PID_MARKER.findall(app.output)
    }
    assert len(reported_pids) == 2, (
        f"two instances of one class reported {reported_pids} — expected two "
        f"distinct pids:\n{app.output}"
    )
    app_pid = int(matched_marker(APP_PID_MARKER, app.output).group(1))
    assert app_pid not in reported_pids


def helper_process_is_still_alive(pid: int) -> bool:
    """Whether `pid` is still a live streamlib helper.

    The command line is checked, not just the pid: pids are reused, and a
    recycled one would otherwise read as a leaked child.
    """
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as command_line:
            return b"streamlib._helper" in command_line.read()
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return False


def test_no_helper_survives_the_app(start_app_under_test):
    """`rt.run()` returning means every child was reaped.

    Asserted against the children's own pids, which the spawn host reports as
    it starts each one. The app's process group cannot answer this: the spawn
    host puts every child in a group of its own — that is what keeps a terminal
    Ctrl-C from reaching them directly — so the app's group is empty of helpers
    whether or not they were reaped, and an assertion on it passes over a leak.

    A survivor holds this processor's iceoryx2 ports open, and the next run
    fails to open them — which reads as a transport bug rather than a leak.
    """
    app = run_scenario_until(
        start_app_under_test,
        "every_child_is_reaped",
        "MARKER:SINK_PID",
        "the graph to reach a bag crossing between two children",
    )
    helper_pids = [int(pid) for pid in HELPER_STARTED_MARKER.findall(app.output)]
    assert len(helper_pids) == 2, (
        f"expected the source and the sink to each report a helper pid, got "
        f"{helper_pids}:\n{app.output}"
    )

    survivors = [pid for pid in helper_pids if helper_process_is_still_alive(pid)]
    assert not survivors, (
        f"helper processes {survivors} outlived the app that spawned them"
    )
