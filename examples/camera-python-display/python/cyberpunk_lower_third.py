# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk-style lower third overlay processor — continuous RGBA generator.

Linux subprocess processor on the canonical surface-adapter pattern
(#485): the host pre-allocates a render-target-capable DMA-BUF
``VkImage`` and registers it in surface-share + ``texture_cache`` under
a fixed UUID; this processor opens that UUID via
:meth:`streamlib.adapters.skia.SkiaContext.acquire_write` every tick
and draws into the yielded ``skia.Surface``. Skia composes on the
OpenGL adapter through ``skia.GrDirectContext.MakeGL(MakeEGL())`` —
see ``libs/streamlib-python/python/streamlib/adapters/skia.py`` for
the full rationale.

Features:
- Slide-in animation with "back" easing (overshoot) for snappy Cyberpunk feel
- Cyberpunk 2077 yellow HUD style with angular chamfered corners
- Dark text on yellow background for readability
- Channel logo section (like "N54 NEWS")
- Cyan accent details and tech lines
- Runs at 60fps (16ms interval) continuous mode
- Zero-copy GPU texture via DMA-BUF VkImage + EGL DMA-BUF import
- Outputs transparent RGBA (alpha=0 background for layer compositing)
- Static elements cached as Skia Picture after slide-in completes

macOS support was removed in #485; the pre-RHI CGL+IOSurface path
predated the surface-adapter pattern.

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

# Animation timing
SLIDE_DURATION = 0.6  # seconds

# =============================================================================
# Cyberpunk Color Palette (Yellow HUD Theme)
# =============================================================================

# Primary Cyberpunk yellow — the signature color.
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
    """Back easing — overshoots then settles. Classic Cyberpunk UI feel."""
    t = t - 1
    return t * t * ((overshoot + 1) * t + overshoot) + 1


def ease_out_expo(t: float) -> float:
    """Exponential ease out — fast start, slow end."""
    return 1 if t == 1 else 1 - pow(2, -10 * t)


# =============================================================================
# Angular Shape Helpers
# =============================================================================

def create_angular_panel_path(x, y, width, height, cut_tl, cut_tr, cut_br, cut_bl):
    """Path for an angular panel with chamfered corners.

    Each cut parameter is the size of the 45-degree chamfer at that
    corner. TL=top-left, TR=top-right, BR=bottom-right, BL=bottom-left.
    """
    path = skia.Path()
    path.moveTo(x + cut_tl, y)
    path.lineTo(x + width - cut_tr, y)
    if cut_tr > 0:
        path.lineTo(x + width, y + cut_tr)
    path.lineTo(x + width, y + height - cut_br)
    if cut_br > 0:
        path.lineTo(x + width - cut_br, y + height)
    path.lineTo(x + cut_bl, y + height)
    if cut_bl > 0:
        path.lineTo(x, y + height - cut_bl)
    path.lineTo(x, y + cut_tl)
    path.close()
    return path


# =============================================================================
# Lower Third Drawing — Static Elements (cached as Skia Picture)
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
    """Record static lower-third elements to a `skia.Picture` for fast replay.

    Static elements: panel backgrounds, accent lines, text. Recorded
    once slide-in completes (panel_x at final position).
    """
    panel_height = height * 0.12
    panel_y = height - panel_height - (height * 0.06)
    panel_width = width * 0.50

    logo_width = width * 0.08
    logo_cut = 20

    main_cut_left = 15
    main_cut_right = 30

    recorder = skia.PictureRecorder()
    bounds = skia.Rect.MakeWH(width, height)
    canvas = recorder.beginRecording(bounds)

    # === CHANNEL LOGO SECTION (Red angular box on left) ===
    logo_x = panel_x
    logo_path = create_angular_panel_path(
        logo_x, panel_y,
        logo_width, panel_height,
        cut_tl=logo_cut, cut_tr=0, cut_br=0, cut_bl=logo_cut,
    )
    canvas.drawPath(logo_path, skia.Paint(Color=CYBER_RED, AntiAlias=True))

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

    # === MAIN YELLOW PANEL (Angular shape) ===
    main_x = logo_x + logo_width + 3
    main_width = panel_width - logo_width - 3
    main_path = create_angular_panel_path(
        main_x, panel_y,
        main_width, panel_height,
        cut_tl=0, cut_tr=main_cut_right, cut_br=main_cut_right, cut_bl=0,
    )
    canvas.drawPath(main_path, skia.Paint(Color=CYBER_YELLOW, AntiAlias=True))

    # === CYAN ACCENT LINE (top edge) ===
    accent_path = skia.Path()
    accent_path.moveTo(main_x, panel_y)
    accent_path.lineTo(main_x + main_width - main_cut_right, panel_y)
    accent_path.lineTo(main_x + main_width - main_cut_right + 3, panel_y + 3)
    accent_path.lineTo(main_x, panel_y + 3)
    accent_path.close()
    canvas.drawPath(accent_path, skia.Paint(Color=CYBER_CYAN, AntiAlias=True))

    # === HEADLINE TEXT (Dark on yellow) ===
    headline_font = skia.Font(typeface, panel_height * 0.45)
    headline_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    text_x = main_x + 20
    text_y = panel_y + panel_height * 0.50
    canvas.drawString(
        headline, text_x, text_y, headline_font,
        skia.Paint(Color=CYBER_DARK, AntiAlias=True),
    )

    # === SUBTITLE TEXT (smaller, below headline) ===
    subtitle_font = skia.Font(typeface, panel_height * 0.28)
    subtitle_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    subtitle_y = panel_y + panel_height * 0.82
    canvas.drawString(
        subtitle, text_x, subtitle_y, subtitle_font,
        skia.Paint(Color=skia.Color(40, 40, 50, 255), AntiAlias=True),
    )

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
    dot_paint = skia.Paint(Color=CYBER_DARK, AntiAlias=True)
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
        cut_tl=0, cut_tr=10, cut_br=10, cut_bl=0,
    )
    canvas.drawPath(info_path, skia.Paint(Color=CYBER_DARK_TRANS, AntiAlias=True))
    info_font = skia.Font(typeface, info_bar_height * 0.6)
    info_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    canvas.drawString(
        "LIVE // WATSON DISTRICT",
        main_x + 10,
        info_bar_y + info_bar_height * 0.72,
        info_font,
        skia.Paint(Color=CYBER_CYAN, AntiAlias=True),
    )

    return recorder.finishRecordingAsPicture()


