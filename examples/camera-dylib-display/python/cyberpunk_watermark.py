# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk spray paint watermark processor - CONTINUOUS RGBA GENERATOR.

This processor generates watermark overlay frames independently,
outputting transparent RGBA textures for compositing by the BlendingCompositor.

Features:
- Spray paint style "CL" tag with drips and neon glow
- Animated drip effects
- Runs at 60fps (16ms interval) continuous mode
- Zero-copy GPU texture binding (stable GL texture IDs)
- Outputs transparent RGBA (alpha=0 background for layer compositing)
- Static elements cached as Skia Picture (drawn once, replayed each frame)
"""

import logging
import math

import skia

from streamlib import processor, output, PixelFormat

logger = logging.getLogger(__name__)

# OpenGL constants
GL_RGBA8 = 0x8058

# Default output dimensions (will be used for overlay generation)
DEFAULT_WIDTH = 1920
DEFAULT_HEIGHT = 1080


# =============================================================================
# Spray Paint Tag Drawing - Static Elements (cached as Picture)
# =============================================================================

def record_static_watermark(width, height, scale):
    """Record static watermark elements to a Skia Picture.

    Static elements: glow, main stroke, highlight accent.
    These are drawn once and replayed each frame.
    """
    # Calculate position (lower-right corner)
    tag_x = width - 100 * scale
    tag_y = height - 80 * scale

    # Record to picture
    recorder = skia.PictureRecorder()
    bounds = skia.Rect.MakeWH(width, height)
    canvas = recorder.beginRecording(bounds)

    # Colors
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

    # Main symbol: stylized "CL" in a circuit-like design
    symbol_path = skia.Path()

    # "C" shape - angular, circuit-like
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

    # Connection line (circuit trace)
    symbol_path.moveTo(42, 30)
    symbol_path.lineTo(48, 30)

    # Draw glow
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
    """Draw only the animated watermark elements.

    Animated elements: drips (length varies), splatter dots (alpha varies).
    """
    # Calculate position (lower-right corner)
    tag_x = width - 100 * scale
    tag_y = height - 80 * scale

    # Colors
    cyan = skia.Color(0, 255, 255, 255)
    magenta = skia.Color(255, 0, 255, 255)

    canvas.save()
    canvas.translate(tag_x, tag_y)
    canvas.scale(scale, scale)

    # === DRIP EFFECTS (animated) ===
    drip_paint = skia.Paint(
        Color=cyan,
        AntiAlias=True,
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=3,
        StrokeCap=skia.Paint.kRound_Cap,
    )

    # Drip 1 - from C bottom
    drip1_length = 15 + 10 * math.sin(time_offset * 2)
    drip1 = skia.Path()
    drip1.moveTo(25, 50)
    drip1.quadTo(26, 50 + drip1_length * 0.5, 24, 50 + drip1_length)
    canvas.drawPath(drip1, drip_paint)

    # Drip 2 - from L bottom
    drip2_length = 20 + 8 * math.sin(time_offset * 2.5 + 1)
    drip2 = skia.Path()
    drip2.moveTo(65, 50)
    drip2.quadTo(66, 50 + drip2_length * 0.6, 64, 50 + drip2_length)
    canvas.drawPath(drip2, drip_paint)

    # Drip 3 - smaller
    drip3_length = 8 + 5 * math.sin(time_offset * 3 + 2)
    drip3 = skia.Path()
    drip3.moveTo(75, 50)
    drip3.lineTo(75, 50 + drip3_length)
    canvas.drawPath(drip3, drip_paint)

    # === ACCENT DOTS (animated alpha) ===
    splatter_paint = skia.Paint(
        Color=magenta,
        AntiAlias=True,
    )

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
# Cyberpunk Watermark Processor (GENERATOR - outputs transparent RGBA)
# =============================================================================

@processor(
    name="CyberpunkWatermark",
    description="Spray paint watermark RGBA overlay generator",
    execution="Continuous",
)
class CyberpunkWatermark:
    """Generates animated spray paint watermark as transparent RGBA texture.

    This is a GENERATOR processor - it outputs standalone RGBA textures
    with transparent backgrounds for compositing by BlendingCompositor.

    Features:
    - Neon cyan "CL" circuit-style logo
    - Animated drip effects
    - Magenta splatter accents
    - Zero-copy GPU rendering via Skia + IOSurface
    - Transparent background (alpha=0) for layer compositing
    - Static elements cached as Skia Picture (glow, stroke, highlight)
    """

    @output(schema="VideoFrame")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Initialize Skia with StreamLib's GL context."""
        self.frame_count = 0
        self.frame_number = 0

        # Output dimensions
        self.width = DEFAULT_WIDTH
        self.height = DEFAULT_HEIGHT

        # Get StreamLib's GL context
        self.gl_ctx = ctx.gpu._experimental_gl_context()
        self.gl_ctx.make_current()

        # Create Skia GPU context
        self.skia_ctx = skia.GrDirectContext.MakeGL()
        if self.skia_ctx is None:
            raise RuntimeError("Failed to create Skia GL context")

        # Create output texture binding (stable GL texture ID)
        self.output_binding = self.gl_ctx.create_texture_binding()

        # Create output pixel buffer for RGBA output
        self._gpu_ctx = ctx.gpu
        self.output_pixel_buffer = self._gpu_ctx.acquire_pixel_buffer(
            self.width, self.height, PixelFormat.BGRA32
        )

        # Update output binding to point to the output buffer
        self.output_binding.update(self.output_pixel_buffer)

        # Create Skia surface from output binding's stable texture ID
        output_gl_info = skia.GrGLTextureInfo(
            self.output_binding.target, self.output_binding.id, GL_RGBA8
        )
        output_backend = skia.GrBackendTexture(
            self.width, self.height, skia.GrMipmapped.kNo, output_gl_info
        )
        self.output_skia_surface = skia.Surface.MakeFromBackendTexture(
            self.skia_ctx,
            output_backend,
            skia.GrSurfaceOrigin.kTopLeft_GrSurfaceOrigin,
            0,
            skia.ColorType.kBGRA_8888_ColorType,
            None,
            None,
        )

        if self.output_skia_surface is None:
            raise RuntimeError("Failed to create Skia surface from pixel buffer")

        # Cache scale for animation drawing
        self.tag_scale = min(self.width, self.height) / 800.0

        # Record static elements to Picture (drawn once, replayed each frame)
        self.static_picture = record_static_watermark(
            self.width, self.height, self.tag_scale
        )

        logger.info(
            f"Cyberpunk Watermark initialized as GENERATOR ({self.width}x{self.height}, "
            f"static picture cached)"
        )

    def process(self, ctx):
        """Generate watermark overlay frame with transparent background."""
        # Ensure GL context is current
        self.gl_ctx.make_current()

        # Get canvas from output surface
        canvas = self.output_skia_surface.getCanvas()

        # Clear to fully transparent (alpha=0) for layer compositing
        canvas.clear(skia.Color(0, 0, 0, 0))

        # Draw static elements from cached picture (fast replay)
        canvas.drawPicture(self.static_picture)

        # Draw animated elements (drips, splatter dots)
        elapsed = ctx.time.elapsed_secs
        draw_animated_watermark(canvas, self.width, self.height, self.tag_scale, elapsed)

        # Flush Skia and GL
        self.output_skia_surface.flushAndSubmit()
        self.gl_ctx.flush()

        # Output overlay frame
        timestamp_ns = int(elapsed * 1_000_000_000)
        ctx.output("video_out").set({
            "pixel_buffer": self.output_pixel_buffer,
            "timestamp_ns": timestamp_ns,
            "frame_number": self.frame_number,
        })

        self.frame_count += 1
        self.frame_number += 1

        if self.frame_count == 1:
            logger.info(f"Watermark: First frame generated ({self.width}x{self.height})")
        if self.frame_count % 300 == 0:
            # Purge unlocked GPU resources to prevent Skia memory accumulation
            # Every 300 frames (~5 seconds at 60fps) to avoid micro-stutters
            self.skia_ctx.freeGpuResources()
        if self.frame_count % 300 == 0:
            logger.debug(f"Watermark: generated {self.frame_count} frames")

    def teardown(self, ctx):
        """Cleanup."""
        self.static_picture = None  # Release picture before context
        if self.skia_ctx:
            self.skia_ctx.abandonContext()
        logger.info(f"Cyberpunk Watermark shutdown ({self.frame_count} frames)")
