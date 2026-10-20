# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Publishes the skia overlay as a frame the compositor can sample.

A source, not an effect: it has no input port and runs on its own interval.
skia is an ordinary PyPI dependency of this stream, drawing into host memory,
and the CPU door is how those pixels reach an engine surface. That door is the
slow one; an overlay redrawn at 30 Hz is what it is for.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy
import skia
from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

from ..elapsed_seconds_since_first_process import ElapsedSecondsSinceFirstProcess
from ..gpu_surface_conventions import (
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


@dataclass
class NeonOverlaySourceConfig:
    """NeonOverlaySource's settings, as `stream_builder.add(..., config={...})` spells them."""

    width: int = 1920
    height: int = 1080


@node(
    execution="continuous",
    interval_ms=OVERLAY_REDRAW_INTERVAL_MS,
    description="Cyberpunk lower third and spray-paint watermark, drawn with skia",
)
class NeonOverlaySource:
    """A transparent RGBA layer, redrawn every tick."""

    def __init__(self, config: NeonOverlaySourceConfig) -> None:
        self.overlay_width = config.width
        self.overlay_height = config.height

    @node.output()
    def overlay_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # skia draws straight into this one host buffer; the surface borrows it
        # without holding a reference, so the node keeps it alive.
        self.overlay_host_pixels = numpy.zeros(
            (self.overlay_height, self.overlay_width, 4), dtype=numpy.uint8
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
                canvas, self.overlay_width, self.overlay_height, elapsed_seconds
            )

        overlay_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, self.overlay_width, self.overlay_height
        )
        copy_host_pixels_into_ring_texture(overlay_texture, self.overlay_host_pixels)

        ctx.outputs.write(
            "overlay_to_downstream",
            VideoFrame(
                surface_id=overlay_texture.surface_id,
                width=self.overlay_width,
                height=self.overlay_height,
                timestamp_ns=tick_time_ns,
            ).to_bytes(),
            timestamp_ns=tick_time_ns,
        )
