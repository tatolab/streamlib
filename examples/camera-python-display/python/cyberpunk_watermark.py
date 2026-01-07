# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk spray paint watermark processor - CONTINUOUS SOURCE.

This processor generates watermark overlay frames independently,
not dependent on incoming video. It outputs frames with transparent
background that can be composited with the video stream.

Features:
- Spray paint style "CL" tag with drips and neon glow
- Animated drip effects
- Runs at display refresh rate (continuous mode)
- Zero-copy GPU texture binding (stable GL texture IDs)
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
# Cyberpunk Watermark Processor (FILTER - composites onto input)
# =============================================================================

@processor(
    name="CyberpunkWatermark",
    description="Spray paint watermark overlay filter",
    execution="Continuous"
)
class CyberpunkWatermark:
    """Composites animated spray paint watermark onto incoming video.

    This is a FILTER processor - it takes video input and overlays
    the watermark onto it.

    Features:
    - Neon cyan "CL" circuit-style logo
    - Animated drip effects
    - Magenta splatter accents
    - Zero-copy GPU rendering via Skia + IOSurface
    - Stable GL texture IDs (create once, update per-frame)
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

        # Create reusable texture bindings - these have STABLE texture IDs
        # Input binding: for reading camera frames
        self.input_binding = self.gl_ctx.create_texture_binding()
        # Output binding: for writing composited frames
        self.output_binding = self.gl_ctx.create_texture_binding()

        # Lazy initialization - defer pixel buffer creation to first process()
        # to avoid race with camera initialization
        self.output_pixel_buffer = None
        self.output_skia_surface = None
        self._gpu_ctx = ctx.gpu  # Store for lazy init
        self._current_width = 0
        self._current_height = 0

        logger.info("Cyberpunk Watermark initialized as FILTER")

    def _ensure_resources(self, width: int, height: int, input_format: str):
        """Lazy-initialize GPU resources on first use or resize."""
        if self.output_pixel_buffer is not None and self._current_width == width and self._current_height == height:
            return

        self._current_width = width
        self._current_height = height

        # Create output pixel buffer using input format (passthrough - no conversion)
        self.output_pixel_buffer = self._gpu_ctx.acquire_pixel_buffer(width, height, input_format)
        logger.debug(f"Watermark: acquired output buffer with format={input_format}")

        # Update output binding to point to the output buffer
        # This is a fast rebind operation (no new GL textures created)
        self.output_binding.update(self.output_pixel_buffer)

        # Create Skia surface from output binding's STABLE texture ID
        # We only recreate this on resize, not every frame
        output_gl_info = skia.GrGLTextureInfo(
            self.output_binding.target, self.output_binding.id, GL_RGBA8
        )
        output_backend = skia.GrBackendTexture(
            width, height, skia.GrMipmapped.kNo, output_gl_info
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

        logger.info(f"Cyberpunk Watermark: GPU resources initialized ({width}x{height})")

    def process(self, ctx):
        """Composite watermark overlay onto incoming video frame."""
        # Read input frame
        input_frame = ctx.input("video_in").get()
        if input_frame is None:
            return  # No input yet

        # Ensure GL context is current
        self.gl_ctx.make_current()

        # Get input frame dimensions (frame is a dict)
        width = input_frame["width"]
        height = input_frame["height"]
        pixel_buffer = input_frame["pixel_buffer"]
        timestamp_ns = input_frame["timestamp_ns"]
        frame_number = input_frame["frame_number"]
        input_format = pixel_buffer.format  # Passthrough: use input's format

        # Lazy-init GPU resources (deferred from setup to avoid race)
        self._ensure_resources(width, height, input_format)

        # Update input binding to point to current frame's buffer
        # This is FAST - just rebinds the existing GL texture to new IOSurface
        self.input_binding.update(pixel_buffer)

        # Create Skia image from input binding's STABLE texture ID
        input_gl_info = skia.GrGLTextureInfo(
            self.input_binding.target, self.input_binding.id, GL_RGBA8
        )
        input_backend = skia.GrBackendTexture(
            width, height, skia.GrMipmapped.kNo, input_gl_info
        )
        input_image = skia.Image.MakeFromTexture(
            self.skia_ctx,
            input_backend,
            skia.GrSurfaceOrigin.kTopLeft_GrSurfaceOrigin,
            skia.ColorType.kBGRA_8888_ColorType,
            skia.AlphaType.kPremul_AlphaType,
            None,
        )

        # Get canvas from output surface
        canvas = self.output_skia_surface.getCanvas()

        # Draw input frame first
        if input_image:
            canvas.drawImage(input_image, 0, 0)
        else:
            # Fallback: clear to black if image creation failed
            canvas.clear(skia.Color(0, 0, 0, 255))

        # Draw watermark in lower-right corner
        elapsed = ctx.time.elapsed_secs
        tag_scale = min(width, height) / 800.0
        tag_x = width - 100 * tag_scale
        tag_y = height - 80 * tag_scale
        draw_spray_paint_tag(canvas, tag_x, tag_y, tag_scale, elapsed)

        # Flush Skia and GL
        self.output_skia_surface.flushAndSubmit()
        self.gl_ctx.flush()

        # Output composited frame
        ctx.output("video_out").set({
            "pixel_buffer": self.output_pixel_buffer,
            "timestamp_ns": timestamp_ns,
            "frame_number": frame_number,
        })

        self.frame_count += 1
        if self.frame_count % 60 == 0:
            # Purge unlocked GPU resources to prevent Skia memory accumulation
            self.skia_ctx.freeGpuResources()
        if self.frame_count % 120 == 0:
            logger.debug(f"Watermark: processed {self.frame_count} frames")

    def teardown(self, ctx):
        """Cleanup."""
        if self.skia_ctx:
            self.skia_ctx.abandonContext()
        logger.info(f"Cyberpunk Watermark shutdown ({self.frame_count} frames)")
