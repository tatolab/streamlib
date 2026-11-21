# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@node` classes `test_stream_graph_load_on_tatolabd.py` names in a graph by type alone.

Nothing in the test process imports this module: `tatolabd` describes it in a
node interpreter, so a graph naming `runtime_load_nodes:LoadedFrameRelay`
proves a node type registers from its description alone.
"""

from typing import Annotated

from tatolab.stream import (
    InputPin,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    node,
)


@node
class LoadedFrameRelay:
    """Passes each frame it reads downstream unchanged."""

    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]]

    video_to_downstream: OutputPin[Annotated[Texture, TexturePixelFormats("rgba8_unorm")]]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = self.video_from_upstream.read()
        if frame is not None:
            self.video_to_downstream.write(frame)

