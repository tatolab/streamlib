# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The nodes `test_stream_graph_builder.py` adds to a stream.

Their own module rather than the test file because a node's type is the import
path of its class or function, `stream_graph_builder_nodes:<qualname>` — a name a child
process can import.
"""

import dataclasses

from tatolab.stream import Input, Output, Packets, RuntimeContextLimitedAccess, node


@node.function
def remap_onto_a_range(x: float, low: float = 0.0, high: float = 1.0) -> float:
    """Maps 0..1 onto low..high."""
    return low + x * (high - low)


@node.process
class FrameInverter:
    """Inverts each frame it reads."""

    video_from_upstream = Input(Packets, delivery="newest")

    video_to_downstream = Output(Packets)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream")
        if frame is not None:
            ctx.outputs.write("video_to_downstream", frame)


@node.process
class BrightnessReader:
    """Reads each frame it receives and writes nothing."""

    video_from_upstream = Input(Packets, delivery="newest")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video_from_upstream")


class FrameFilters:
    """A namespace, so a node's qualname differs from its short name."""

    @node.process
    class FrameDarkener:
        """Darkens each frame it reads."""

        video_from_upstream = Input(Packets, delivery="newest")

        video_to_downstream = Output(Packets)

        def process(self, ctx: RuntimeContextLimitedAccess) -> None:
            frame = ctx.inputs.read("video_from_upstream")
            if frame is not None:
                ctx.outputs.write("video_to_downstream", frame)


@dataclasses.dataclass
class RigMonitorConfig:
    """What a `RigMonitor` shows its picture under."""

    name: "str | None" = None


@node.process
class RigMonitor:
    """Shows each frame it reads under the name it is configured with."""

    def __init__(self, config: RigMonitorConfig) -> None:
        self._config = config

    video = Input(Packets, delivery="newest")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video")
