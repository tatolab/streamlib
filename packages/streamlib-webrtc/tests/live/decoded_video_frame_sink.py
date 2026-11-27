# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Reads every decoded frame and keeps none, so `graph` counts what the decoder writes."""

from typing import Annotated, Callable

from tatolab.stream import InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, node


@node(execution="reactive")
def DecodedVideoFrameSink(
    *,
    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """The reader a decoder's `video` port needs for `graph` to count its writes."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        video_from_upstream.read()

    return each_frame
