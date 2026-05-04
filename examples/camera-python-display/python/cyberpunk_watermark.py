# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk spray paint watermark processor — continuous RGBA generator.

Linux subprocess processor on the canonical surface-adapter pattern
(#485). Companion to ``cyberpunk_lower_third`` — same shape (host
pre-registers a render-target DMA-BUF VkImage, this processor draws
into it via :class:`SkiaContext` every tick), different visual: a
spray-paint "CL" tag with neon glow + animated drips.

Features:
- Spray paint style "CL" tag with drips and neon glow
- Animated drip effects
- Runs at 60fps (16ms interval) continuous mode
- Zero-copy GPU texture via DMA-BUF VkImage + EGL DMA-BUF import
- Outputs transparent RGBA (alpha=0 background for layer compositing)
- Static elements cached as Skia Picture (drawn once, replayed each frame)

macOS support was removed in #485 alongside the engine compositor
rewrite — see ``cyberpunk_lower_third.py`` for the full rationale.

Config keys (set by `examples/camera-python-display/src/linux.rs`):
    output_surface_uuid (str) — pre-registered render-target DMA-BUF VkImage.
    width, height (int) — surface dimensions.
"""

import logging
import math
import time

import skia

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.skia import SkiaContext
from streamlib.surface_adapter import StreamlibSurface, SurfaceFormat, SurfaceUsage

logger = logging.getLogger(__name__)


# =============================================================================
# Spray Paint Tag Drawing — Static Elements (cached as Skia Picture)
# =============================================================================

def record_static_watermark(width, height, scale):
    """Record the static watermark elements (glow, main stroke,
    highlight accent) to a `skia.Picture` — drawn once and replayed
    each frame inside the processor's `acquire_write` scope."""
    tag_x = width - 100 * scale
    tag_y = height - 80 * scale

    recorder = skia.PictureRecorder()
    bounds = skia.Rect.MakeWH(width, height)
    canvas = recorder.beginRecording(bounds)

    cyan = skia.Color(0, 255, 255, 255)

    # === GLOW LAYER (underneath) ===
    glow_paint = skia.Paint(
        Color=skia.Color(0, 255, 255, 80),
        AntiAlias=True,
        MaskFilter=skia.MaskFilter.MakeBlur(skia.BlurStyle.kNormal_BlurStyle, 15 * scale),
    )

    canvas.save()
    canvas.translate(tag_x, tag_y)
    canvas.scale(scale, scale)

    # Stylized "CL" symbol — circuit-trace inspired.
    symbol_path = skia.Path()
    # "C" shape — angular, circuit-like
    symbol_path.moveTo(40, 10)
    symbol_path.lineTo(15, 10)
    symbol_path.lineTo(10, 15)
    symbol_path.lineTo(10, 45)
    symbol_path.lineTo(15, 50)
    symbol_path.lineTo(40, 50)
    # "L" shape connected
    symbol_path.moveTo(50, 10)
    symbol_path.lineTo(50, 45)
    symbol_path.lineTo(55, 50)
    symbol_path.lineTo(80, 50)
    # Connection trace
    symbol_path.moveTo(42, 30)
    symbol_path.lineTo(48, 30)

    canvas.drawPath(symbol_path, glow_paint)

    # === MAIN STROKE LAYER ===
    stroke_paint = skia.Paint(
        Color=cyan,
        AntiAlias=True,
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=4,
        StrokeCap=skia.Paint.kRound_Cap,
        StrokeJoin=skia.Paint.kRound_Join,
    )
    canvas.drawPath(symbol_path, stroke_paint)

    # === HIGHLIGHT ACCENT ===
    highlight_paint = skia.Paint(
        Color=skia.Color(255, 255, 255, 200),
        AntiAlias=True,
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=1.5,
    )
    highlight_path = skia.Path()
    highlight_path.moveTo(15, 15)
    highlight_path.lineTo(12, 20)
    canvas.drawPath(highlight_path, highlight_paint)

    canvas.restore()
    return recorder.finishRecordingAsPicture()


