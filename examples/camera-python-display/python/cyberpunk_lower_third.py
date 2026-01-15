# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk-style lower third overlay processor - CONTINUOUS RGBA GENERATOR.

This processor generates lower third overlay frames independently,
outputting transparent RGBA textures for compositing by the BlendingCompositor.

Features:
- Slide-in animation with "back" easing (overshoot) for snappy Cyberpunk feel
- Cyberpunk 2077 yellow HUD style with angular chamfered corners
- Dark text on yellow background for readability
- Channel logo section (like "N54 NEWS")
- Cyan accent details and tech lines
- Runs at 60fps (16ms interval) continuous mode
- Zero-copy GPU texture binding (stable GL texture IDs)
- Outputs transparent RGBA (alpha=0 background for layer compositing)
- Static elements cached as Skia Picture after slide-in completes
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

# Animation timing
SLIDE_DURATION = 0.6  # seconds

# =============================================================================
# Cyberpunk Color Palette (Yellow HUD Theme)
# =============================================================================

# Primary Cyberpunk yellow - the signature color
CYBER_YELLOW = skia.Color(252, 238, 10, 255)       # #fcee0a - main panel color
CYBER_YELLOW_DARK = skia.Color(200, 180, 0, 255)   # Darker yellow for accents
CYBER_CYAN = skia.Color(0, 240, 255, 255)          # #00f0ff - accent color
CYBER_RED = skia.Color(255, 30, 60, 255)           # #ff1e3c - channel logo
CYBER_DARK = skia.Color(15, 15, 20, 255)           # Near black for text
CYBER_DARK_TRANS = skia.Color(15, 15, 20, 230)     # Semi-transparent dark

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
# Angular Shape Helpers
# =============================================================================

def create_angular_panel_path(x, y, width, height, cut_tl, cut_tr, cut_br, cut_bl):
    """Create a path for an angular panel with chamfered corners.

    Each cut parameter is the size of the 45-degree chamfer at that corner.
    TL=top-left, TR=top-right, BR=bottom-right, BL=bottom-left
    """
    path = skia.Path()

    # Start at top-left (after chamfer)
    path.moveTo(x + cut_tl, y)

    # Top edge to top-right chamfer
    path.lineTo(x + width - cut_tr, y)

    # Top-right chamfer
    if cut_tr > 0:
        path.lineTo(x + width, y + cut_tr)

    # Right edge to bottom-right chamfer
    path.lineTo(x + width, y + height - cut_br)

    # Bottom-right chamfer
    if cut_br > 0:
        path.lineTo(x + width - cut_br, y + height)

    # Bottom edge to bottom-left chamfer
    path.lineTo(x + cut_bl, y + height)

    # Bottom-left chamfer
    if cut_bl > 0:
        path.lineTo(x, y + height - cut_bl)

    # Left edge to top-left chamfer
    path.lineTo(x, y + cut_tl)

    # Close path (completes top-left chamfer)
    path.close()

    return path


# =============================================================================
# Lower Third Drawing - Static Elements (cached as Picture after slide-in)
# =============================================================================

