# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The Python nodes `test_links_between_streams_on_one_machine.py` runs at either end of a
link between two streams.

In a module of their own because a processor interpreter imports each class
by its import path, and a class declared inside a pytest module would have the
child import the test suite.
"""

import dataclasses
import json
import os

from tatolab.stream import RuntimeContextLimitedAccess, log, node


def bags_counted_marker_of(counter: str) -> str:
    """The marker the counting sink named `counter` reports on, followed by
    `{"bags_counted": <n>, "pid": <its process>}`."""
    return f"BAGS_COUNTED_BY_{counter.upper()}"


#: A counting sink reports its first bag, then every this many.
BAGS_BETWEEN_COUNT_REPORTS = 10


@dataclasses.dataclass
class CountsEachBagItReadsConfig:
    counter: str = "unnamed"


@node
class CountsEachBagItReads:
    """Counts each bag it reads, reporting the count and its own process id
    on its first bag and every `BAGS_BETWEEN_COUNT_REPORTS` after."""

    def __init__(self, config: CountsEachBagItReadsConfig) -> None:
        self.counter = config.counter
        self.bags_counted = 0

    @node.input(delivery_profile="newest")
    def bags_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if ctx.inputs.read("bags_from_upstream") is None:
            return
        self.bags_counted += 1
        if self.bags_counted == 1 or self.bags_counted % BAGS_BETWEEN_COUNT_REPORTS == 0:
            log.info(
                f"MARKER:{bags_counted_marker_of(self.counter)} "
                + json.dumps({"bags_counted": self.bags_counted, "pid": os.getpid()})
            )


@node
class RelaysEachFrameDownstream:
    """Writes each frame it reads to its own output unchanged, so the frame
    leaves its stream from a Python node."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @node.output()
    def video_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame_payload = ctx.inputs.read("video_from_upstream")
        if frame_payload is not None:
            ctx.outputs.write("video_to_downstream", frame_payload)
