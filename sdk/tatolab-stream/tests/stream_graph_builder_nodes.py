# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@node.process` classes `test_stream_graph_builder.py` adds to a stream.

Their own module rather than the test file because a node's type is the import
path of its class, `stream_graph_builder_nodes:<qualname>` — a name a child
process can import.
"""

import dataclasses

from tatolab.stream import Input, Output, Packets, RuntimeContextLimitedAccess, node


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