def record_static_lower_third(
    width: int,
    height: int,
    headline: str,
    subtitle: str,
    channel: str,
    typeface: skia.Typeface,
    panel_x: float,
):
    """Record static lower third elements to a Skia Picture.

    Static elements: panel backgrounds, accent lines, text.
    These are recorded once slide-in completes (panel_x at final position).
    """
    # Lower third dimensions
    panel_height = height * 0.12
    panel_y = height - panel_height - (height * 0.06)
    panel_width = width * 0.50

    # Channel logo dimensions (left section)
    logo_width = width * 0.08
    logo_cut = 20  # Chamfer size

    # Main panel chamfers
    main_cut_left = 15
    main_cut_right = 30

    # Record to picture
    recorder = skia.PictureRecorder()
    bounds = skia.Rect.MakeWH(width, height)
    canvas = recorder.beginRecording(bounds)

    # === CHANNEL LOGO SECTION (Red angular box on left) ===
    logo_x = panel_x
    logo_path = create_angular_panel_path(
        logo_x, panel_y,
        logo_width, panel_height,
        cut_tl=logo_cut, cut_tr=0, cut_br=0, cut_bl=logo_cut
    )
    logo_paint = skia.Paint(
        Color=CYBER_RED,
        AntiAlias=True,
    )
    canvas.drawPath(logo_path, logo_paint)

    # Channel text (vertical or horizontal)
    channel_font = skia.Font(typeface, panel_height * 0.35)
    channel_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    channel_paint = skia.Paint(
        Color=skia.Color(255, 255, 255, 255),
        AntiAlias=True,
    )

    # Measure text to center it
    channel_bounds = skia.Rect()
    channel_font.measureText(channel, bounds=channel_bounds)
    channel_x = logo_x + (logo_width - channel_bounds.width()) / 2
    channel_y = panel_y + panel_height * 0.65
    canvas.drawString(channel, channel_x, channel_y, channel_font, channel_paint)

    # === MAIN YELLOW PANEL (Angular shape) ===
    main_x = logo_x + logo_width + 3  # Small gap after logo
    main_width = panel_width - logo_width - 3

    main_path = create_angular_panel_path(
        main_x, panel_y,
        main_width, panel_height,
        cut_tl=0, cut_tr=main_cut_right, cut_br=main_cut_right, cut_bl=0
    )
    main_paint = skia.Paint(
        Color=CYBER_YELLOW,
        AntiAlias=True,
    )
    canvas.drawPath(main_path, main_paint)

    # === CYAN ACCENT LINE (top edge of main panel) ===
    accent_path = skia.Path()
    accent_path.moveTo(main_x, panel_y)
    accent_path.lineTo(main_x + main_width - main_cut_right, panel_y)
    accent_path.lineTo(main_x + main_width - main_cut_right + 3, panel_y + 3)
    accent_path.lineTo(main_x, panel_y + 3)
    accent_path.close()

    accent_paint = skia.Paint(
        Color=CYBER_CYAN,
        AntiAlias=True,
    )
    canvas.drawPath(accent_path, accent_paint)

    # === HEADLINE TEXT (Dark on yellow) ===
    headline_size = panel_height * 0.45
    headline_font = skia.Font(typeface, headline_size)
    headline_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)

    text_x = main_x + 20
    text_y = panel_y + panel_height * 0.50

    headline_paint = skia.Paint(
        Color=CYBER_DARK,
        AntiAlias=True,
    )
    canvas.drawString(headline, text_x, text_y, headline_font, headline_paint)

    # === SUBTITLE TEXT (smaller, below headline) ===
    subtitle_size = panel_height * 0.28
    subtitle_font = skia.Font(typeface, subtitle_size)
    subtitle_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)

    subtitle_y = panel_y + panel_height * 0.82
    subtitle_paint = skia.Paint(
        Color=skia.Color(40, 40, 50, 255),  # Slightly lighter dark
        AntiAlias=True,
    )
    canvas.drawString(subtitle, text_x, subtitle_y, subtitle_font, subtitle_paint)

    # === TECH DETAIL: Corner bracket (top-right of main panel) ===
    bracket_paint = skia.Paint(
        Color=CYBER_DARK,
        AntiAlias=True,
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=2,
    )
    bracket_x = main_x + main_width - main_cut_right - 40
    bracket_y = panel_y + 8
    bracket_path = skia.Path()
    bracket_path.moveTo(bracket_x, bracket_y + 12)
    bracket_path.lineTo(bracket_x, bracket_y)
    bracket_path.lineTo(bracket_x + 12, bracket_y)
    canvas.drawPath(bracket_path, bracket_paint)

    # === TECH DETAIL: Small dots/indicators ===
    dot_paint = skia.Paint(
        Color=CYBER_DARK,
        AntiAlias=True,
    )
    for i in range(3):
        dot_x = main_x + main_width - 70 - (i * 12)
        dot_y = panel_y + panel_height - 12
        canvas.drawCircle(dot_x, dot_y, 3, dot_paint)

    # === SECONDARY INFO BAR (below main panel) ===
    info_bar_height = panel_height * 0.25
    info_bar_y = panel_y + panel_height + 2
    info_bar_width = main_width * 0.6

    info_path = create_angular_panel_path(
        main_x, info_bar_y,
        info_bar_width, info_bar_height,
        cut_tl=0, cut_tr=10, cut_br=10, cut_bl=0
    )
    info_paint = skia.Paint(
        Color=CYBER_DARK_TRANS,
        AntiAlias=True,
    )
    canvas.drawPath(info_path, info_paint)

    # Info bar text
    info_font = skia.Font(typeface, info_bar_height * 0.6)
    info_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    info_paint = skia.Paint(
        Color=CYBER_CYAN,
        AntiAlias=True,
    )
    canvas.drawString("LIVE // WATSON DISTRICT", main_x + 10, info_bar_y + info_bar_height * 0.72, info_font, info_paint)

    return recorder.finishRecordingAsPicture()


