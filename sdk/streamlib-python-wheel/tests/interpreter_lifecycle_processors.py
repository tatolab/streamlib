# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that make an app's shutdown slow in the ways the ladder bounds.

Imported by `interpreter_lifecycle_app.py` to register them and by each helper
process that hosts one — never to host an instance in the app.
"""

import os
import signal
import time

from streamlib import log, output, processor

# How long a probe's teardown takes when it is meant to be slow but still inside
# the ladder's five-second teardown budget.
SLOW_TEARDOWN_SECONDS = 3.0


@processor(execution="continuous", interval_ms=10)
class AsleepInItsCallbackProbe:
    """Parks in `process()` far past the ladder's one-second callback budget."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        log.info(f"MARKER:ASLEEP_IN_PROCESS {os.getpid()}")
        time.sleep(30)

    def teardown(self, ctx) -> None:
        log.info("MARKER:ASLEEP_PROBE_TORE_DOWN")


#: Names the directory `AsleepInItsCallbackRecordingItsTeardownProbe` records
#: its `teardown()` in, one file per helper pid — the one witness left once the
#: app it would log to is dead.
TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE = "STREAMLIB_TEST_TEARDOWN_RECORD_DIRECTORY"


@processor(execution="continuous", interval_ms=10)
class AsleepInItsCallbackRecordingItsTeardownProbe:
    """Parks in `process()`, and records its `teardown()` in a file."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        log.info(f"MARKER:ASLEEP_IN_PROCESS {os.getpid()}")
        time.sleep(30)

    def teardown(self, ctx) -> None:
        record_directory = os.environ[TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE]
        with open(os.path.join(record_directory, str(os.getpid())), "w"):
            pass


@processor(execution="continuous", interval_ms=10)
class AsleepInItsCallbackAndSlowToTearDownProbe:
    """Parks in `process()`, then takes three seconds over `teardown()`.

    Three of these stopped one after another cost three ladders of about four
    seconds each; stopped at once, about one.
    """

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        log.info(f"MARKER:SLOW_TO_TEAR_DOWN_ASLEEP {os.getpid()}")
        time.sleep(30)

    def teardown(self, ctx) -> None:
        time.sleep(SLOW_TEARDOWN_SECONDS)
        log.info(f"MARKER:SLOW_TEARDOWN_FINISHED {os.getpid()}")


@processor(execution="continuous", interval_ms=10)
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

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.worker_pid is None:
            self.worker_pid = os.fork()
            if self.worker_pid == 0:
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                time.sleep(120)
                os._exit(0)
            log.info(f"MARKER:TEARDOWN_WORKER_PID {self.worker_pid}")
        time.sleep(30)

    def teardown(self, ctx) -> None:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        log.info("MARKER:LONG_TEARDOWN_BEGAN")
        time.sleep(30)
        log.info("MARKER:LONG_TEARDOWN_FINISHED")


# Set only in a helper process: the app imports this module too, to register
# the class, and must not pay the import's thirty seconds itself.
if os.environ.get("STREAMLIB_ENTRYPOINT", "").endswith(":ThirtySecondImportProbe"):
    log.info("MARKER:HELPER_IMPORT_ASLEEP")
    time.sleep(30)


@processor(execution="manual")
class ThirtySecondImportProbe:
    """Its module takes thirty seconds to import in the helper that hosts it."""

    @output()
    def frames_to_downstream(self) -> None: ...
