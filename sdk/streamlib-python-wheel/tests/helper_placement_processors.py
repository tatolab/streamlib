# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that report which process they are running in.

Their module is imported by the child that hosts them and by the app that
registers them — and by nothing else. What the placement gate watches for is a
*second* parent-side load: the engine importing this module to host an
instance, which is the shape the ban forbids.
"""

import dataclasses
import os
import time

from streamlib import input, log, output, processor
from streamlib._engine import (
    processor_class_import_paths_in_this_processes_catalog,
)


@dataclasses.dataclass
class ReportsItsOwnProcessSourceConfig:
    label: str = "unlabelled"


@processor(execution="continuous", interval_ms=10)
class ReportsItsOwnProcessSource:
    """Stamps every bag with the pid it was produced in."""

    def __init__(self, config: ReportsItsOwnProcessSourceConfig) -> None:
        self.label = config.label
        self.announced = False

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if not self.announced:
            log.info(f"MARKER:SOURCE_PID {self.label} {os.getpid()}")
            self.announced = True
        ctx.outputs.write("frames_to_downstream", {"produced_in_pid": os.getpid()})


@processor
class ReportsUpstreamProcessSink:
    """Announces the pid a bag was produced in, alongside its own."""

    def __init__(self) -> None:
        self.bags_seen = 0

    @input(delivery_profile="newest")
    def frames_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("frames_from_upstream")
        if bag is None:
            return
        # Announced on every tenth bag rather than once: a test that waits for
        # this *after* another processor crashed needs evidence of live
        # traffic, not a marker emitted before the crash.
        self.bags_seen += 1
        if self.bags_seen % 10 == 1:
            log.info(
                f"MARKER:SINK_PID {os.getpid()} UPSTREAM_PID {bag['produced_in_pid']}"
            )


@processor
class ReportsItsOwnProcessVideoSink:
    """Announces its own process, reading frames a native built-in produced.

    The frames come from `TestPatternSource`, which is native and therefore
    has no process of its own — so this sink's pid, set against the app's,
    is what discriminates the two sides of the boundary.
    """

    def __init__(self) -> None:
        self.announced = False

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if ctx.inputs.read("video_from_upstream") is None or self.announced:
            return
        self.announced = True
        log.info(f"MARKER:VIDEO_SINK_PID {os.getpid()}")


@processor(execution="continuous", interval_ms=10)
class DiesAbruptlyProbe:
    """Takes its own process down mid-run, the way a segfaulting native call
    inside a user callback would."""

    def __init__(self) -> None:
        self.frames_before_dying = 3

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        self.frames_before_dying -= 1
        if self.frames_before_dying <= 0:
            log.info(f"MARKER:ABOUT_TO_DIE {os.getpid()}")
            # Not an exception: the point is a process that stops existing
            # without unwinding, which is what a segfaulting native call does.
            os._exit(1)


@processor(execution="manual")
class ReportsItsOwnProcessesProcessorCatalog:
    """Announces the processor catalog of the process it was constructed in.

    A helper hosts no graph, so importing this module inside one must leave its
    registry empty of every class the module declares — the one thing the app
    process cannot see for itself, because the registry is per process. The
    count is of this module's classes rather than of the whole catalog: the
    native built-ins register when the wheel's extension module initialises,
    wherever that happens, and none of them came from a decorator.
    """

    def setup(self, ctx) -> None:
        declared_by_this_module = [
            path
            for path in processor_class_import_paths_in_this_processes_catalog()
            if path.startswith("helper_placement_processors:")
        ]
        log.info(
            f"MARKER:CHILD_CATALOG {os.getpid()} "
            f"{'helper_placement_processors:ReportsItsOwnProcessesProcessorCatalog' in declared_by_this_module} "
            f"{len(declared_by_this_module)}"
        )


@processor(execution="continuous", interval_ms=10)
class SleepsThroughItsOwnShutdownProbe:
    """Parks in `process()` far past the ladder's one-second budget.

    The bag it had in flight is lost to the interrupt, and `stop()` and
    `teardown()` still run — which is what the markers below are for.
    """

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        log.info(f"MARKER:ASLEEP_IN_PROCESS {os.getpid()}")
        time.sleep(30)
        log.info("MARKER:SLEPT_THE_WHOLE_WAY")

    def stop(self, ctx) -> None:
        log.info("MARKER:SLEEPER_STOPPED")

    def teardown(self, ctx) -> None:
        log.info("MARKER:SLEEPER_TORE_DOWN")


@processor(execution="continuous", interval_ms=50)
class ForksAWorkerThatOutlivesItProbe:
    """Starts a worker of its own the way `os.system("sleep 60 &")` does.

    The worker is the descendant the process-group rung exists to reach. Its
    pid is announced so a test can go looking for it after the app is gone.
    """

    def __init__(self) -> None:
        self.worker_pid = None

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.worker_pid is None:
            self.worker_pid = os.fork()
            if self.worker_pid == 0:
                time.sleep(120)
                os._exit(0)
            log.info(f"MARKER:WORKER_PID {self.worker_pid} HELPER_PID {os.getpid()}")