def draw_animated_lower_third(
    canvas: skia.Canvas,
    width: int,
    height: int,
    panel_x: float,
    elapsed: float,
):
    """Draw only the animated elements (scan line, pulsing indicator,
    tech bars). Run on every frame after slide-in completes."""
    panel_height = height * 0.12
    panel_y = height - panel_height - (height * 0.06)
    panel_width = width * 0.50
    logo_width = width * 0.08
    main_x = panel_x + logo_width + 3
    main_width = panel_width - logo_width - 3

    # === SCAN LINE (animated position across yellow panel) ===
    scan_line_x = main_x + ((elapsed * 200) % main_width)
    scan_paint = skia.Paint(Color=skia.Color(255, 255, 255, 80), AntiAlias=True)
    canvas.drawRect(
        skia.Rect.MakeXYWH(scan_line_x, panel_y + 5, 3, panel_height - 10),
        scan_paint,
    )

    # === PULSING INDICATOR (in channel logo area) ===
    pulse_alpha = int(150 + 105 * math.sin(elapsed * 4))
    pulse_paint = skia.Paint(
        Color=skia.Color(255, 255, 255, pulse_alpha),
        AntiAlias=True,
    )
    pulse_x = panel_x + logo_width - 15
    pulse_y = panel_y + 10
    canvas.drawCircle(pulse_x, pulse_y, 4, pulse_paint)

    # === ANIMATED TECH BARS (bottom-right of main panel) ===
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
        canvas.drawRect(
            skia.Rect.MakeXYWH(bar_x, bar_y, bar_width, 4),
            bar_paint,
        )


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
    """Full lower-third draw — used during slide-in animation when the
    static-Picture cache hasn't been recorded yet."""
    if elapsed < SLIDE_DURATION:
        progress = ease_out_back(elapsed / SLIDE_DURATION, overshoot=2.0)
    else:
        progress = 1.0

    panel_height = height * 0.12
    panel_y = height - panel_height - (height * 0.06)
    panel_width = width * 0.50

    logo_width = width * 0.08
    logo_cut = 20
    main_cut_left = 15
    main_cut_right = 30

    panel_x = -panel_width + (panel_width + width * 0.05) * progress

    logo_x = panel_x
    logo_path = create_angular_panel_path(
        logo_x, panel_y,
        logo_width, panel_height,
        cut_tl=logo_cut, cut_tr=0, cut_br=0, cut_bl=logo_cut,
    )
    canvas.drawPath(logo_path, skia.Paint(Color=CYBER_RED, AntiAlias=True))

    channel_font = skia.Font(typeface, panel_height * 0.35)
    channel_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    channel_paint = skia.Paint(Color=skia.Color(255, 255, 255, 255), AntiAlias=True)
    channel_bounds = skia.Rect()
    channel_font.measureText(channel, bounds=channel_bounds)
    channel_x = logo_x + (logo_width - channel_bounds.width()) / 2
    channel_y = panel_y + panel_height * 0.65
    canvas.drawString(channel, channel_x, channel_y, channel_font, channel_paint)

    main_x = logo_x + logo_width + 3
    main_width = panel_width - logo_width - 3
    main_path = create_angular_panel_path(
        main_x, panel_y,
        main_width, panel_height,
        cut_tl=0, cut_tr=main_cut_right, cut_br=main_cut_right, cut_bl=0,
    )
    canvas.drawPath(main_path, skia.Paint(Color=CYBER_YELLOW, AntiAlias=True))

    accent_path = skia.Path()
    accent_path.moveTo(main_x, panel_y)
    accent_path.lineTo(main_x + main_width - main_cut_right, panel_y)
    accent_path.lineTo(main_x + main_width - main_cut_right + 3, panel_y + 3)
    accent_path.lineTo(main_x, panel_y + 3)
    accent_path.close()
    canvas.drawPath(accent_path, skia.Paint(Color=CYBER_CYAN, AntiAlias=True))

    headline_font = skia.Font(typeface, panel_height * 0.45)
    headline_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    text_x = main_x + 20
    text_y = panel_y + panel_height * 0.50
    canvas.drawString(
        headline, text_x, text_y, headline_font,
        skia.Paint(Color=CYBER_DARK, AntiAlias=True),
    )

    subtitle_font = skia.Font(typeface, panel_height * 0.28)
    subtitle_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    subtitle_y = panel_y + panel_height * 0.82
    canvas.drawString(
        subtitle, text_x, subtitle_y, subtitle_font,
        skia.Paint(Color=skia.Color(40, 40, 50, 255), AntiAlias=True),
    )

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

    dot_paint = skia.Paint(Color=CYBER_DARK, AntiAlias=True)
    for i in range(3):
        dot_x = main_x + main_width - 70 - (i * 12)
        dot_y = panel_y + panel_height - 12
        canvas.drawCircle(dot_x, dot_y, 3, dot_paint)

    info_bar_height = panel_height * 0.25
    info_bar_y = panel_y + panel_height + 2
    info_bar_width = main_width * 0.6
    info_path = create_angular_panel_path(
        main_x, info_bar_y,
        info_bar_width, info_bar_height,
        cut_tl=0, cut_tr=10, cut_br=10, cut_bl=0,
    )
    canvas.drawPath(info_path, skia.Paint(Color=CYBER_DARK_TRANS, AntiAlias=True))

    info_font = skia.Font(typeface, info_bar_height * 0.6)
    info_font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    canvas.drawString(
        "LIVE // WATSON DISTRICT",
        main_x + 10,
        info_bar_y + info_bar_height * 0.72,
        info_font,
        skia.Paint(Color=CYBER_CYAN, AntiAlias=True),
    )

    if progress >= 1.0:
        scan_line_x = main_x + ((elapsed * 200) % main_width)
        canvas.drawRect(
            skia.Rect.MakeXYWH(scan_line_x, panel_y + 5, 3, panel_height - 10),
            skia.Paint(Color=skia.Color(255, 255, 255, 80), AntiAlias=True),
        )
        pulse_alpha = int(150 + 105 * math.sin(elapsed * 4))
        canvas.drawCircle(
            panel_x + logo_width - 15,
            panel_y + 10,
            4,
            skia.Paint(
                Color=skia.Color(255, 255, 255, pulse_alpha),
                AntiAlias=True,
            ),
        )