def draw_animated_lower_third(
    canvas: skia.Canvas,
    width: int,
    height: int,
    panel_x: float,
    elapsed: float,
):
    """Draw only the animated lower third elements.

    Animated elements: scan line (position varies), pulsing indicator.
    """
    panel_height = height * 0.12
    panel_y = height - panel_height - (height * 0.06)
    panel_width = width * 0.50
    logo_width = width * 0.08
    main_x = panel_x + logo_width + 3
    main_width = panel_width - logo_width - 3

    # === SCAN LINE (animated position across yellow panel) ===
    scan_line_x = main_x + ((elapsed * 200) % main_width)
    scan_paint = skia.Paint(
        Color=skia.Color(255, 255, 255, 80),
        AntiAlias=True,
    )
    scan_rect = skia.Rect.MakeXYWH(scan_line_x, panel_y + 5, 3, panel_height - 10)
    canvas.drawRect(scan_rect, scan_paint)

    # === PULSING INDICATOR (in channel logo area) ===
    pulse_alpha = int(150 + 105 * math.sin(elapsed * 4))
    pulse_paint = skia.Paint(
        Color=skia.Color(255, 255, 255, pulse_alpha),
        AntiAlias=True,
    )
    pulse_x = panel_x + logo_width - 15
    pulse_y = panel_y + 10
    canvas.drawCircle(pulse_x, pulse_y, 4, pulse_paint)

    # === ANIMATED TECH BARS (bottom right of main panel) ===
    bar_base_x = main_x + main_width - 100
    bar_y = panel_y + panel_height - 8

    for i in range(4):
        bar_alpha = max(0, min(255, int(100 + 155 * math.sin(elapsed * 3 + i * 0.8))))
        bar_width = 15 - (i * 2)
        bar_paint = skia.Paint(
            Color=skia.Color(15, 15, 20, bar_alpha),
            AntiAlias=True,
        )
        bar_x = bar_base_x + (i * 18)
        bar_rect = skia.Rect.MakeXYWH(bar_x, bar_y, bar_width, 4)
        canvas.drawRect(bar_rect, bar_paint)


