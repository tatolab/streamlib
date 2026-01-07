# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk-style lower third overlay processor - CONTINUOUS SOURCE.

This processor generates lower third overlay frames independently,
not dependent on incoming video. It outputs frames with transparent
background that can be composited with the video stream.

Features:
- Slide-in animation with "back" easing (overshoot) for snappy Cyberpunk feel
- Cyberpunk 2077 color palette (cyan, magenta, yellow accents)
- Bitter font (from Cyberpunk website)
- Scan line effects
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
# Cyberpunk Color Palette
# =============================================================================

# Primary colors from Cyberpunk 2077
CYBER_CYAN = skia.Color(0, 240, 255, 255)        # #00f0ff - main UI color
CYBER_MAGENTA = skia.Color(255, 0, 60, 255)      # #ff003c - accent/warning
CYBER_YELLOW = skia.Color(252, 238, 10, 255)    # #fcee0a - highlight
CYBER_DARK = skia.Color(10, 10, 15, 255)        # Near black background
CYBER_DARK_BLUE = skia.Color(15, 25, 40, 230)   # Dark blue panel


# =============================================================================
# Animation Easing Functions
# =============================================================================

def ease_out_back(t: float, overshoot: float = 1.70158) -> float:
    """Back easing - overshoots then settles. Classic Cyberpunk UI feel."""
    t = t - 1
    return t * t * ((overshoot + 1) * t + overshoot) + 1


def ease_out_expo(t: float) -> float:
    """Exponential ease out - fast start, slow end."""
    return 1 if t == 1 else 1 - pow(2, -10 * t)


# =============================================================================
# Lower Third Drawing
# =============================================================================

def draw_lower_third(
    canvas: skia.Canvas,
    width: int,
    height: int,
    headline: str,
    subtitle: str,
    elapsed: float,
    typeface: skia.Typeface,
):
    """Draw Cyberpunk-style lower third with slide-in animation."""

    # Animation timing
    slide_duration = 0.6  # seconds

    # Calculate animation progress
    if elapsed < slide_duration:
        # Slide in phase
        progress = ease_out_back(elapsed / slide_duration, overshoot=2.0)
    else:
        progress = 1.0

    # Lower third dimensions
    panel_height = height * 0.18
    panel_y = height - panel_height - (height * 0.08)  # 8% from bottom
    panel_width = width * 0.45

    # Slide from left
    panel_x = -panel_width + (panel_width + width * 0.05) * progress

    # === MAIN PANEL BACKGROUND ===
    panel_rect = skia.Rect.MakeXYWH(panel_x, panel_y, panel_width, panel_height)

    # Dark background with slight transparency
    bg_paint = skia.Paint(
        Color=CYBER_DARK_BLUE,
        AntiAlias=True,
    )
    canvas.drawRect(panel_rect, bg_paint)

    # === CYAN ACCENT LINE (left edge) ===
    accent_width = 4
    accent_rect = skia.Rect.MakeXYWH(panel_x, panel_y, accent_width, panel_height)
    accent_paint = skia.Paint(
        Color=CYBER_CYAN,
        AntiAlias=True,
    )
    canvas.drawRect(accent_rect, accent_paint)

    # === TOP ACCENT LINE (horizontal) ===
    top_line_rect = skia.Rect.MakeXYWH(panel_x, panel_y, panel_width, 2)
    canvas.drawRect(top_line_rect, accent_paint)

    # === DIAGONAL CUT (top right corner) ===
    cut_size = 25
    cut_path = skia.Path()
    cut_path.moveTo(panel_x + panel_width - cut_size, panel_y)
    cut_path.lineTo(panel_x + panel_width, panel_y)
    cut_path.lineTo(panel_x + panel_width, panel_y + cut_size)
    cut_path.close()

    # Draw cut in transparent (creates angular look)
    cut_paint = skia.Paint(
        Color=skia.Color(0, 0, 0, 0),
        AntiAlias=True,
        BlendMode=skia.BlendMode.kClear,
    )
    canvas.drawPath(cut_path, cut_paint)

    # === GLOWING CORNER ACCENT ===
    corner_paint = skia.Paint(
        Color=CYBER_YELLOW,
        AntiAlias=True,
    )
    corner_rect = skia.Rect.MakeXYWH(
        panel_x + panel_width - cut_size - 3,
        panel_y + cut_size - 3,
        6, 6
    )
    canvas.drawRect(corner_rect, corner_paint)

    # === HEADLINE TEXT ===
    headline_size = panel_height * 0.35
    headline_font = skia.Font(typeface, headline_size)
    headline_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)

    # Text position
    text_x = panel_x + accent_width + 20
    text_y = panel_y + panel_height * 0.45

    # Glow effect (draw text multiple times with blur)
    glow_paint = skia.Paint(
        Color=skia.Color(0, 240, 255, 60),
        AntiAlias=True,
        MaskFilter=skia.MaskFilter.MakeBlur(skia.BlurStyle.kNormal_BlurStyle, 8),
    )
    canvas.drawString(headline, text_x, text_y, headline_font, glow_paint)

    # Main headline text
    headline_paint = skia.Paint(
        Color=skia.Color(255, 255, 255, 255),
        AntiAlias=True,
    )
    canvas.drawString(headline, text_x, text_y, headline_font, headline_paint)

    # === SUBTITLE TEXT ===
    subtitle_size = panel_height * 0.22
    subtitle_font = skia.Font(typeface, subtitle_size)
    subtitle_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)

    subtitle_y = panel_y + panel_height * 0.75

    # Subtitle in cyan
    subtitle_paint = skia.Paint(
        Color=CYBER_CYAN,
        AntiAlias=True,
    )
    canvas.drawString(subtitle, text_x, subtitle_y, subtitle_font, subtitle_paint)

    # === SCAN LINES EFFECT ===
    if progress >= 1.0:
        scan_line_y = panel_y + ((elapsed * 50) % panel_height)
        scan_paint = skia.Paint(
            Color=skia.Color(0, 240, 255, 30),
            AntiAlias=True,
        )
        scan_rect = skia.Rect.MakeXYWH(panel_x, scan_line_y, panel_width, 2)
        canvas.drawRect(scan_rect, scan_paint)

    # === DECORATIVE ELEMENTS ===
    # Small bars on the right side
    bar_x = panel_x + panel_width - 60
    bar_width = 40

    for i in range(3):
        # Pulsing alpha (clamped to 0-255)
        bar_alpha = max(0, min(255, int(255 * (0.5 + 0.5 * math.sin(elapsed * 3 + i * 0.5)))))
        bar_paint = skia.Paint(
            Color=skia.Color(0, 240, 255, bar_alpha),
            AntiAlias=True,
        )
        bar_y = panel_y + panel_height - 15 - (i * 8)
        bar_rect = skia.Rect.MakeXYWH(bar_x, bar_y, bar_width - (i * 10), 3)
        canvas.drawRect(bar_rect, bar_paint)


