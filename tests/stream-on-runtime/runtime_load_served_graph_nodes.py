# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The sink a running stream in `test_stream_graph_load_on_tatolabd.py` is read back through."""

from tatolab.stream import Image, Input, RuntimeContextLimitedAccess, node


@node
class LoadedFrameSink:
    """Reads each frame it receives and keeps none."""

    video_from_upstream = Input(Image, delivery="newest")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read(self.video_from_upstream)
