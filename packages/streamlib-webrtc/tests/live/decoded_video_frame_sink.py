# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Reads every decoded frame and keeps none, so `graph` counts what the decoder writes."""

from typing import Annotated

from tatolab.stream import InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, node


@node
class DecodedVideoFrameSink:
    """The reader a decoder's `video` port needs for `graph` to count its writes."""

    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        self.video_from_upstream.read()
