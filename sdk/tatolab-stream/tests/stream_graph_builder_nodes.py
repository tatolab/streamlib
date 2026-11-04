# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@node` classes `test_stream_graph_builder.py` adds to a stream.

Their own module rather than the test file because a node's type is the import
path of its class, `stream_graph_builder_nodes:<qualname>` — a name a child
process can import.
"""

import dataclasses

from tatolab.stream import RuntimeContextLimitedAccess, node


@node
class FrameInverter:
    """Inverts each frame it reads."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @node.output()
    def video_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream")
        if frame is not None:
            ctx.outputs.write("video_to_downstream", frame)


@node
class BrightnessReader:
    """Reads each frame it receives and writes nothing."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video_from_upstream")


class FrameFilters:
    """A namespace, so a node's qualname differs from its short name."""

    @node
    class FrameDarkener:
        """Darkens each frame it reads."""

        @node.input(delivery_profile="newest")
        def video_from_upstream(self) -> None: ...

        @node.output()
        def video_to_downstream(self) -> None: ...

        def process(self, ctx: RuntimeContextLimitedAccess) -> None:
            frame = ctx.inputs.read("video_from_upstream")
            if frame is not None:
                ctx.outputs.write("video_to_downstream", frame)


@dataclasses.dataclass
class RigMonitorConfig:
    """What a `RigMonitor` shows its picture under."""

    name: "str | None" = None


@node
class RigMonitor:
    """Shows each frame it reads under the name it is configured with."""

    def __init__(self, config: RigMonitorConfig) -> None:
        self._config = config

    @node.input(delivery_profile="newest")
    def video(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video")