# =============================================================================
# Cyberpunk Lower Third Processor
# =============================================================================

class CyberpunkLowerThird:
    """Continuous RGBA generator drawing a Cyberpunk lower-third overlay
    into a host-pre-registered DMA-BUF render-target via Skia-on-GL.

    Output frames carry the host's surface UUID; the BlendingCompositor
    consumes them as `lower_third_in` and alpha-composites onto the
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

        # Hardcoded content (kept in code rather than config — content
        # is part of the example's "look", not a pipeline knob).
        self.headline = "NIGHT CITY NEWS"
        self.subtitle = "Breaking developments in Watson"
        self.channel = "N54"

        # Bitter font is the cyberpunk identity; default fallback if missing.
        self.typeface = skia.Typeface.MakeFromName("Bitter", skia.FontStyle.Bold())
        if self.typeface is None:
            logger.warning("Bitter font not found, falling back to default")
            self.typeface = skia.Typeface.MakeDefault()

        # Picture-cache state — recorded once slide-in completes.
        self.static_picture = None
        self.slide_in_complete = False
        panel_width = self.width * 0.50
        self.final_panel_x = -panel_width + (panel_width + self.width * 0.05) * 1.0

        # In-process consumer (BlendingCompositor) — match
        # AvatarCharacter's pattern: SkiaContext.acquire_write only,
        # no producer-side QFOT release. glFinish on the inner OpenGL
        # release plus DMA-BUF kernel-fence semantics carry visibility;
        # the consumer's first Vulkan barrier transitions the registered
        # layout to SHADER_READ_ONLY_OPTIMAL.
        self._skia_ctx = SkiaContext.from_runtime(ctx)
        self._surface = StreamlibSurface(
            id=self._uuid,
            width=self.width,
            height=self.height,
            format=int(SurfaceFormat.BGRA8),
            usage=int(SurfaceUsage.RENDER_TARGET | SurfaceUsage.SAMPLED),
        )

        logger.info(
            f"Cyberpunk Lower Third initialized as GENERATOR "
            f"({self.width}x{self.height}, uuid={self._uuid}, "
            f"font: {self.typeface.getFamilyName()})"
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        elapsed = time.monotonic() - self._start_time

        try:
            with self._skia_ctx.acquire_write(self._surface) as guard:
                canvas = guard.surface.getCanvas()
                # Clear to fully transparent (alpha=0) for layer compositing.
                canvas.clear(skia.Color(0, 0, 0, 0))

                if not self.slide_in_complete:
                    if elapsed >= SLIDE_DURATION:
                        # Slide-in just completed — record + replay static.
                        self.static_picture = record_static_lower_third(
                            self.width, self.height,
                            self.headline, self.subtitle, self.channel,
                            self.typeface, self.final_panel_x,
                        )
                        self.slide_in_complete = True
                        logger.info(
                            "Lower Third: slide-in complete, static picture cached"
                        )
                        canvas.drawPicture(self.static_picture)
                        draw_animated_lower_third(
                            canvas, self.width, self.height,
                            self.final_panel_x, elapsed,
                        )
                    else:
                        # Still animating — full draw each frame.
                        draw_lower_third_full(
                            canvas, self.width, self.height,
                            self.headline, self.subtitle, self.channel,
                            elapsed, self.typeface,
                        )
                else:
                    # Steady state — replay cached Picture + animated overlay.
                    canvas.drawPicture(self.static_picture)
                    draw_animated_lower_third(
                        canvas, self.width, self.height,
                        self.final_panel_x, elapsed,
                    )
        except Exception as e:
            if self.frame_count % 60 == 0:
                logger.warning(
                    f"Cyberpunk Lower Third: acquire_write / draw failed: {e}"
                )
            return

        # Output VideoFrame referencing the pre-registered output UUID.
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
                f"Cyberpunk Lower Third: First frame generated "
                f"({self.width}x{self.height})"
            )
        if self.frame_count % 300 == 0:
            logger.debug(f"Cyberpunk Lower Third: generated {self.frame_count} frames")

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        # Release the cached Picture before SkiaContext goes away (the
        # Picture references shared GPU resources via the adapter's
        # DirectContext).
        self.static_picture = None
        logger.info(f"Cyberpunk Lower Third shutdown ({self.frame_count} frames)")
