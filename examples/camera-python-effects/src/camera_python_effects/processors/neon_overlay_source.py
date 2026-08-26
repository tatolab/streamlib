# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Publishes the skia overlay as a frame the compositor can sample.

A source, not an effect: it has no input port and runs on its own interval.
skia is not in the wheel's adapter closure and does not need to be — it is an
ordinary PyPI dependency of this app, drawing into host memory, and the CPU
door is how those pixels reach an engine surface. That door is the slow one and
says so in its name; an overlay redrawn at 30 Hz is what it is for.
"""

from __future__ import annotations

import numpy
import skia

from streamlib import (
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    output,
    processor,
)

from ..gpu_surface_conventions import (
    SAMPLED_ONLY_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    video_frame_bag_naming,
)
from ..neon_overlay_canvas import (
    OVERLAY_ALPHA_TYPE,
    OVERLAY_COLOR_TYPE,
    draw_neon_overlay,
)
from ..single_pass_video_effect import NANOSECONDS_PER_SECOND

OVERLAY_REDRAW_INTERVAL_MS = 33


@processor(
    execution="continuous",
    interval_ms=OVERLAY_REDRAW_INTERVAL_MS,
    description="Cyberpunk lower third and spray-paint watermark, drawn with skia",
)
class NeonOverlaySource:
    """A transparent RGBA layer, redrawn every tick."""

    def __init__(self, width: int = 1920, height: int = 1080) -> None:
        self.overlay_width = width
        self.overlay_height = height

    @output()
    def overlay_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.skia_surface = skia.Surface.MakeRaster(
            skia.ImageInfo.Make(
                self.overlay_width,
                self.overlay_height,
                OVERLAY_COLOR_TYPE,
                OVERLAY_ALPHA_TYPE,
            )
        )
        self.first_process_at_ns: int | None = None
        self.output_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_ONLY_TEXTURE_USAGE
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.first_process_at_ns is None:
            self.first_process_at_ns = ctx.time
        elapsed_seconds = (ctx.time - self.first_process_at_ns) / NANOSECONDS_PER_SECOND

        with self.skia_surface as canvas:
            draw_neon_overlay(
                canvas, self.overlay_width, self.overlay_height, elapsed_seconds
            )
        drawn_overlay = numpy.array(self.skia_surface.makeImageSnapshot())

        overlay_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, self.overlay_width, self.overlay_height
        )
        # `unlock` in a `finally` rather than a `with`: the handle's context
        # manager closes the surface on the way out, and this slot has to
        # outlive the frame — the ring owns it for the processor's life.
        overlay_texture.lock(read_only=False)
        try:
            overlay_texture.as_numpy()[...] = drawn_overlay
        finally:
            overlay_texture.unlock()

        ctx.outputs.write(
            "overlay_to_downstream",
            video_frame_bag_naming(
                overlay_texture.surface_id,
                self.overlay_width,
                self.overlay_height,
                ctx.time,
            ),
        )
