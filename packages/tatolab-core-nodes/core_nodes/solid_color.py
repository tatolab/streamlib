# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`SolidColor`: one flat colour at a set size, the picture a generator takes its size from."""

from __future__ import annotations

from typing import Annotated, Any

from tatolab.stream import (
    Color,
    ColorInfo,
    TextureToWrite,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    Texture,
    TexturePixelFormats,
    node,
)

#: The texture format a `SolidColor` publishes.
SOLID_COLOR_TEXTURE_FORMAT = "rgba8_unorm"

#: Two slots, so a frame published on a change of colour or size never waits on a reader
#: still holding the one before it.
SOLID_COLOR_OUTPUT_RING_DEPTH = 2

#: How often a `SolidColor` sends its picture: about 60 times a second. A shader node runs once
#: per picture it is sent, so this is the frame rate of every picture node downstream.
SOLID_COLOR_INTERVAL_BETWEEN_PICTURES_MS = 16

#: The colour space its colour's channels are read in: sRGB, full range.
SOLID_COLOR_COLOUR = ColorInfo(primaries="bt709", transfer="srgb", matrix="identity", range="full")


def rgba_bytes_of_the_color(color: Color) -> tuple[int, int, int, int]:
    """`color` as four 8-bit channels, each held between 0 and 255."""
    return tuple(  # type: ignore[return-value]
        round(min(max(channel, 0.0), 1.0) * 255.0)
        for channel in (color.red, color.green, color.blue, color.alpha)
    )


@node.meta(
    category="Generators",
    description="A flat colour at the size you set, sent about 60 times a second. Also the blank canvas a generator such as Noise or Gradient takes its size and its frame rate from.",
)
@node(execution="continuous", interval_ms=SOLID_COLOR_INTERVAL_BETWEEN_PICTURES_MS)
class SolidColor:
    """Sends the picture of its colour and size at each interval, filled again only when
    either has changed since it was last filled."""

    width: Annotated[int, NumberRange(minimum=1)] = 1920
    height: Annotated[int, NumberRange(minimum=1)] = 1080
    color: Color = Color(0.0, 0.0, 0.0, 1.0)
    image: OutputPin[Annotated[Texture, TexturePixelFormats(SOLID_COLOR_TEXTURE_FORMAT)]]

    def __init__(self) -> None:
        self._output_texture_ring = NodeOutputTextureRing(
            SOLID_COLOR_TEXTURE_FORMAT, ["texture_binding"], depth=SOLID_COLOR_OUTPUT_RING_DEPTH
        )
        self._filled_last: "tuple[int, int, Color] | None" = None
        self._frame_filled_last: Any = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        wanted = (self.width, self.height, self.color)
        if wanted != self._filled_last:
            self._frame_filled_last = self._filled_frame(ctx)
            self._filled_last = wanted
        self.image.write(self._frame_filled_last)

    def _filled_frame(self, ctx: RuntimeContextLimitedAccess) -> TextureToWrite:
        width, height = self.width, self.height
        slot = self._output_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, width, height
        )
        slot.lock(read_only=False)
        try:
            slot.as_numpy()[:height, :width] = rgba_bytes_of_the_color(self.color)
        finally:
            slot.unlock()
        return TextureToWrite.of_texture(slot, SOLID_COLOR_COLOUR)
