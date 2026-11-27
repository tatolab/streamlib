# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The sink a running stream in `test_stream_graph_load_on_tatolabd.py` is read back through."""

from typing import Annotated, Callable

from tatolab.stream import InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, node


@node(execution="reactive")
def LoadedFrameSink(
    *,
    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads each frame it receives and keeps none."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        video_from_upstream.read()

    return each_frame
