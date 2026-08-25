# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The broadcast overlay, drawn with skia onto a transparent canvas.

Kept clear of streamlib on purpose: this is ordinary 2D drawing code with a
canvas and a clock, so it runs — and is tested — without a GPU, an engine, or a
graph. `processors/neon_overlay_source.py` is the part that knows where the
pixels go.
"""

from __future__ import annotations

import math

import skia

__all__ = [
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

CHANNEL_NAME = "N54 NEWS"
HEADLINE = "CYBERPUNK PIPELINE"
SUBHEADLINE = "LIVE // PYTHON PROCESSORS // GLSL KERNELS"
WATERMARK_TAG = "CL"


def ease_out_back(progress: float, overshoot: float = 1.70158) -> float:
    """Overshoots past the target, then settles — the snappy HUD feel."""
    shifted = progress - 1.0
    return shifted * shifted * ((overshoot + 1.0) * shifted + overshoot) + 1.0


def _overlay_font(size_in_pixels: float, *, bold: bool) -> skia.Font:
    """A font at `size_in_pixels`, from whatever the host actually has.

    Named families are asked for and not insisted on: skia falls back to the
    default typeface when the host has no match, which is the difference
    between an overlay that renders everywhere and one that renders here.
    """
    style = skia.FontStyle.Bold() if bold else skia.FontStyle.Normal()
    typeface = skia.Typeface.MakeFromName("DejaVu Sans", style)
    return skia.Font(typeface, size_in_pixels)


def _chamfered_panel(left: float, top: float, right: float, bottom: float, chamfer: float) -> skia.Path:
    """A rectangle with its top-left and bottom-right corners cut off."""
    path = skia.Path()
    path.moveTo(left + chamfer, top)
    path.lineTo(right, top)
    path.lineTo(right, bottom - chamfer)
    path.lineTo(right - chamfer, bottom)
    path.lineTo(left, bottom)
    path.lineTo(left, top + chamfer)
    path.close()
    return path


def _draw_lower_third(
    canvas: skia.Canvas, width: int, height: int, scale: float, slide_progress: float
) -> None:
    panel_height = 96.0 * scale
    panel_width = 760.0 * scale
    margin = 48.0 * scale
    chamfer = 18.0 * scale

    # Off-screen left at 0, docked at 1.
    slide_offset = (1.0 - slide_progress) * (panel_width + margin)
    left = margin - slide_offset
    top = height - margin - panel_height
    right = left + panel_width
    bottom = top + panel_height

    panel = _chamfered_panel(left, top, right, bottom, chamfer)
    canvas.drawPath(panel, skia.Paint(AntiAlias=True, Color=CYBER_YELLOW))

    # Clipped to the panel so the logo block takes the panel's chamfer rather
    # than squaring off the corner it sits in.
    logo_width = 190.0 * scale
    canvas.save()
    canvas.clipPath(panel, doAntiAlias=True)
    canvas.drawRect(
        skia.Rect.MakeLTRB(left, top, left + logo_width, bottom),
        skia.Paint(AntiAlias=True, Color=CYBER_RED),
    )
    canvas.restore()
    canvas.drawString(
        CHANNEL_NAME,
        left + 18.0 * scale,
        top + panel_height * 0.62,
        _overlay_font(30.0 * scale, bold=True),
        skia.Paint(AntiAlias=True, Color=skia.ColorWHITE),
    )

    text_left = left + logo_width + 24.0 * scale
    canvas.drawString(
        HEADLINE,
        text_left,
        top + panel_height * 0.44,
        _overlay_font(36.0 * scale, bold=True),
        skia.Paint(AntiAlias=True, Color=CYBER_DARK),
    )
    canvas.drawString(
        SUBHEADLINE,
        text_left,
        top + panel_height * 0.78,
        _overlay_font(20.0 * scale, bold=False),
        skia.Paint(AntiAlias=True, Color=CYBER_DARK),
    )

    # A cyan tech rule under the panel, drawn only as far as it has slid in.
    canvas.drawRect(
        skia.Rect.MakeLTRB(left, bottom, right, bottom + 5.0 * scale),
        skia.Paint(AntiAlias=True, Color=CYBER_CYAN),
    )


def _draw_watermark(
    canvas: skia.Canvas, width: int, height: int, scale: float, elapsed_seconds: float
) -> None:
    tag_x = 60.0 * scale
    tag_y = 120.0 * scale
    font = _overlay_font(84.0 * scale, bold=True)

    glow = skia.Paint(
        AntiAlias=True,
        Color=CYBER_CYAN,
        MaskFilter=skia.MaskFilter.MakeBlur(skia.kNormal_BlurStyle, 14.0 * scale),
    )
    canvas.drawString(WATERMARK_TAG, tag_x, tag_y, font, glow)
    canvas.drawString(
        WATERMARK_TAG,
        tag_x,
        tag_y,
        font,
        skia.Paint(AntiAlias=True, Color=skia.ColorWHITE),
    )

    # Three drips, each on its own period so they never fall in step.
    drip_paint = skia.Paint(AntiAlias=True, Color=CYBER_CYAN)
    for index, (offset_x, period_seconds, longest_drip) in enumerate(
        ((14.0, 3.1, 46.0), (52.0, 4.3, 30.0), (86.0, 5.7, 38.0))
    ):
        phase = (elapsed_seconds / period_seconds + index * 0.37) % 1.0
        drip_length = longest_drip * scale * (1.0 - math.cos(phase * math.pi)) * 0.5
        canvas.drawRoundRect(
            skia.Rect.MakeXYWH(
                tag_x + offset_x * scale, tag_y, 5.0 * scale, drip_length
            ),
            2.5 * scale,
            2.5 * scale,
            drip_paint,
        )


def draw_neon_overlay(
    canvas: skia.Canvas, width: int, height: int, elapsed_seconds: float
) -> None:
    """Paint one frame of the overlay over a cleared, transparent canvas."""
    canvas.clear(skia.ColorTRANSPARENT)
    # Every dimension below is in 1080p units, scaled here — so the overlay
    # holds its proportions at whatever the camera negotiated.
    scale = height / 1080.0
    slide_progress = ease_out_back(min(elapsed_seconds / SLIDE_IN_SECONDS, 1.0))
    _draw_lower_third(canvas, width, height, scale, slide_progress)
    _draw_watermark(canvas, width, height, scale, elapsed_seconds)
