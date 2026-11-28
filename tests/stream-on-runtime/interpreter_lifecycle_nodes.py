# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Nodes that make a stream's shutdown slow in the ways the ladder bounds.

Imported by the test process to compile a stream, by the node
interpreter describing each class, and by the helper hosting each node.
"""

from typing import Callable
import json
import os
import signal
import subprocess
import time

from tatolab.stream import OutputPin, RuntimeContextLimitedAccess, Trigger, computed, log, node

# How long a probe's teardown takes when it is meant to be slow but still inside
# the ladder's five-second teardown budget.
SLOW_TEARDOWN_SECONDS = 3.0


def report(marker_name: str, payload: "dict[str, object] | None" = None) -> None:
    log.info(f"MARKER:{marker_name}" + ("" if payload is None else " " + json.dumps(payload)))


class RunsAtTeardown:
    """A value of a node's scope whose close, at the node's teardown, runs `at_teardown`."""

    def __init__(self, at_teardown: "Callable[[], None]") -> None:
        self._at_teardown = at_teardown

    def close(self) -> None:
        self._at_teardown()


@node(execution="continuous", interval_ms=10)
def AsleepInItsCallbackProbe(
    *,
    frames_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Parks in its frame far past the ladder's one-second callback budget."""
    computed(lambda: RunsAtTeardown(lambda: report("ASLEEP_PROBE_TORE_DOWN")))

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        report("ASLEEP_IN_PROCESS", {"pid": os.getpid()})
        time.sleep(30)

    return each_frame


#: Names the directory `AsleepInItsCallbackRecordingItsTeardownProbe` records
#: its teardown in, one file per helper pid — the one witness left once the
#: `tatolabd` it would log to is dead.
TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE = "STREAMLIB_TEST_TEARDOWN_RECORD_DIRECTORY"


def _record_the_teardown_in_a_file() -> None:
    record_directory = os.environ[TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE]
    with open(os.path.join(record_directory, str(os.getpid())), "w"):
        pass


@node(execution="continuous", interval_ms=10)
def AsleepInItsCallbackRecordingItsTeardownProbe(
    *,
    frames_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Parks in its frame, and records its teardown in a file."""
    computed(lambda: RunsAtTeardown(_record_the_teardown_in_a_file))

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        report("ASLEEP_IN_PROCESS", {"pid": os.getpid()})
        time.sleep(30)

    return each_frame


def _tear_down_slowly() -> None:
    time.sleep(SLOW_TEARDOWN_SECONDS)
    report("SLOW_TEARDOWN_FINISHED", {"pid": os.getpid()})


@node(execution="continuous", interval_ms=10)
def AsleepInItsCallbackAndSlowToTearDownProbe(
    *,
    frames_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Parks in its frame, then takes three seconds over its teardown.

    Three of these stopped one after another cost three ladders of about four
    seconds each; stopped at once, about one.
    """
    computed(lambda: RunsAtTeardown(_tear_down_slowly))

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        report("SLOW_TO_TEAR_DOWN_ASLEEP", {"pid": os.getpid()})
        time.sleep(30)

    return each_frame


def _tear_down_for_thirty_seconds_deaf_to_termination() -> None:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    report("LONG_TEARDOWN_BEGAN")
    time.sleep(30)
    report("LONG_TEARDOWN_FINISHED")


@node(execution="continuous", interval_ms=10)
def WorkerKeepingTeardownGoingProbe(
    *,
    frames_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Forks a worker that ignores SIGTERM, then spends thirty seconds in a
    teardown that ignores it too.

    The teardown is what a second interrupt cuts short. With the helper deaf to
    termination, the forced ladder waits out its whole grace before it sends the
    group `SIGKILL`, so a third interrupt landing inside that grace is the only
    thing that can end the worker in time.
    """
    computed(lambda: RunsAtTeardown(_tear_down_for_thirty_seconds_deaf_to_termination))
    worker_pid: "list[int | None]" = [None]

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        if worker_pid[0] is None:
            worker_pid[0] = os.fork()
            if worker_pid[0] == 0:
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                time.sleep(120)
                os._exit(0)
            report("TEARDOWN_WORKER_PID", {"pid": worker_pid[0]})
        time.sleep(30)

    return each_frame


@node(execution="continuous", interval_ms=10)
def StartsAProcessThatOutlivesItProbe(
    *,
    frames_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Starts a process that inherits every descriptor it can and outlives
    `tatolabd` by far.

    It leads a session of its own, so no process-group kill reaches it, and is
    started once the stream runs, so it inherits whatever the helper holds.
    """

    survivor_pid = None

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal survivor_pid
        if survivor_pid is None:
            survivor = subprocess.Popen(
                ["sleep", "30"], close_fds=False, stdin=subprocess.DEVNULL, start_new_session=True
            )
            survivor_pid = survivor.pid
            report("SURVIVOR_PID", {"pid": survivor.pid})

    return each_frame


# Set only in the helper hosting the node: the test process imports this module
# to compile, and the describing interpreter to learn the ports, and neither
# must pay the import's thirty seconds.
if os.environ.get("STREAMLIB_ENTRYPOINT", "").endswith(":ThirtySecondImportProbe"):
    report("HELPER_IMPORT_ASLEEP")
    time.sleep(30)


@node(execution="manual")
def ThirtySecondImportProbe(
    *,
    frames_to_downstream: OutputPin[Trigger],
) -> None:
    """Its module takes thirty seconds to import in the helper that hosts it."""