def draw_animated_watermark(canvas, width, height, scale, time_offset):
    """Draw only the animated elements (drips with varying length,
    splatter dots with varying alpha)."""
    tag_x = width - 100 * scale
    tag_y = height - 80 * scale

    cyan = skia.Color(0, 255, 255, 255)
    magenta = skia.Color(255, 0, 255, 255)

    canvas.save()
    canvas.translate(tag_x, tag_y)
    canvas.scale(scale, scale)

    # === DRIP EFFECTS (animated length) ===
    drip_paint = skia.Paint(
        Color=cyan,
        AntiAlias=True,
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=3,
        StrokeCap=skia.Paint.kRound_Cap,
    )

    drip1_length = 15 + 10 * math.sin(time_offset * 2)
    drip1 = skia.Path()
    drip1.moveTo(25, 50)
    drip1.quadTo(26, 50 + drip1_length * 0.5, 24, 50 + drip1_length)
    canvas.drawPath(drip1, drip_paint)

    drip2_length = 20 + 8 * math.sin(time_offset * 2.5 + 1)
    drip2 = skia.Path()
    drip2.moveTo(65, 50)
    drip2.quadTo(66, 50 + drip2_length * 0.6, 64, 50 + drip2_length)
    canvas.drawPath(drip2, drip_paint)

    drip3_length = 8 + 5 * math.sin(time_offset * 3 + 2)
    drip3 = skia.Path()
    drip3.moveTo(75, 50)
    drip3.lineTo(75, 50 + drip3_length)
    canvas.drawPath(drip3, drip_paint)

    # === ACCENT DOTS (animated alpha) ===
    splatter_paint = skia.Paint(Color=magenta, AntiAlias=True)
    dots = [
        (5, 25, 2),
        (85, 15, 2.5),
        (88, 45, 1.5),
        (3, 48, 2),
        (45, 5, 1.5),
    ]
    for dx, dy, r in dots:
        splatter_paint.setAlpha(int(150 + 50 * math.sin(time_offset * 4 + dx)))
        canvas.drawCircle(dx, dy, r, splatter_paint)

    canvas.restore()


# =============================================================================
# Cyberpunk Watermark Processor
# =============================================================================

class CyberpunkWatermark:
    """Continuous RGBA generator drawing a Cyberpunk spray-paint
    watermark into a host-pre-registered DMA-BUF render-target via
    Skia-on-GL.

    Output frames carry the host's surface UUID; the BlendingCompositor
    consumes them as `watermark_in` and alpha-composites onto the
    base camera layer.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._uuid = str(cfg["output_surface_uuid"])
        self.width = int(cfg["width"])
        self.height = int(cfg["height"])

        self.frame_count = 0
        self.frame_number = 0
        self._start_time = time.monotonic()

        self.tag_scale = min(self.width, self.height) / 800.0

        # Picture cache — drawn once, replayed every frame.
        self.static_picture = record_static_watermark(
            self.width, self.height, self.tag_scale
        )

        # In-process consumer (BlendingCompositor) — match
        # AvatarCharacter's pattern: SkiaContext.acquire_write only,
        # no producer-side QFOT release.
        self._skia_ctx = SkiaContext.from_runtime(ctx)
        self._surface = StreamlibSurface(
            id=self._uuid,
            width=self.width,
            height=self.height,
            format=int(SurfaceFormat.BGRA8),
            usage=int(SurfaceUsage.RENDER_TARGET | SurfaceUsage.SAMPLED),
        )

        logger.info(
            f"Cyberpunk Watermark initialized as GENERATOR "
            f"({self.width}x{self.height}, uuid={self._uuid}, static picture cached)"
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        elapsed = time.monotonic() - self._start_time

        try:
            with self._skia_ctx.acquire_write(self._surface) as guard:
                canvas = guard.surface.getCanvas()
                # Clear to fully transparent (alpha=0) for layer compositing.
                canvas.clear(skia.Color(0, 0, 0, 0))
                # Replay static + animated.
                canvas.drawPicture(self.static_picture)
                draw_animated_watermark(
                    canvas, self.width, self.height, self.tag_scale, elapsed,
                )
        except Exception as e:
            if self.frame_count % 60 == 0:
                logger.warning(
                    f"Cyberpunk Watermark: acquire_write / draw failed: {e}"
                )
            return

        timestamp_ns = int(elapsed * 1_000_000_000)
        ctx.outputs.write("video_out", {
            "frame_index": str(self.frame_number),
            "height": self.height,
            "surface_id": self._uuid,
            "timestamp_ns": str(timestamp_ns),
            "width": self.width,
        })

        self.frame_count += 1
        self.frame_number += 1

        if self.frame_count == 1:
            logger.info(
                f"Cyberpunk Watermark: First frame generated "
                f"({self.width}x{self.height})"
            )
        if self.frame_count % 300 == 0:
            logger.debug(f"Cyberpunk Watermark: generated {self.frame_count} frames")

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        # Release the cached Picture before SkiaContext goes away.
        self.static_picture = None
        logger.info(f"Cyberpunk Watermark shutdown ({self.frame_count} frames)")
