# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk spray paint watermark processor - CONTINUOUS RGBA GENERATOR.

Isolated subprocess processor using standalone CGL context.
Generates watermark overlay frames independently,
outputting transparent RGBA textures for compositing by the BlendingCompositor.

Features:
- Spray paint style "CL" tag with drips and neon glow
- Animated drip effects
- Runs at 60fps (16ms interval) continuous mode
- Zero-copy GPU texture via IOSurface + CGL binding
- Outputs transparent RGBA (alpha=0 background for layer compositing)
- Static elements cached as Skia Picture (drawn once, replayed each frame)
"""

import logging
import math
import time

import skia
from OpenGL.GL import glGenTextures

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

class CyberpunkWatermark:
    """Generates animated spray paint watermark as transparent RGBA texture.

    Isolated subprocess processor with own CGL context.
    Outputs standalone RGBA textures with transparent backgrounds
    for compositing by BlendingCompositor.
    """

    def setup(self, ctx):
        """Initialize standalone CGL context and Skia."""
        from streamlib.cgl_context import create_cgl_context, make_current

        self.frame_count = 0
        self.frame_number = 0
        self._start_time = time.monotonic()

        # Output dimensions
        self.width = DEFAULT_WIDTH
        self.height = DEFAULT_HEIGHT

        # Create standalone CGL context (own GPU context, not host's)
        self.cgl_ctx = create_cgl_context()
        make_current(self.cgl_ctx)

        # Create Skia GPU context on our CGL context
        self.skia_ctx = skia.GrDirectContext.MakeGL()
        if self.skia_ctx is None:
            raise RuntimeError("Failed to create Skia GL context")

        # Create persistent GL texture for output
        self.output_tex_id = glGenTextures(1)

        # Skia surface (recreated each frame when IOSurface backing changes)
        self.output_skia_surface = None

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
        from streamlib.cgl_context import make_current, bind_iosurface_to_texture, flush

        make_current(self.cgl_ctx)

        # Acquire output surface from Rust pool
        surface_id, handle = ctx.gpu.acquire_surface(
            width=self.width, height=self.height, format="bgra"
        )

        # Bind IOSurface as GL texture (zero-copy)
        bind_iosurface_to_texture(
            self.cgl_ctx, self.output_tex_id,
            handle.iosurface_ref, self.width, self.height
        )

        # Create Skia surface from GL texture (recreate each frame since texture backing changes)
        from streamlib.cgl_context import GL_TEXTURE_RECTANGLE
        output_gl_info = skia.GrGLTextureInfo(
            GL_TEXTURE_RECTANGLE, self.output_tex_id, GL_RGBA8
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
            handle.release()
            logger.error("Failed to create Skia surface from IOSurface texture")
            return

        # Get canvas from output surface
        canvas = self.output_skia_surface.getCanvas()

        # Clear to fully transparent (alpha=0) for layer compositing
        canvas.clear(skia.Color(0, 0, 0, 0))

        # Draw static elements from cached picture (fast replay)
        canvas.drawPicture(self.static_picture)

        # Draw animated elements (drips, splatter dots)
        elapsed = time.monotonic() - self._start_time
        draw_animated_watermark(canvas, self.width, self.height, self.tag_scale, elapsed)

        # Flush Skia and GL
        self.output_skia_surface.flushAndSubmit()
        flush()

        # Release IOSurface reference
        handle.release()

        # Output VideoFrame msgpack array
        timestamp_ns = int(elapsed * 1_000_000_000)
        frame = [
            str(self.frame_number),   # index 0: frame_index
            self.height,              # index 1: height
            surface_id,               # index 2: surface_id (UUID from acquire)
            str(timestamp_ns),        # index 3: timestamp_ns
            self.width,               # index 4: width
        ]
        ctx.outputs.write("video_out", frame)

        self.frame_count += 1
        self.frame_number += 1

        if self.frame_count == 1:
            logger.info(f"Watermark: First frame generated ({self.width}x{self.height})")
        if self.frame_count % 300 == 0:
            # Purge unlocked GPU resources to prevent Skia memory accumulation
            self.skia_ctx.freeGpuResources()
        if self.frame_count % 300 == 0:
            logger.debug(f"Watermark: generated {self.frame_count} frames")

    def teardown(self, ctx):
        """Cleanup."""
        self.static_picture = None  # Release picture before context
        if hasattr(self, 'skia_ctx') and self.skia_ctx:
            self.skia_ctx.abandonContext()
        if hasattr(self, 'cgl_ctx'):
            from streamlib.cgl_context import destroy_cgl_context
            destroy_cgl_context(self.cgl_ctx)
        logger.info(f"Cyberpunk Watermark shutdown ({self.frame_count} frames)")
