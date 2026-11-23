# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A source of frames whose every pixel is known, and a sink that reports the pixels a shader node wrote.

The source writes a horizontal ramp into its own texture ring in the format its
`texture_format` names, so the shader node's input is a texture another process
published. The sink reports one `MARKER:PROBE_RESULT` line per frame it reads.
"""

import json
import os
import traceback
from typing import Annotated

from tatolab.stream import (
    Color,
    ColorInfo,
    ImageFrameToWrite,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    log,
    node,
)

FRAME_WIDTH = 64
FRAME_HEIGHT = 16
FRAMES_TO_PUBLISH = 8
RAMP_RED_STEP = 4
CONSTANT_GREEN = 100
CONSTANT_BLUE = 60
CONSTANT_ALPHA = 255
SAMPLED_COLUMNS = (0, 17, 40, 63)

RESULT_MARKER = "MARKER:PROBE_RESULT "


def known_pixel_at_column(column: int) -> "list[int]":
    """The RGBA bytes the source writes at `column` of every row."""
    return [column * RAMP_RED_STEP, CONSTANT_GREEN, CONSTANT_BLUE, CONSTANT_ALPHA]


def _report(probe_name: str, observation_body) -> None:
    try:
        observation = observation_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"probe": probe_name, "pid": os.getpid(), **observation}))


#: What `QuarterAmount` hands out: an amount no shader node of these scenarios has as its default.
QUARTER_AMOUNT = 0.25


@node
def QuarterAmount() -> float:  # noqa: N802
    """A quarter, whatever the time: a function node with a value to link into a shader node."""
    return QUARTER_AMOUNT


#: What `HalfRedQuarterGreen` hands out: a Color no shader node of these scenarios has as its default.
HALF_RED_QUARTER_GREEN = Color(0.5, 0.25, 1.0)


@node
def HalfRedQuarterGreen() -> Color:  # noqa: N802
    """A Color, whatever the time: a function node with a Color to link into a shader node."""
    return HALF_RED_QUARTER_GREEN


#: The formats the source can write its ramp in.
KNOWN_PIXELS_TEXTURE_FORMATS = ("rgba8_unorm", "bgra8_unorm")


@node(execution="continuous", interval_ms=30)
class KnownPixelsVideoSource:
    """Publishes a red ramp, constant green, blue and alpha, from its own texture ring."""

    video: OutputPin[Annotated[Texture, TexturePixelFormats(*KNOWN_PIXELS_TEXTURE_FORMATS)]]

    texture_format: str = "rgba8_unorm"
    frames_to_publish: int = FRAMES_TO_PUBLISH

    def __init__(self) -> None:
        self._texture_format = self.texture_format
        self._frames_to_publish = self.frames_to_publish
        self._output_texture_ring = NodeOutputTextureRing(
            self.texture_format, ["texture_binding"], depth=4
        )
        self._frames_published = 0

    def process(self, ctx) -> None:
        if self._frames_published >= self._frames_to_publish:
            return
        texture = self._output_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, FRAME_WIDTH, FRAME_HEIGHT
        )
        texture.lock(read_only=False)
        try:
            pixels = texture.as_numpy()
            for column in range(FRAME_WIDTH):
                red, green, blue, alpha = known_pixel_at_column(column)
                if self._texture_format.startswith("bgra"):
                    pixels[:, column] = [blue, green, red, alpha]
                else:
                    pixels[:, column] = [red, green, blue, alpha]
        finally:
            texture.unlock()
        self.video.write(ImageFrameToWrite.of_texture(texture, ColorInfo()))
        self._frames_published += 1


@node
class ShadedFramePixelReadingSink:
    """Reports the extent, format and sampled pixels of each frame a shader node wrote."""

    shaded: InputPin[Annotated[Texture, StreamDelivery("ordered")]]

    def process(self, ctx) -> None:
        frame = self.shaded.read()
        if frame is None:
            return

        def read_the_shaded_pixels() -> dict:
            with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
                surface.lock()
                pixels = surface.as_numpy()
                sampled = {
                    str(column): pixels[FRAME_HEIGHT // 2, column].tolist()
                    for column in SAMPLED_COLUMNS
                }
                surface.unlock()
            return {
                "extent": [frame.width, frame.height],
                "format": frame.format,
                "pixels_by_column": sampled,
            }

        _report("ShadedFramePixelReadingSink", read_the_shaded_pixels)


@node
class FrameDiscardingSink:
    """Reads each frame it is handed and keeps none: a second reader of the Texture feeding it."""

    discarded: InputPin[Annotated[Texture, StreamDelivery("newest")]]

    def process(self, ctx) -> None:
        self.discarded.read()
