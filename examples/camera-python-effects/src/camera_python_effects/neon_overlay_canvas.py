# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The broadcast overlay, drawn with skia onto a transparent canvas.

Cyberpunk 2077 UI language throughout: signature yellow with black glyphs,
cyan accents, angular 45° chamfers instead of rounded corners, and sparse
"tech" detailing — brackets, dots, flickering hex readouts, a scan line
sweeping the panel. Four clusters: the N54 lower third (bottom-left), a
scrolling news ticker (bottom edge), a REC/status HUD (top-left, clear of the
picture-in-picture), and the circuit-trace CL tag (bottom-right).

Kept clear of streamlib on purpose: this is ordinary 2D drawing code with a
canvas and a clock, so it runs — and is tested — without a GPU, an engine, or a
graph. `processors/neon_overlay_source.py` is the part that knows where the
pixels go.
"""

from __future__ import annotations

import math

import skia

__all__ = [
    "OVERLAY_ALPHA_TYPE",
    "OVERLAY_COLOR_TYPE",
    "SLIDE_IN_SECONDS",
    "draw_neon_overlay",
    "ease_out_back",
]

# Premultiplied RGBA8, which is both what the compositor's source-over blend
# expects and the byte order of the `rgba8_unorm` texture this lands in.
OVERLAY_COLOR_TYPE = skia.kRGBA_8888_ColorType
OVERLAY_ALPHA_TYPE = skia.kPremul_AlphaType

SLIDE_IN_SECONDS = 0.6

CYBER_YELLOW = skia.Color(252, 238, 10, 255)
CYBER_CYAN = skia.Color(0, 240, 255, 255)
CYBER_RED = skia.Color(255, 30, 60, 255)
CYBER_DARK = skia.Color(15, 15, 20, 255)
CYBER_DARK_TRANS = skia.Color(15, 15, 20, 230)

CHANNEL_NAME = "N54"
HEADLINE = "CYBERPUNK PIPELINE"
SUBHEADLINE = "SIX HELPER PROCESSES // GLSL KERNELS // ZERO COPIES"
INFO_BAR_TEXT = "LIVE // WATSON DISTRICT"
TICKER_TEXT = (
    "N54 BREAKING //// STREAMLIB NODE ONLINE //// SIX HELPER INTERPRETERS "
    "NOMINAL //// GPU KERNEL CHAIN ACTIVE //// POSE SCAN RUNNING //// "
    "SIGNAL SOURCE: CAM_01 //// NIGHT CITY FEED STABLE //// "
)
HUD_STATUS_TEXT = "CAM_01 // 1920x1080 // NV12"

TICKER_SCROLL_PIXELS_PER_SECOND = 130.0


def ease_out_back(progress: float, overshoot: float = 1.70158) -> float:
    """Overshoots past the target, then settles — the snappy HUD feel."""
    shifted = progress - 1.0
    return shifted * shifted * ((overshoot + 1.0) * shifted + overshoot) + 1.0


def _flicker_byte(time_step: int, lane: int) -> int:
    """A deterministic pseudo-random byte for the flickering hex readouts.

    Derived from time rather than `random` so two draws at one timestamp are
    one picture — which is also what makes the overlay testable.
    """
    return (time_step * 2654435761 + lane * 40503) % 251


def _overlay_font(size_in_pixels: float, *, bold: bool) -> skia.Font:
    """A font at `size_in_pixels`, from whatever the host actually has.

    Named families are asked for and not insisted on: skia falls back to the
    default typeface when the host has no match, which is the difference
    between an overlay that renders everywhere and one that renders here.
    """
    style = skia.FontStyle.Bold() if bold else skia.FontStyle.Normal()
    typeface = skia.Typeface.MakeFromName("DejaVu Sans", style)
    font = skia.Font(typeface, size_in_pixels)
    font.setEdging(skia.Font.Edging.kSubpixelAntiAlias)
    return font


def _chamfered_panel(
    left: float,
    top: float,
    width: float,
    height: float,
    cut_top_left: float = 0.0,
    cut_top_right: float = 0.0,
    cut_bottom_right: float = 0.0,
    cut_bottom_left: float = 0.0,
) -> skia.Path:
    """An angular panel with 45° chamfers at the named corners."""
    right = left + width
    bottom = top + height
    path = skia.Path()
    path.moveTo(left + cut_top_left, top)
    path.lineTo(right - cut_top_right, top)
    if cut_top_right > 0:
        path.lineTo(right, top + cut_top_right)
    path.lineTo(right, bottom - cut_bottom_right)
    if cut_bottom_right > 0:
        path.lineTo(right - cut_bottom_right, bottom)
    path.lineTo(left + cut_bottom_left, bottom)
    if cut_bottom_left > 0:
        path.lineTo(left, bottom - cut_bottom_left)
    path.lineTo(left, top + cut_top_left)
    path.close()
    return path


def _draw_lower_third(
    canvas: skia.Canvas,
    width: int,
    height: int,
    scale: float,
    slide_progress: float,
    elapsed_seconds: float,
) -> None:
    panel_height = 112.0 * scale
    panel_width = width * 0.50
    ticker_clearance = 34.0 * scale
    info_bar_height = panel_height * 0.26
    panel_y = height - ticker_clearance - info_bar_height - 6.0 * scale - panel_height

    logo_width = width * 0.075
    logo_cut = 18.0 * scale
    main_cut = 28.0 * scale

    # Off-screen left at 0, docked at 1; overshoot past 1 swings it through.
    panel_x = -panel_width + (panel_width + width * 0.024) * slide_progress

    # Channel logo box — red, chamfered on its outer corners.
    canvas.drawPath(
        _chamfered_panel(
            panel_x, panel_y, logo_width, panel_height,
            cut_top_left=logo_cut, cut_bottom_left=logo_cut,
        ),
        skia.Paint(AntiAlias=True, Color=CYBER_RED),
    )
    channel_font = _overlay_font(panel_height * 0.38, bold=True)
    channel_bounds = skia.Rect()
    channel_font.measureText(CHANNEL_NAME, bounds=channel_bounds)
    canvas.drawString(
        CHANNEL_NAME,
        panel_x + (logo_width - channel_bounds.width()) / 2.0,
        panel_y + panel_height * 0.62,
        channel_font,
        skia.Paint(AntiAlias=True, Color=skia.ColorWHITE),
    )
    # Pulsing indicator in the logo box.
    pulse_alpha = int(150 + 105 * math.sin(elapsed_seconds * 4.0))
    canvas.drawCircle(
        panel_x + logo_width - 14.0 * scale,
        panel_y + 12.0 * scale,
        4.0 * scale,
        skia.Paint(AntiAlias=True, Color=skia.Color(255, 255, 255, pulse_alpha)),
    )

    # Main yellow panel, chamfered away from the logo.
    main_x = panel_x + logo_width + 3.0 * scale
    main_width = panel_width - logo_width - 3.0 * scale
    canvas.drawPath(
        _chamfered_panel(
            main_x, panel_y, main_width, panel_height,
            cut_top_right=main_cut, cut_bottom_right=main_cut,
        ),
        skia.Paint(AntiAlias=True, Color=CYBER_YELLOW),
    )

    # Cyan accent strip along the top edge.
    accent = skia.Path()
    accent.moveTo(main_x, panel_y)
    accent.lineTo(main_x + main_width - main_cut, panel_y)
    accent.lineTo(main_x + main_width - main_cut + 3.0 * scale, panel_y + 3.0 * scale)
    accent.lineTo(main_x, panel_y + 3.0 * scale)
    accent.close()
    canvas.drawPath(accent, skia.Paint(AntiAlias=True, Color=CYBER_CYAN))

    text_x = main_x + 20.0 * scale
    canvas.drawString(
        HEADLINE,
        text_x,
        panel_y + panel_height * 0.48,
        _overlay_font(panel_height * 0.40, bold=True),
        skia.Paint(AntiAlias=True, Color=CYBER_DARK),
    )
    canvas.drawString(
        SUBHEADLINE,
        text_x,
        panel_y + panel_height * 0.80,
        _overlay_font(panel_height * 0.22, bold=False),
        skia.Paint(AntiAlias=True, Color=skia.Color(40, 40, 50, 255)),
    )

    # Tech detailing: corner bracket, dots, animated bars — dark on yellow.
    bracket = skia.Path()
    bracket_x = main_x + main_width - main_cut - 40.0 * scale
    bracket_y = panel_y + 8.0 * scale
    bracket.moveTo(bracket_x, bracket_y + 12.0 * scale)
    bracket.lineTo(bracket_x, bracket_y)
    bracket.lineTo(bracket_x + 12.0 * scale, bracket_y)
    canvas.drawPath(
        bracket,
        skia.Paint(
            AntiAlias=True, Color=CYBER_DARK,
            Style=skia.Paint.kStroke_Style, StrokeWidth=2.0 * scale,
        ),
    )
    dot_paint = skia.Paint(AntiAlias=True, Color=CYBER_DARK)
    for dot in range(3):
        canvas.drawCircle(
            main_x + main_width - (70.0 + dot * 12.0) * scale,
            panel_y + panel_height - 12.0 * scale,
            3.0 * scale,
            dot_paint,
        )
    for bar in range(4):
        bar_alpha = max(0, min(255, int(100 + 155 * math.sin(elapsed_seconds * 3.0 + bar * 0.8))))
        canvas.drawRect(
            skia.Rect.MakeXYWH(
                main_x + main_width - (100.0 - bar * 18.0) * scale,
                panel_y + panel_height - 8.0 * scale,
                (15.0 - bar * 2.0) * scale,
                4.0 * scale,
            ),
            skia.Paint(AntiAlias=True, Color=skia.Color(15, 15, 20, bar_alpha)),
        )

    # Scan line sweeping the yellow panel.
    scan_x = main_x + ((elapsed_seconds * 200.0 * scale) % main_width)
    canvas.drawRect(
        skia.Rect.MakeXYWH(scan_x, panel_y + 5.0 * scale, 3.0 * scale, panel_height - 10.0 * scale),
        skia.Paint(AntiAlias=True, Color=skia.Color(255, 255, 255, 80)),
    )

    # Secondary info bar below the panel.
    info_y = panel_y + panel_height + 2.0 * scale
    canvas.drawPath(
        _chamfered_panel(
            main_x, info_y, main_width * 0.6, info_bar_height,
            cut_top_right=10.0 * scale, cut_bottom_right=10.0 * scale,
        ),
        skia.Paint(AntiAlias=True, Color=CYBER_DARK_TRANS),
    )
    # The LIVE dot blinks; the district does not.
    live_dot_on = (elapsed_seconds % 1.2) < 0.8
    canvas.drawCircle(
        main_x + 12.0 * scale,
        info_y + info_bar_height * 0.52,
        4.0 * scale,
        skia.Paint(AntiAlias=True, Color=CYBER_RED if live_dot_on else skia.Color(90, 20, 30, 255)),
    )
    canvas.drawString(
        INFO_BAR_TEXT,
        main_x + 24.0 * scale,
        info_y + info_bar_height * 0.72,
        _overlay_font(info_bar_height * 0.58, bold=False),
        skia.Paint(AntiAlias=True, Color=CYBER_CYAN),
    )


def _draw_bottom_ticker(
    canvas: skia.Canvas, width: int, height: int, scale: float, elapsed_seconds: float
) -> None:
    strip_height = 28.0 * scale
    strip_top = height - strip_height
    canvas.drawRect(
        skia.Rect.MakeXYWH(0, strip_top, width, strip_height),
        skia.Paint(Color=skia.Color(10, 10, 14, 235)),
    )
    canvas.drawRect(
        skia.Rect.MakeXYWH(0, strip_top, width, 2.0 * scale),
        skia.Paint(Color=skia.Color(252, 238, 10, 160)),
    )

    ticker_font = _overlay_font(strip_height * 0.55, bold=True)
    ticker_width = ticker_font.measureText(TICKER_TEXT)
    # Two copies chase each other so the loop never shows a gap.
    scrolled = (elapsed_seconds * TICKER_SCROLL_PIXELS_PER_SECOND * scale) % ticker_width
    text_paint = skia.Paint(AntiAlias=True, Color=CYBER_YELLOW)
    baseline = strip_top + strip_height * 0.72
    canvas.drawString(TICKER_TEXT, width * 0.0 - scrolled, baseline, ticker_font, text_paint)
    canvas.drawString(TICKER_TEXT, ticker_width - scrolled, baseline, ticker_font, text_paint)


def _draw_top_left_hud(
    canvas: skia.Canvas, width: int, height: int, scale: float, elapsed_seconds: float
) -> None:
    origin_x = 26.0 * scale
    origin_y = 30.0 * scale

    # Thin cyan corner bracket framing the cluster.
    bracket = skia.Path()
    bracket.moveTo(origin_x - 8.0 * scale, origin_y + 30.0 * scale)
    bracket.lineTo(origin_x - 8.0 * scale, origin_y - 12.0 * scale)
    bracket.lineTo(origin_x + 34.0 * scale, origin_y - 12.0 * scale)
    canvas.drawPath(
        bracket,
        skia.Paint(
            AntiAlias=True, Color=CYBER_CYAN,
            Style=skia.Paint.kStroke_Style, StrokeWidth=2.0 * scale,
        ),
    )

    # REC with a blinking dot — a hard blink, not a fade.
    rec_on = (elapsed_seconds % 1.0) < 0.62
    canvas.drawCircle(
        origin_x + 7.0 * scale,
        origin_y + 3.0 * scale,
        6.0 * scale,
        skia.Paint(AntiAlias=True, Color=CYBER_RED if rec_on else skia.Color(80, 16, 26, 255)),
    )
    canvas.drawString(
        "REC",
        origin_x + 20.0 * scale,
        origin_y + 10.0 * scale,
        _overlay_font(22.0 * scale, bold=True),
        skia.Paint(AntiAlias=True, Color=skia.ColorWHITE),
    )
    canvas.drawString(
        HUD_STATUS_TEXT,
        origin_x - 2.0 * scale,
        origin_y + 34.0 * scale,
        _overlay_font(15.0 * scale, bold=False),
        skia.Paint(AntiAlias=True, Color=CYBER_CYAN),
    )

    # A row of hex readouts that re-randomize a few times a second — the
    # aimless diagnostics chatter every 2077 screen carries.
    time_step = int(elapsed_seconds * 2.5)
    readout = " ".join(f"{_flicker_byte(time_step, lane):02X}" for lane in range(6))
    canvas.drawString(
        readout,
        origin_x - 2.0 * scale,
        origin_y + 54.0 * scale,
        _overlay_font(13.0 * scale, bold=False),
        skia.Paint(AntiAlias=True, Color=skia.Color(0, 240, 255, 150)),
    )


def _circuit_trace_tag_path() -> skia.Path:
    """The stylized CL glyph — angular, circuit-trace inspired, in tag units."""
    path = skia.Path()
    path.moveTo(40, 10)
    path.lineTo(15, 10)
    path.lineTo(10, 15)
    path.lineTo(10, 45)
    path.lineTo(15, 50)
    path.lineTo(40, 50)
    path.moveTo(50, 10)
    path.lineTo(50, 45)
    path.lineTo(55, 50)
    path.lineTo(80, 50)
    path.moveTo(42, 30)
    path.lineTo(48, 30)
    return path


def _draw_circuit_watermark(
    canvas: skia.Canvas, width: int, height: int, scale: float, elapsed_seconds: float
) -> None:
    tag_scale = 1.5 * scale
    canvas.save()
    canvas.translate(width - 160.0 * scale, height - 190.0 * scale)
    canvas.scale(tag_scale, tag_scale)

    tag = _circuit_trace_tag_path()
    canvas.drawPath(
        tag,
        skia.Paint(
            AntiAlias=True,
            Color=skia.Color(0, 255, 255, 80),
            Style=skia.Paint.kStroke_Style,
            StrokeWidth=6.0,
            MaskFilter=skia.MaskFilter.MakeBlur(skia.kNormal_BlurStyle, 10.0),
        ),
    )
    canvas.drawPath(
        tag,
        skia.Paint(
            AntiAlias=True,
            Color=skia.Color(0, 255, 255, 255),
            Style=skia.Paint.kStroke_Style,
            StrokeWidth=4.0,
            StrokeCap=skia.Paint.kRound_Cap,
            StrokeJoin=skia.Paint.kRound_Join,
        ),
    )

    # Spray drips with breathing lengths.
    drip_paint = skia.Paint(
        AntiAlias=True,
        Color=skia.Color(0, 255, 255, 255),
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=3.0,
        StrokeCap=skia.Paint.kRound_Cap,
    )
    for start_x, speed, phase, longest in ((25, 2.0, 0.0, 25), (65, 2.5, 1.0, 28), (75, 3.0, 2.0, 13)):
        drip_length = longest * 0.6 + longest * 0.4 * math.sin(elapsed_seconds * speed + phase)
        drip = skia.Path()
        drip.moveTo(start_x, 50)
        drip.quadTo(start_x + 1, 50 + drip_length * 0.5, start_x - 1, 50 + drip_length)
        canvas.drawPath(drip, drip_paint)

    # Magenta splatter with pulsing alpha.
    splatter_paint = skia.Paint(AntiAlias=True)
    for dot_x, dot_y, radius in ((5, 25, 2.0), (85, 15, 2.5), (88, 45, 1.5), (3, 48, 2.0), (45, 5, 1.5)):
        splatter_alpha = int(150 + 50 * math.sin(elapsed_seconds * 4.0 + dot_x))
        splatter_paint.setColor(skia.Color(255, 0, 255, splatter_alpha))
        canvas.drawCircle(dot_x, dot_y, radius, splatter_paint)

    canvas.restore()


def draw_neon_overlay(
    canvas: skia.Canvas, width: int, height: int, elapsed_seconds: float
) -> None:
    """Paint one frame of the overlay over a cleared, transparent canvas."""
    canvas.clear(skia.ColorTRANSPARENT)
    # Every dimension is in 1080p units, scaled here — so the overlay holds
    # its proportions at whatever the camera negotiated.
    scale = height / 1080.0
    slide_progress = ease_out_back(
        min(elapsed_seconds / SLIDE_IN_SECONDS, 1.0), overshoot=2.0
    )
    _draw_lower_third(canvas, width, height, scale, slide_progress, elapsed_seconds)
    _draw_bottom_ticker(canvas, width, height, scale, elapsed_seconds)
    _draw_top_left_hud(canvas, width, height, scale, elapsed_seconds)
    _draw_circuit_watermark(canvas, width, height, scale, elapsed_seconds)