# =============================================================================
# Cyberpunk Lower Third Processor (FILTER - composites onto input)
# =============================================================================

@processor(
    name="CyberpunkLowerThird",
    description="Cyberpunk-style lower third overlay filter",
    execution="Continuous"
)
class CyberpunkLowerThird:
    """Composites animated lower third overlay onto incoming video.

    This is a FILTER processor - it takes video input and overlays
    the lower third graphic onto it.

    Features:
    - Slide-in animation with back easing (overshoot)
    - Cyberpunk color palette
    - Bitter font for authentic look
    - Scan lines
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
        """Initialize Skia and load fonts."""
        self.frame_count = 0

        # Configuration
        self.headline = "NIGHT CITY NEWS"
        self.subtitle = "Live from Watson District"

        # Get GL context
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

        # Load Bitter font
        self.typeface = skia.Typeface.MakeFromName("Bitter", skia.FontStyle.Bold())
        if self.typeface is None:
            logger.warning("Bitter font not found, falling back to default")
            self.typeface = skia.Typeface.MakeDefault()

        logger.info(f"Cyberpunk Lower Third initialized as FILTER (font: {self.typeface.getFamilyName()})")

    def _ensure_resources(self, width: int, height: int, input_format: str):
        """Lazy-initialize GPU resources on first use or resize."""
        if self.output_pixel_buffer is not None and self._current_width == width and self._current_height == height:
            return

        self._current_width = width
        self._current_height = height

        # Create output pixel buffer using input format (passthrough - no conversion)
        self.output_pixel_buffer = self._gpu_ctx.acquire_pixel_buffer(width, height, input_format)
        logger.debug(f"Lower Third: acquired output buffer with format={input_format}")

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

        logger.info(f"Cyberpunk Lower Third: GPU resources initialized ({width}x{height})")

    def process(self, ctx):
        """Composite lower third overlay onto incoming video frame."""
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
        # Note: We create this each frame because the underlying buffer changes,
        # but the texture ID is stable so Skia can cache effectively
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

        # Draw lower third overlay on top
        elapsed = ctx.time.elapsed_secs
        draw_lower_third(
            canvas,
            width,
            height,
            self.headline,
            self.subtitle,
            elapsed,
            self.typeface,
        )

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
            logger.debug(f"Lower Third: processed {self.frame_count} frames")

    def teardown(self, ctx):
        """Cleanup."""
        if self.skia_ctx:
            self.skia_ctx.abandonContext()
        logger.info(f"Cyberpunk Lower Third shutdown ({self.frame_count} frames)")
