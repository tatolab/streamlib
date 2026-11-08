# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Reads every decoded frame and keeps none, so `graph` counts what the decoder writes."""

from tatolab.stream import Image, Input, RuntimeContextLimitedAccess, node


@node
class DecodedVideoFrameSink:
    """The reader a decoder's `video` port needs for `graph` to count its writes."""

    video_from_upstream = Input(Image, delivery="newest")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read(self.video_from_upstream)
