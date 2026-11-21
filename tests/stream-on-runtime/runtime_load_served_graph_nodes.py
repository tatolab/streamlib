# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The sink a running stream in `test_stream_graph_load_on_tatolabd.py` is read back through."""

from typing import Annotated

from tatolab.stream import InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, node


@node
class LoadedFrameSink:
    """Reads each frame it receives and keeps none."""

    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.video_from_upstream.read()
