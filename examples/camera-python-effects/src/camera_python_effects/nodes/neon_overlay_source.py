# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Publishes the skia overlay as a frame the compositor can sample.

A source, not an effect: it has no input port and runs on its own interval.
skia is an ordinary PyPI dependency of this stream, drawing into host memory,
and the CPU door is how those pixels reach an engine surface. That door is the
slow one; an overlay redrawn at 30 Hz is what it is for.
"""

from __future__ import annotations

from typing import Annotated, Callable

import numpy
import skia
from tatolab.stream import (
    Description,
    Input,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
    computed,
    node,
)

from ..elapsed_seconds_since_first_process import ElapsedSecondsSinceFirstProcess
from ..gpu_surface_conventions import (
    HOST_DRAWN_FRAME_COLOUR,
    SAMPLED_ONLY_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    copy_host_pixels_into_ring_texture,
)
from ..neon_overlay_canvas import (
    OVERLAY_ALPHA_TYPE,
    OVERLAY_COLOR_TYPE,
    draw_neon_overlay,
)

OVERLAY_REDRAW_INTERVAL_MS = 33


class OverlayHostPixels:
    """The host buffer skia draws the overlay into, at one size, and the skia surface over it."""

    def __init__(self, width: int, height: int) -> None:
        self.width = width
        self.height = height
        # skia draws straight into this one host buffer; the surface borrows it
        # without holding a reference, so this object keeps it alive.
        self.pixels = numpy.zeros((height, width, 4), dtype=numpy.uint8)
        self.skia_surface = skia.Surface(self.pixels, OVERLAY_COLOR_TYPE, OVERLAY_ALPHA_TYPE)


@node.meta(description="Cyberpunk lower third and spray-paint watermark, drawn with skia")
@node(execution="continuous", interval_ms=OVERLAY_REDRAW_INTERVAL_MS)
def NeonOverlaySource(
    *,
    width: Input[
        Annotated[int, NumberRange(minimum=1), Description("The overlay's width in pixels.")]
    ] = Input(1920),
    height: Input[
        Annotated[int, NumberRange(minimum=1), Description("The overlay's height in pixels.")]
    ] = Input(1080),
    overlay_to_downstream: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(TEXTURE_FORMAT),
            Description("The overlay, premultiplied RGBA over a transparent frame"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """A transparent RGBA layer, redrawn every tick, at the size its inputs hold."""
    host_pixels = computed(lambda: OverlayHostPixels(width.get(), height.get()))
    elapsed_seconds_since_first_process = ElapsedSecondsSinceFirstProcess()
    output_ring = NodeOutputTextureRing(TEXTURE_FORMAT, SAMPLED_ONLY_TEXTURE_USAGE)

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        tick_time_ns = ctx.time
        elapsed_seconds = elapsed_seconds_since_first_process.seconds_at(tick_time_ns)
        overlay = host_pixels.get()

        with overlay.skia_surface as canvas:
            draw_neon_overlay(canvas, overlay.width, overlay.height, elapsed_seconds)

        overlay_texture = output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, overlay.width, overlay.height
        )
        copy_host_pixels_into_ring_texture(overlay_texture, overlay.pixels)

        overlay_to_downstream.write(
            TextureToWrite.of_texture(overlay_texture, HOST_DRAWN_FRAME_COLOUR),
            timestamp_ns=tick_time_ns,
        )

    return each_frame
