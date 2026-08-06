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
        self.announced = False

    @input()
    def frames_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("frames_from_upstream")
        if bag is None or self.announced:
            return
        log.info(f"MARKER:SINK_PID {os.getpid()} UPSTREAM_PID {bag['produced_in_pid']}")
        self.announced = True