def draw_lower_third_full(
    canvas: skia.Canvas,
    width: int,
    height: int,
    headline: str,
    subtitle: str,
    channel: str,
    elapsed: float,
    typeface: skia.Typeface,
):
    """Draw full lower third (used during slide-in animation)."""
    # Calculate animation progress
    if elapsed < SLIDE_DURATION:
        progress = ease_out_back(elapsed / SLIDE_DURATION, overshoot=2.0)
    else:
        progress = 1.0

    # Lower third dimensions
    panel_height = height * 0.12
    panel_y = height - panel_height - (height * 0.06)
    panel_width = width * 0.50

    # Channel logo dimensions
    logo_width = width * 0.08
    logo_cut = 20

    # Main panel chamfers
    main_cut_left = 15
    main_cut_right = 30

    # Slide from left
    panel_x = -panel_width + (panel_width + width * 0.05) * progress

    # === CHANNEL LOGO SECTION ===
    logo_x = panel_x
    logo_path = create_angular_panel_path(
        logo_x, panel_y,
        logo_width, panel_height,
        cut_tl=logo_cut, cut_tr=0, cut_br=0, cut_bl=logo_cut
    )
    logo_paint = skia.Paint(
        Color=CYBER_RED,
        AntiAlias=True,
    )
    canvas.drawPath(logo_path, logo_paint)

    # Channel text
    channel_font = skia.Font(typeface, panel_height * 0.35)
    channel_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    channel_paint = skia.Paint(
        Color=skia.Color(255, 255, 255, 255),
        AntiAlias=True,
    )
    channel_bounds = skia.Rect()
    channel_font.measureText(channel, bounds=channel_bounds)
    channel_x = logo_x + (logo_width - channel_bounds.width()) / 2
    channel_y = panel_y + panel_height * 0.65
    canvas.drawString(channel, channel_x, channel_y, channel_font, channel_paint)

    # === MAIN YELLOW PANEL ===
    main_x = logo_x + logo_width + 3
    main_width = panel_width - logo_width - 3

    main_path = create_angular_panel_path(
        main_x, panel_y,
        main_width, panel_height,
        cut_tl=0, cut_tr=main_cut_right, cut_br=main_cut_right, cut_bl=0
    )
    main_paint = skia.Paint(
        Color=CYBER_YELLOW,
        AntiAlias=True,
    )
    canvas.drawPath(main_path, main_paint)

    # === CYAN ACCENT LINE ===
    accent_path = skia.Path()
    accent_path.moveTo(main_x, panel_y)
    accent_path.lineTo(main_x + main_width - main_cut_right, panel_y)
    accent_path.lineTo(main_x + main_width - main_cut_right + 3, panel_y + 3)
    accent_path.lineTo(main_x, panel_y + 3)
    accent_path.close()

    accent_paint = skia.Paint(
        Color=CYBER_CYAN,
        AntiAlias=True,
    )
    canvas.drawPath(accent_path, accent_paint)

    # === HEADLINE TEXT ===
    headline_size = panel_height * 0.45
    headline_font = skia.Font(typeface, headline_size)
    headline_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)

    text_x = main_x + 20
    text_y = panel_y + panel_height * 0.50

    headline_paint = skia.Paint(
        Color=CYBER_DARK,
        AntiAlias=True,
    )
    canvas.drawString(headline, text_x, text_y, headline_font, headline_paint)

    # === SUBTITLE TEXT ===
    subtitle_size = panel_height * 0.28
    subtitle_font = skia.Font(typeface, subtitle_size)
    subtitle_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)

    subtitle_y = panel_y + panel_height * 0.82
    subtitle_paint = skia.Paint(
        Color=skia.Color(40, 40, 50, 255),
        AntiAlias=True,
    )
    canvas.drawString(subtitle, text_x, subtitle_y, subtitle_font, subtitle_paint)

    # === TECH DETAIL: Corner bracket ===
    bracket_paint = skia.Paint(
        Color=CYBER_DARK,
        AntiAlias=True,
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=2,
    )
    bracket_x = main_x + main_width - main_cut_right - 40
    bracket_y = panel_y + 8
    bracket_path = skia.Path()
    bracket_path.moveTo(bracket_x, bracket_y + 12)
    bracket_path.lineTo(bracket_x, bracket_y)
    bracket_path.lineTo(bracket_x + 12, bracket_y)
    canvas.drawPath(bracket_path, bracket_paint)

    # === TECH DETAIL: Small dots ===
    dot_paint = skia.Paint(
        Color=CYBER_DARK,
        AntiAlias=True,
    )
    for i in range(3):
        dot_x = main_x + main_width - 70 - (i * 12)
        dot_y = panel_y + panel_height - 12
        canvas.drawCircle(dot_x, dot_y, 3, dot_paint)

    # === SECONDARY INFO BAR ===
    info_bar_height = panel_height * 0.25
    info_bar_y = panel_y + panel_height + 2
    info_bar_width = main_width * 0.6

    info_path = create_angular_panel_path(
        main_x, info_bar_y,
        info_bar_width, info_bar_height,
        cut_tl=0, cut_tr=10, cut_br=10, cut_bl=0
    )
    info_paint_bg = skia.Paint(
        Color=CYBER_DARK_TRANS,
        AntiAlias=True,
    )
    canvas.drawPath(info_path, info_paint_bg)

    info_font = skia.Font(typeface, info_bar_height * 0.6)
    info_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    info_paint = skia.Paint(
        Color=CYBER_CYAN,
        AntiAlias=True,
    )
    canvas.drawString("LIVE // WATSON DISTRICT", main_x + 10, info_bar_y + info_bar_height * 0.72, info_font, info_paint)

    # === ANIMATED ELEMENTS (only after slide-in) ===
    if progress >= 1.0:
        # Scan line
        scan_line_x = main_x + ((elapsed * 200) % main_width)
        scan_paint = skia.Paint(
            Color=skia.Color(255, 255, 255, 80),
            AntiAlias=True,
        )
        scan_rect = skia.Rect.MakeXYWH(scan_line_x, panel_y + 5, 3, panel_height - 10)
        canvas.drawRect(scan_rect, scan_paint)

        # Pulsing indicator
        pulse_alpha = int(150 + 105 * math.sin(elapsed * 4))
        pulse_paint = skia.Paint(
            Color=skia.Color(255, 255, 255, pulse_alpha),
            AntiAlias=True,
        )
        pulse_x = panel_x + logo_width - 15
        pulse_y = panel_y + 10
        canvas.drawCircle(pulse_x, pulse_y, 4, pulse_paint)


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
    - Cyberpunk 2077 yellow HUD style
    - Angular chamfered corners
    - Channel logo section
    - Dark text on yellow background
    - Zero-copy GPU rendering via Skia + IOSurface
    - Transparent background (alpha=0) for layer compositing
    - Static elements cached as Skia Picture after slide-in completes
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
        self.subtitle = "Breaking developments in Watson"
        self.channel = "N54"

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

        # Load Bitter font (Cyberpunk style)
        self.typeface = skia.Typeface.MakeFromName("Bitter", skia.FontStyle.Bold())
        if self.typeface is None:
            logger.warning("Bitter font not found, falling back to default")
            self.typeface = skia.Typeface.MakeDefault()

        # Picture caching state - recorded once slide-in completes
        self.static_picture = None
        self.slide_in_complete = False

        # Pre-calculate final panel_x for when slide-in completes
        panel_width = self.width * 0.50
        self.final_panel_x = -panel_width + (panel_width + self.width * 0.05) * 1.0

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

        elapsed = ctx.time.elapsed_secs

        # Two-phase rendering:
        # 1. During slide-in (first 0.6s): draw everything each frame
        # 2. After slide-in: use cached picture + animated elements only
        if not self.slide_in_complete:
            if elapsed >= SLIDE_DURATION:
                # Slide-in just completed - record static picture
                self.static_picture = record_static_lower_third(
                    self.width,
                    self.height,
                    self.headline,
                    self.subtitle,
                    self.channel,
                    self.typeface,
                    self.final_panel_x,
                )
                self.slide_in_complete = True
                logger.info("Lower Third: Slide-in complete, static picture cached")

                # Draw using picture + animated
                canvas.drawPicture(self.static_picture)
                draw_animated_lower_third(
                    canvas, self.width, self.height, self.final_panel_x, elapsed
                )
            else:
                # Still animating - draw full each frame
                draw_lower_third_full(
                    canvas,
                    self.width,
                    self.height,
                    self.headline,
                    self.subtitle,
                    self.channel,
                    elapsed,
                    self.typeface,
                )
        else:
            # Slide-in complete - use cached picture + animated elements
            canvas.drawPicture(self.static_picture)
            draw_animated_lower_third(
                canvas, self.width, self.height, self.final_panel_x, elapsed
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
        if self.frame_count % 300 == 0:
            # Purge unlocked GPU resources to prevent Skia memory accumulation
            self.skia_ctx.freeGpuResources()
        if self.frame_count % 300 == 0:
            logger.debug(f"Lower Third: generated {self.frame_count} frames")

    def teardown(self, ctx):
        """Cleanup."""
        self.static_picture = None  # Release picture before context
        if self.skia_ctx:
            self.skia_ctx.abandonContext()
        logger.info(f"Cyberpunk Lower Third shutdown ({self.frame_count} frames)")
