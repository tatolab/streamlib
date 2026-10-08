# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that make a stream's shutdown slow in the ways the ladder bounds.

Imported by the test process to compile a stream, by the processor
interpreter describing each class, and by the helper hosting each node.
"""

import json
import os
import signal
import subprocess
import time

from tatolab.stream import log, node

# How long a probe's teardown takes when it is meant to be slow but still inside
# the ladder's five-second teardown budget.
SLOW_TEARDOWN_SECONDS = 3.0


def report(marker_name: str, payload: "dict[str, object] | None" = None) -> None:
    log.info(f"MARKER:{marker_name}" + ("" if payload is None else " " + json.dumps(payload)))


@node(execution="continuous", interval_ms=10)
class AsleepInItsCallbackProbe:
    """Parks in `process()` far past the ladder's one-second callback budget."""

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        report("ASLEEP_IN_PROCESS", {"pid": os.getpid()})
        time.sleep(30)

    def teardown(self, ctx) -> None:
        report("ASLEEP_PROBE_TORE_DOWN")


#: Names the directory `AsleepInItsCallbackRecordingItsTeardownProbe` records
#: its `teardown()` in, one file per helper pid — the one witness left once the
#: `tatolabd` it would log to is dead.
TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE = "STREAMLIB_TEST_TEARDOWN_RECORD_DIRECTORY"


@node(execution="continuous", interval_ms=10)
class AsleepInItsCallbackRecordingItsTeardownProbe:
    """Parks in `process()`, and records its `teardown()` in a file."""

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        report("ASLEEP_IN_PROCESS", {"pid": os.getpid()})
        time.sleep(30)

    def teardown(self, ctx) -> None:
        record_directory = os.environ[TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE]
        with open(os.path.join(record_directory, str(os.getpid())), "w"):
            pass


@node(execution="continuous", interval_ms=10)
class AsleepInItsCallbackAndSlowToTearDownProbe:
    """Parks in `process()`, then takes three seconds over `teardown()`.

    Three of these stopped one after another cost three ladders of about four
    seconds each; stopped at once, about one.
    """

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        report("SLOW_TO_TEAR_DOWN_ASLEEP", {"pid": os.getpid()})
        time.sleep(30)

    def teardown(self, ctx) -> None:
        time.sleep(SLOW_TEARDOWN_SECONDS)
        report("SLOW_TEARDOWN_FINISHED", {"pid": os.getpid()})


@node(execution="continuous", interval_ms=10)
class WorkerKeepingTeardownGoingProbe:
    """Forks a worker that ignores SIGTERM, then spends thirty seconds in a
    `teardown()` that ignores it too.

    The teardown is what a second interrupt cuts short. With the helper deaf to
    termination, the forced ladder waits out its whole grace before it sends the
    group `SIGKILL`, so a third interrupt landing inside that grace is the only
    thing that can end the worker in time.
    """

    def __init__(self) -> None:
        self.worker_pid = None

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.worker_pid is None:
            self.worker_pid = os.fork()
            if self.worker_pid == 0:
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                time.sleep(120)
                os._exit(0)
            report("TEARDOWN_WORKER_PID", {"pid": self.worker_pid})
        time.sleep(30)

    def teardown(self, ctx) -> None:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        report("LONG_TEARDOWN_BEGAN")
        time.sleep(30)
        report("LONG_TEARDOWN_FINISHED")


@node(execution="continuous", interval_ms=10)
class StartsAProcessThatOutlivesItProbe:
    """Starts a process that inherits every descriptor it can and outlives
    `tatolabd` by far.

    It leads a session of its own, so no process-group kill reaches it, and is
    started once the stream runs, so it inherits whatever the helper holds.
    """

    def __init__(self) -> None:
        self.survivor_pid = None

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.survivor_pid is None:
            survivor = subprocess.Popen(
                ["sleep", "30"], close_fds=False, stdin=subprocess.DEVNULL, start_new_session=True
            )
            self.survivor_pid = survivor.pid
            report("SURVIVOR_PID", {"pid": survivor.pid})


# Set only in the helper hosting the node: the test process imports this module
# to compile, and the describing interpreter to learn the ports, and neither
# must pay the import's thirty seconds.
if os.environ.get("STREAMLIB_ENTRYPOINT", "").endswith(":ThirtySecondImportProbe"):
    report("HELPER_IMPORT_ASLEEP")
    time.sleep(30)


@node(execution="manual")
class ThirtySecondImportProbe:
    """Its module takes thirty seconds to import in the helper that hosts it."""

    @node.output()
    def frames_to_downstream(self) -> None: ...
