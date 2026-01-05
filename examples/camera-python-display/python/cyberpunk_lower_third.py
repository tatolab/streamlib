# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk-style lower third overlay processor using Skia GPU rendering.

Features:
- Slide-in animation with "back" easing (overshoot) for snappy Cyberpunk feel
- Cyberpunk 2077 color palette (cyan, magenta, yellow accents)
- Bitter font (from Cyberpunk website)
- Glitch/scan line effects
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


def ease_in_out_cubic(t: float) -> float:
    """Smooth cubic ease in-out."""
    if t < 0.5:
        return 4 * t * t * t
    else:
        return 1 - pow(-2 * t + 2, 3) / 2


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
    hold_start = 1.0      # when to start showing full

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

    # Draw cut in dark color (creates angular look)
    cut_paint = skia.Paint(
        Color=skia.Color(10, 10, 15, 255),
        AntiAlias=True,
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
# Cyberpunk Lower Third Processor
# =============================================================================

@processor(name="CyberpunkLowerThird", description="Cyberpunk-style lower third overlay with Skia")
class CyberpunkLowerThird:
    """Renders animated lower third overlay in Cyberpunk 2077 style.

    Features:
    - Slide-in animation with back easing (overshoot)
    - Cyberpunk color palette
    - Bitter font for authentic look
    - Glitch effects and scan lines
    - Zero-copy GPU rendering via Skia + IOSurface
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

        # Load Bitter font
        self.typeface = skia.Typeface.MakeFromName("Bitter", skia.FontStyle.Bold())
        if self.typeface is None:
            logger.warning("Bitter font not found, falling back to default")
            self.typeface = skia.Typeface.MakeDefault()

        # Surface cache
        self._surface_cache = {}

        logger.info(f"Cyberpunk Lower Third initialized (font: {self.typeface.getFamilyName()})")

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
        """Render lower third overlay on each frame."""
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

        # Draw lower third overlay
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
            logger.debug(f"Processed {self.frame_count} frames")

    def teardown(self, ctx):
        """Cleanup."""
        self._surface_cache.clear()
        if self.skia_ctx:
            self.skia_ctx.abandonContext()
        logger.info(f"Cyberpunk Lower Third shutdown ({self.frame_count} frames)")
