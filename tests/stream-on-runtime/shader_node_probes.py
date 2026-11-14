# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""A source of frames whose every pixel is known, and a sink that reports the pixels a shader node wrote.

The source writes a horizontal ramp into its own texture ring in the format its
config names, so the shader node's input is a texture another process
published. The sink reports one `MARKER:PROBE_RESULT` line per frame it reads.
"""

import dataclasses
import json
import os
import traceback

from tatolab.stream import (
    ColorInfo,
    Image,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    Output,
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


@node.function
def QuarterAmount() -> float:  # noqa: N802
    """A quarter, whatever the time: a function node with a value to link into a shader node."""
    return QUARTER_AMOUNT


@dataclasses.dataclass
class KnownPixelsVideoSourceConfig:
    texture_format: str = "rgba8_unorm"
    frames_to_publish: int = FRAMES_TO_PUBLISH


@node.process(execution="continuous", interval_ms=30)
class KnownPixelsVideoSource:
    """Publishes a red ramp, constant green, blue and alpha, from its own texture ring."""

    video = Output(Image)

    def __init__(self, config: KnownPixelsVideoSourceConfig) -> None:
        self._texture_format = config.texture_format
        self._frames_to_publish = config.frames_to_publish
        self._output_texture_ring = NodeOutputTextureRing(
            config.texture_format, ["texture_binding"], depth=4
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
        ctx.outputs.write(self.video, ImageFrameToWrite.of_texture(texture, ColorInfo()))
        self._frames_published += 1


@node.process
class ShadedFramePixelReadingSink:
    """Reports the extent, format and sampled pixels of each frame a shader node wrote."""

    shaded = Input(Image, delivery="ordered")

    def process(self, ctx) -> None:
        frame = ctx.inputs.read(self.shaded)
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


@node.process
class FrameDiscardingSink:
    """Reads each frame it is handed and keeps none: a second reader of the Image feeding it."""

    discarded = Input(Image, delivery="newest")

    def process(self, ctx) -> None:
        ctx.inputs.read(self.discarded)
