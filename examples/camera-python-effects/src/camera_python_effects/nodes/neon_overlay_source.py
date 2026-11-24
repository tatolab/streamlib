# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Publishes the skia overlay as a frame the compositor can sample.

A source, not an effect: it has no input port and runs on its own interval.
skia is an ordinary PyPI dependency of this stream, drawing into host memory,
and the CPU door is how those pixels reach an engine surface. That door is the
slow one; an overlay redrawn at 30 Hz is what it is for.
"""

from __future__ import annotations

from typing import Annotated

import numpy
import skia
from tatolab.stream import (
    Description,
    TextureToWrite,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    SettingOnly,
    Texture,
    TexturePixelFormats,
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


@node.meta(description="Cyberpunk lower third and spray-paint watermark, drawn with skia")
@node(execution="continuous", interval_ms=OVERLAY_REDRAW_INTERVAL_MS)
class NeonOverlaySource:
    """A transparent RGBA layer, redrawn every tick."""

    width: Annotated[
        int, SettingOnly(), NumberRange(minimum=1), Description("The overlay's width in pixels.")
    ] = 1920
    height: Annotated[
        int, SettingOnly(), NumberRange(minimum=1), Description("The overlay's height in pixels.")
    ] = 1080
    overlay_to_downstream: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(TEXTURE_FORMAT),
            Description("The overlay, premultiplied RGBA over a transparent frame"),
        ]
    ]

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # skia draws straight into this one host buffer; the surface borrows it
        # without holding a reference, so the node keeps it alive.
        self.overlay_host_pixels = numpy.zeros(
            (self.height, self.width, 4), dtype=numpy.uint8
        )
        self.skia_surface_over_overlay_host_pixels = skia.Surface(
            self.overlay_host_pixels, OVERLAY_COLOR_TYPE, OVERLAY_ALPHA_TYPE
        )
        self.elapsed_seconds_since_first_process = ElapsedSecondsSinceFirstProcess()
        self.output_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_ONLY_TEXTURE_USAGE
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        tick_time_ns = ctx.time
        elapsed_seconds = self.elapsed_seconds_since_first_process.seconds_at(
            tick_time_ns
        )

        with self.skia_surface_over_overlay_host_pixels as canvas:
            draw_neon_overlay(
                canvas, self.width, self.height, elapsed_seconds
            )

        overlay_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, self.width, self.height
        )
        copy_host_pixels_into_ring_texture(overlay_texture, self.overlay_host_pixels)

        self.overlay_to_downstream.write(
            TextureToWrite.of_texture(overlay_texture, HOST_DRAWN_FRAME_COLOUR),
            timestamp_ns=tick_time_ns,
        )
