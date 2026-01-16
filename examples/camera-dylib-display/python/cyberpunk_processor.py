# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk video overlay processor using Skia GPU rendering.

This processor demonstrates GPU-accelerated 2D drawing on video frames using
skia-python with StreamLib's GL interop. Features:

- Cyberpunk color grading (teal shadows, magenta highlights, crushed blacks)
- Spray paint style watermark tag with drips and neon glow
- Zero-copy GPU texture sharing via IOSurface
- Stable GL texture IDs (create once, update per-frame)
"""

import logging
import math

import skia

from streamlib import processor, input, output

logger = logging.getLogger(__name__)

# OpenGL constants (not exposed by skia-python)
GL_RGBA8 = 0x8058


# =============================================================================
# Cyberpunk Color Grading - Subtle game-style color pop
# =============================================================================

def create_cyberpunk_color_matrix():
    """Create a subtle color matrix for Cyberpunk 2077 style.

    Lightly boosts reds, yellows, and purples while keeping the image natural.
    No bloom or overexposure - just subtle color enhancement.
    """
    # Color matrix format: [R, G, B, A, translate] for each row
    # Very light adjustments - the game uses subtle color grading
    return [
        1.08, 0.02, 0.03, 0, 0.0,    # Red: slight boost, tiny warmth
        0.0,  1.0,  0.0,  0, 0.0,    # Green: unchanged (preserves skin)
        0.03, 0.02, 1.06, 0, 0.0,    # Blue: slight boost for purple tones
        0,    0,    0,    1, 0,      # Alpha: unchanged
    ]


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
    white = skia.Color(255, 255, 255, 255)

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
# Cyberpunk Processor
# =============================================================================

@processor(name="CyberpunkProcessor", description="Cyberpunk overlay with Skia GPU rendering")
class CyberpunkProcessor:
    """Applies cyberpunk color grading and spray paint watermark using Skia.

    This processor demonstrates StreamLib's GL interop capabilities:
    - Input frame IOSurface is bound to an OpenGL texture
    - Skia draws directly on GPU via shared GL context
    - Output uses StreamLib's texture pool (IOSurface-backed)
    - Zero-copy pipeline: Metal -> GL -> Metal
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
        self._gpu_ctx = ctx.gpu

        # Get StreamLib's GL context
        self.gl_ctx = ctx.gpu._experimental_gl_context()
        self.gl_ctx.make_current()

        # Create Skia GPU context
        self.skia_ctx = skia.GrDirectContext.MakeGL()
        if self.skia_ctx is None:
            raise RuntimeError("Failed to create Skia GL context")

        # Create reusable texture bindings - these have STABLE texture IDs
        self.input_binding = self.gl_ctx.create_texture_binding()
        self.output_binding = self.gl_ctx.create_texture_binding()

        # Lazy init for output resources
        self.output_buffer = None
        self.output_surface = None
        self._current_width = 0
        self._current_height = 0

        # Simple color filter - subtle enhancement of reds, yellows, purples
        self.color_filter = skia.ColorFilters.Matrix(create_cyberpunk_color_matrix())

        logger.info("Cyberpunk processor initialized with subtle color grading")

    def _ensure_output_resources(self, width, height, input_format):
        """Ensure output buffer and surface are initialized for current size."""
        if self.output_buffer is not None and self._current_width == width and self._current_height == height:
            return

        self._current_width = width
        self._current_height = height

        # Create output pixel buffer
        self.output_buffer = self._gpu_ctx.acquire_pixel_buffer(width, height, input_format)

        # Update output binding (fast rebind, no new GL texture)
        self.output_binding.update(self.output_buffer)

        # Create Skia surface from output binding's STABLE texture ID
        gl_info = skia.GrGLTextureInfo(
            self.output_binding.target,
            self.output_binding.id,
            GL_RGBA8,
        )
        backend_texture = skia.GrBackendTexture(
            width,
            height,
            skia.GrMipmapped.kNo,
            gl_info,
        )
        self.output_surface = skia.Surface.MakeFromBackendTexture(
            self.skia_ctx,
            backend_texture,
            skia.GrSurfaceOrigin.kTopLeft_GrSurfaceOrigin,
            0,  # sample count
            skia.ColorType.kRGBA_8888_ColorType,
            None,  # color space
            None,  # surface props
        )

        if self.output_surface is None:
            raise RuntimeError(f"Failed to create Skia surface from GL texture")

        logger.info(f"Cyberpunk processor: GPU resources initialized ({width}x{height})")

    def process(self, ctx):
        """Apply subtle cyberpunk color grading and watermark to each frame."""
        frame = ctx.input("video_in").get()
        if frame is None:
            return

        # Get pixel buffer from frame (buffer-centric API)
        input_buffer = frame["pixel_buffer"]
        width = input_buffer.width
        height = input_buffer.height

        # Make GL context current
        self.gl_ctx.make_current()

        # Ensure output resources are initialized
        self._ensure_output_resources(width, height, input_buffer.format)

        # Update input binding to current frame's buffer (fast rebind)
        self.input_binding.update(input_buffer)

        # Get canvas from output surface
        canvas = self.output_surface.getCanvas()

        # Create input image from input binding's STABLE texture ID
        input_gl_info = skia.GrGLTextureInfo(
            self.input_binding.target,
            self.input_binding.id,
            GL_RGBA8
        )
        input_backend = skia.GrBackendTexture(
            width, height, skia.GrMipmapped.kNo, input_gl_info
        )
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

        # === Draw input with subtle color grading ===
        # Light boost to reds, yellows, purples - keeps image natural
        paint = skia.Paint(
            ColorFilter=self.color_filter,
        )
        canvas.drawImage(input_image, 0, 0, skia.SamplingOptions(), paint)

        # === DRAW SPRAY PAINT TAG ===
        elapsed = ctx.time.elapsed_secs
        tag_scale = min(width, height) / 800.0  # Scale based on resolution
        tag_x = width - 100 * tag_scale
        tag_y = height - 80 * tag_scale
        draw_spray_paint_tag(canvas, tag_x, tag_y, tag_scale, elapsed)

        # === FLUSH AND SYNC ===
        self.output_surface.flushAndSubmit()
        self.gl_ctx.flush()

        # Output the processed frame with pixel buffer
        ctx.output("video_out").set({
            "pixel_buffer": self.output_buffer,
            "timestamp_ns": frame["timestamp_ns"],
            "frame_number": frame["frame_number"],
        })

        # Log periodically and purge Skia caches to prevent memory growth
        self.frame_count += 1
        if self.frame_count % 60 == 0:
            # Purge unlocked GPU resources to prevent Skia memory accumulation
            self.skia_ctx.freeGpuResources()
        if self.frame_count % 120 == 0:
            logger.debug(f"Processed {self.frame_count} frames with cyberpunk effect")

    def teardown(self, ctx):
        """Cleanup Skia resources."""
        if self.skia_ctx:
            self.skia_ctx.abandonContext()

        logger.info(f"Cyberpunk processor shutdown. Processed {self.frame_count} frames")
