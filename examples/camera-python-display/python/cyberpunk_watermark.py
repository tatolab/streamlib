# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk spray paint watermark processor using Skia GPU rendering.

Features:
- Spray paint style "CL" tag with drips and neon glow
- Animated drip effects
- Zero-copy GPU texture sharing via IOSurface
"""

import logging
import math

import skia

from streamlib import processor, input, output

logger = logging.getLogger(__name__)

# OpenGL constants
GL_RGBA8 = 0x8058


# =============================================================================
# Spray Paint Tag Drawing
# =============================================================================

def draw_spray_paint_tag(canvas, x, y, scale=1.0, time_offset=0.0):
    """Draw a spray paint style tag with drips and glow.

    Creates a cyberpunk-inspired watermark that looks like street art.
    """
    # Colors: neon cyan and magenta
    cyan = skia.Color(0, 255, 255, 255)
    magenta = skia.Color(255, 0, 255, 255)

    # === GLOW LAYER (underneath) ===
    glow_paint = skia.Paint(
        Color=skia.Color(0, 255, 255, 80),
        AntiAlias=True,
        MaskFilter=skia.MaskFilter.MakeBlur(skia.BlurStyle.kNormal_BlurStyle, 15 * scale),
    )

    # Draw glow for the main symbol
    canvas.save()
    canvas.translate(x, y)
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

    # === DRIP EFFECTS ===
    # Animated drips from the bottom of letters
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

    # === ACCENT DOTS (spray paint splatter) ===
    splatter_paint = skia.Paint(
        Color=magenta,
        AntiAlias=True,
    )

    # Small dots around the tag
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

    # === HIGHLIGHT ACCENT ===
    # Small white highlight on the C
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


# =============================================================================
# Cyberpunk Watermark Processor
# =============================================================================

@processor(name="CyberpunkWatermark", description="Spray paint watermark overlay")
class CyberpunkWatermark:
    """Draws animated spray paint watermark in lower-right corner.

    Features:
    - Neon cyan "CL" circuit-style logo
    - Animated drip effects
    - Magenta splatter accents
    - Zero-copy GPU rendering via Skia + IOSurface
    """

    @input(schema="VideoFrame")
    def video_in(self):
        pass

    @output(schema="VideoFrame")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Initialize Skia with StreamLib's GL context."""
        self.frame_count = 0

        # Get StreamLib's GL context
        self.gl_ctx = ctx.gpu._experimental_gl_context()
        self.gl_ctx.make_current()

        # Create Skia GPU context
        self.skia_ctx = skia.GrDirectContext.MakeGL()
        if self.skia_ctx is None:
            raise RuntimeError("Failed to create Skia GL context")

        # Cache for Skia surfaces
        self._surface_cache = {}

        logger.info("Cyberpunk Watermark processor initialized")

    def _get_or_create_surface(self, width, height, gl_tex_id, gl_target):
        """Get cached Skia surface or create new one."""
        cache_key = (width, height, gl_tex_id)

        if cache_key in self._surface_cache:
            return self._surface_cache[cache_key]

        gl_info = skia.GrGLTextureInfo(gl_target, gl_tex_id, GL_RGBA8)
        backend_texture = skia.GrBackendTexture(
            width, height, skia.GrMipmapped.kNo, gl_info
        )

        surface = skia.Surface.MakeFromBackendTexture(
            self.skia_ctx,
            backend_texture,
            skia.GrSurfaceOrigin.kTopLeft_GrSurfaceOrigin,
            0,
            skia.ColorType.kRGBA_8888_ColorType,
            None,
            None,
        )

        if surface is None:
            raise RuntimeError(f"Failed to create Skia surface from GL texture {gl_tex_id}")

        if len(self._surface_cache) > 10:
            self._surface_cache.clear()
        self._surface_cache[cache_key] = surface

        return surface

    def process(self, ctx):
        """Draw watermark overlay on each frame."""
        frame = ctx.input("video_in").get()
        if frame is None:
            return

        width = frame["width"]
        height = frame["height"]
        input_texture = frame["texture"]

        # Acquire output surface
        output_tex = ctx.gpu.acquire_surface(width, height)

        # Bind textures to GL
        input_gl_id = input_texture._experimental_gl_texture_id(self.gl_ctx)
        output_gl_id = output_tex._experimental_gl_texture_id(self.gl_ctx)
        gl_target = self.gl_ctx.texture_target

        # Get Skia surface for output
        output_surface = self._get_or_create_surface(width, height, output_gl_id, gl_target)
        canvas = output_surface.getCanvas()

        # Create input image
        input_gl_info = skia.GrGLTextureInfo(gl_target, input_gl_id, GL_RGBA8)
        input_backend = skia.GrBackendTexture(width, height, skia.GrMipmapped.kNo, input_gl_info)
        input_image = skia.Image.MakeFromTexture(
            self.skia_ctx,
            input_backend,
            skia.GrSurfaceOrigin.kTopLeft_GrSurfaceOrigin,
            skia.ColorType.kRGBA_8888_ColorType,
            skia.AlphaType.kPremul_AlphaType,
            None,
        )

        if input_image is None:
            logger.warning("Failed to create input image, passing through")
            ctx.output("video_out").set(frame)
            return

        # Draw input frame
        canvas.drawImage(input_image, 0, 0)

        # Draw watermark in lower-right corner
        elapsed = ctx.time.elapsed_secs
        tag_scale = min(width, height) / 800.0
        tag_x = width - 100 * tag_scale
        tag_y = height - 80 * tag_scale
        draw_spray_paint_tag(canvas, tag_x, tag_y, tag_scale, elapsed)

        # Flush
        output_surface.flushAndSubmit()
        self.gl_ctx.flush()

        # Output
        ctx.output("video_out").set({
            "texture": output_tex.texture,
            "width": width,
            "height": height,
            "timestamp_ns": frame["timestamp_ns"],
            "frame_number": frame["frame_number"],
        })

        self.frame_count += 1
        if self.frame_count % 120 == 0:
            logger.debug(f"Watermark: {self.frame_count} frames")

    def teardown(self, ctx):
        """Cleanup."""
        self._surface_cache.clear()
        if self.skia_ctx:
            self.skia_ctx.abandonContext()
        logger.info(f"Cyberpunk Watermark shutdown ({self.frame_count} frames)")
