# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that report which process they are running in.

Their module is imported by the child that hosts them and by the app that
registers them — and by nothing else. What the placement gate watches for is a
*second* parent-side load: the engine importing this module to host an
instance, which is the shape the ban forbids.
"""

import os

from streamlib import input, log, output, processor


@processor(execution="continuous", interval_ms=10)
class ReportsItsOwnProcessSource:
    """Stamps every bag with the pid it was produced in."""

    def __init__(self, label: str = "unlabelled") -> None:
        self.label = label
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

    @input()
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

    @input(delivery_profile="every_sample")
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
