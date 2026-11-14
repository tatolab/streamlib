# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The `@node.process` classes `test_stream_graph_load_on_tatolabd.py` names in a graph by type alone.

Nothing in the test process imports this module: `tatolabd` describes it in a
processor interpreter, so a graph naming `runtime_load_nodes:LoadedFrameRelay`
proves a node type registers from its description alone.
"""

from tatolab.stream import Image, Input, Output, RuntimeContextLimitedAccess, node


@node.process
class LoadedFrameRelay:
    """Passes each frame it reads downstream unchanged."""

    video_from_upstream = Input(Image, delivery="newest")

    video_to_downstream = Output(Image)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.video_from_upstream)
        if frame is not None:
            ctx.outputs.write(self.video_to_downstream, frame)

