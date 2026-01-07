# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk-style lower third overlay processor - CONTINUOUS RGBA GENERATOR.

This processor generates lower third overlay frames independently,
outputting transparent RGBA textures for compositing by the BlendingCompositor.

Features:
- Slide-in animation with "back" easing (overshoot) for snappy Cyberpunk feel
- Cyberpunk 2077 color palette (cyan, magenta, yellow accents)
- Bitter font (from Cyberpunk website)
- Scan line effects
- Runs at 60fps (16ms interval) continuous mode
- Zero-copy GPU texture binding (stable GL texture IDs)
- Outputs transparent RGBA (alpha=0 background for layer compositing)
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
# Cyberpunk Lower Third Processor (GENERATOR - outputs transparent RGBA)
# =============================================================================

@processor(
    name="CyberpunkLowerThird",
    description="Cyberpunk-style lower third RGBA overlay generator",
    execution="Continuous",
)
class CyberpunkLowerThird:
    """Generates animated lower third overlay as transparent RGBA texture.

    This is a GENERATOR processor - it outputs standalone RGBA textures
    with transparent backgrounds for compositing by BlendingCompositor.

    Features:
    - Slide-in animation with back easing (overshoot)
    - Cyberpunk color palette
    - Bitter font for authentic look
    - Scan lines
    - Zero-copy GPU rendering via Skia + IOSurface
    - Transparent background (alpha=0) for layer compositing
    """

    @output(schema="VideoFrame")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Initialize Skia and load fonts."""
        self.frame_count = 0
        self.frame_number = 0

        # Configuration
        self.headline = "NIGHT CITY NEWS"
        self.subtitle = "Live from Watson District"

        # Output dimensions
        self.width = DEFAULT_WIDTH
        self.height = DEFAULT_HEIGHT

        # Get GL context
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

        # Load Bitter font
        self.typeface = skia.Typeface.MakeFromName("Bitter", skia.FontStyle.Bold())
        if self.typeface is None:
            logger.warning("Bitter font not found, falling back to default")
            self.typeface = skia.Typeface.MakeDefault()

        logger.info(
            f"Cyberpunk Lower Third initialized as GENERATOR "
            f"({self.width}x{self.height}, font: {self.typeface.getFamilyName()})"
        )

    def process(self, ctx):
        """Generate lower third overlay frame with transparent background."""
        # Ensure GL context is current
        self.gl_ctx.make_current()

        # Get canvas from output surface
        canvas = self.output_skia_surface.getCanvas()

        # Clear to fully transparent (alpha=0) for layer compositing
        canvas.clear(skia.Color(0, 0, 0, 0))

        # Draw lower third overlay on transparent background
        elapsed = ctx.time.elapsed_secs
        draw_lower_third(
            canvas,
            self.width,
            self.height,
            self.headline,
            self.subtitle,
            elapsed,
            self.typeface,
        )

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
            logger.info(f"Lower Third: First frame generated ({self.width}x{self.height})")
        if self.frame_count % 60 == 0:
            # Purge unlocked GPU resources to prevent Skia memory accumulation
            self.skia_ctx.freeGpuResources()
        if self.frame_count % 300 == 0:
            logger.debug(f"Lower Third: generated {self.frame_count} frames")

    def teardown(self, ctx):
        """Cleanup."""
        if self.skia_ctx:
            self.skia_ctx.abandonContext()
        logger.info(f"Cyberpunk Lower Third shutdown ({self.frame_count} frames)")
