# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot Skia canvas processor — Python.

End-to-end gate for the subprocess :class:`SkiaContext` runtime
(#577 / #581). Mirrors the in-process Rust stress test
``libs/streamlib-adapter-skia/tests/skia_animated_stress_gl.rs``: the
host pre-allocates a render-target-capable DMA-BUF surface AND an
exportable timeline semaphore, registers both with surface-share, and
spawns this Python processor. On every trigger frame the processor
opens the host surface through ``SkiaContext.acquire_write`` (which
under the hood opens ``OpenGLContext.acquire_write``, imports the
DMA-BUF as a ``GL_TEXTURE_2D`` via EGL, builds a Skia
``GrBackendTexture``, and yields a ``skia.Surface``), draws the
animated scene at frame index ``frame_count``, and releases — Skia's
flush+submit drains the GPU and the inner OpenGL adapter runs
``glFinish`` so the host's per-frame readback sees the drawing.

The animated scene matches the Rust stress test field-for-field
(HSL-modulated linear gradient background + animated stroked ring +
five orbiting discs + spinning spoke wheel + sine curve at the
baseline + color strip), so the produced MP4 + hero PNG should be
visually indistinguishable from the in-process Rust stress run on the
same surface size.

Config keys:
    skia_surface_uuid (str, required)
        Surface-share UUID the host registered the render-target image
        + timeline semaphore under.
    width / height (int, required)
        Surface dimensions.
    fps (int, optional, default 60)
        Frame rate. ``t = frame_count / fps`` is the animation clock
        passed into the draw routine.
"""

from __future__ import annotations

import math
from typing import Optional

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.skia import SkiaContext
from streamlib.adapters.vulkan import VkImageLayout, VulkanContext
from streamlib.surface_adapter import StreamlibSurface, SurfaceFormat, SurfaceUsage


class SkiaCanvasProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._uuid = str(cfg["skia_surface_uuid"])
        self._width = int(cfg["width"])
        self._height = int(cfg["height"])
        self._fps = int(cfg.get("fps", 60))
        self._skia_ctx = SkiaContext.from_runtime(ctx)
        # Dual-register the surface with the Vulkan adapter too so the
        # producer-side QFOT release barrier (#645) can publish layout
        # to host consumers via SkiaContext.release_for_cross_process.
        # Skia composes on the OpenGL adapter, which has no Vulkan
        # device of its own; the Vulkan adapter owns the cross-process
        # release barrier — engine-model composition per
        # docs/architecture/adapter-authoring.md.
        self._vulkan = VulkanContext.from_runtime(ctx)
        self._frame_count = 0
        self._error: Optional[str] = None
        self._surface = StreamlibSurface(
            id=self._uuid,
            width=self._width,
            height=self._height,
            format=int(SurfaceFormat.BGRA8),
            usage=int(SurfaceUsage.RENDER_TARGET | SurfaceUsage.SAMPLED),
        )
        print(
            f"[SkiaCanvas/py] setup uuid={self._uuid} "
            f"size={self._width}x{self._height}@{self._fps}fps",
            flush=True,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return
        try:
            t = self._frame_count / float(self._fps)
            self._draw(t)
            self._frame_count += 1
            if self._frame_count % 120 == 0:
                print(
                    f"[SkiaCanvas/py] drew frame {self._frame_count} (t={t:.2f}s)",
                    flush=True,
                )
        except Exception as e:
            self._error = str(e)
            print(f"[SkiaCanvas/py] draw failed at frame {self._frame_count}: {e}",
                  flush=True)

    def _draw(self, t: float) -> None:
        with self._skia_ctx.acquire_write(self._surface) as guard:
            sk_surface = guard.surface
            canvas = sk_surface.getCanvas()
            _draw_animated_frame(canvas, t, self._width, self._height)
        # Producer-side cross-process release (#645). Skia composes on
        # the OpenGL adapter which has no Vulkan device of its own —
        # delegate to the Vulkan adapter (dual-registration). GENERAL
        # mirrors the OpenGL adapter's release-side convention; the
        # host's pre-stop readback also reads with
        # TextureSourceLayout::General. Pairs with any future host
        # consumer's `acquire_from_foreign` via the bridging fallback
        # on NVIDIA / QFOT-acquire on Mesa.
        self._skia_ctx.release_for_cross_process(
            self._surface, self._vulkan, VkImageLayout.GENERAL
        )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print(
            f"[SkiaCanvas/py] teardown drew={self._frame_count} error={self._error}",
            flush=True,
        )


# ---------------------------------------------------------------------------
# Animated draw — port of `draw_animated_frame` in
# `libs/streamlib-adapter-skia/tests/skia_animated_stress_gl.rs`.
# Field-for-field; tweaks only where skia-python's API requires it.
# ---------------------------------------------------------------------------


def _hsl_to_color4f(h: float, s: float, l: float):
    """Mirror of the Rust `hsl` helper. Returns a `skia.Color4f`."""
    import skia

    h = h % 360.0
    c = (1.0 - abs(2.0 * l - 1.0)) * s
    x = c * (1.0 - abs((h / 60.0) % 2.0 - 1.0))
    m = l - c / 2.0
    sextant = int(h / 60.0)
    if sextant == 0:
        r, g, b = c, x, 0.0
    elif sextant == 1:
        r, g, b = x, c, 0.0
    elif sextant == 2:
        r, g, b = 0.0, c, x
    elif sextant == 3:
        r, g, b = 0.0, x, c
    elif sextant == 4:
        r, g, b = x, 0.0, c
    else:
        r, g, b = c, 0.0, x
    return skia.Color4f(r + m, g + m, b + m, 1.0)


def _color4f_to_int(color4f) -> int:
    """skia-python's gradient APIs want ARGB ints; convert from Color4f."""
    import skia

    a = max(0.0, min(1.0, color4f.fA))
    r = max(0.0, min(1.0, color4f.fR))
    g = max(0.0, min(1.0, color4f.fG))
    b = max(0.0, min(1.0, color4f.fB))
    return skia.ColorSetARGB(int(a * 255), int(r * 255), int(g * 255), int(b * 255))


def _draw_animated_frame(canvas, t: float, width: int, height: int) -> None:
    import skia

    tau = 2.0 * math.pi
    w = float(width)
    h = float(height)

    # Background — vertical linear gradient between two HSL-modulated colors.
    top = _color4f_to_int(_hsl_to_color4f(t * 40.0, 0.80, 0.20))
    bottom = _color4f_to_int(_hsl_to_color4f(t * 40.0 + 120.0, 0.80, 0.55))
    bg_shader = skia.GradientShader.MakeLinear(
        [skia.Point(0.0, 0.0), skia.Point(0.0, h)],
        [top, bottom],
    )
    bg_paint = skia.Paint()
    bg_paint.setShader(bg_shader)
    canvas.drawRect(skia.Rect.MakeLTRB(0.0, 0.0, w, h), bg_paint)

    # Animated stroked ring.
    ring_radius = 180.0 + math.sin(t * tau * 0.5) * 18.0
    ring_color = _hsl_to_color4f(t * 60.0, 0.95, 0.55)
    ring = skia.Paint()
    ring.setColor4f(ring_color, None)
    ring.setStyle(skia.Paint.Style.kStroke_Style)
    ring.setStrokeWidth(6.0 + abs(math.sin(t * tau * 0.3)) * 4.0)
    ring.setAntiAlias(True)
    canvas.drawCircle(w * 0.5, h * 0.5, ring_radius, ring)

    # Five orbiting discs at slightly translucent fill.
    for i in range(5):
        phase = i * 0.4
        fx = 0.5 + 0.6 * (i * 0.3)
        fy = 0.7 + 0.5 * (i * 0.2)
        x = w * 0.5 + math.sin(t * fx + phase) * (190.0 - i * 12.0)
        y = h * 0.5 + math.cos(t * fy + phase) * (180.0 - i * 14.0)
        radius = 26.0 + math.sin(t * 1.6 + phase) * 10.0
        color = _hsl_to_color4f(t * 70.0 + i * 72.0, 0.92, 0.58)
        color = skia.Color4f(color.fR, color.fG, color.fB, 0.78)
        paint = skia.Paint()
        paint.setColor4f(color, None)
        paint.setAntiAlias(True)
        canvas.drawCircle(x, y, radius, paint)

    # Spoke wheel, 16 spokes rotating around the center.
    spokes = 16
    cx = w * 0.5
    cy = h * 0.5
    inner_r = 36.0
    outer_r = 70.0 + math.sin(t * tau * 0.4) * 18.0
    spoke_color = _hsl_to_color4f(t * 90.0 + 200.0, 0.6, 0.85)
    spoke = skia.Paint()
    spoke.setColor4f(spoke_color, None)
    spoke.setStyle(skia.Paint.Style.kStroke_Style)
    spoke.setStrokeWidth(2.0)
    spoke.setAntiAlias(True)
    spoke_path = skia.Path()
    for s in range(spokes):
        a = (s / spokes) * tau + t * 1.2
        spoke_path.moveTo(cx + math.cos(a) * inner_r, cy + math.sin(a) * inner_r)
        spoke_path.lineTo(cx + math.cos(a) * outer_r, cy + math.sin(a) * outer_r)
    canvas.drawPath(spoke_path, spoke)

    # Sine curve along the lower edge — 96 segments, slowly drifting
    # amplitude + phase.
    curve = skia.Path()
    segments = 96
    amp = 30.0 + math.sin(t * 0.7) * 12.0
    baseline = h - 56.0
    phase = t * 3.0
    curve.moveTo(0.0, baseline)
    for i in range(1, segments + 1):
        u = i / segments
        x = u * w
        y = baseline - math.sin(u * tau * 3.0 + phase) * amp
        curve.lineTo(x, y)
    curve_paint = skia.Paint()
    curve_paint.setColor4f(skia.Color4f(1.0, 1.0, 1.0, 0.92), None)
    curve_paint.setStyle(skia.Paint.Style.kStroke_Style)
    curve_paint.setStrokeWidth(3.5)
    curve_paint.setAntiAlias(True)
    canvas.drawPath(curve, curve_paint)

    # Color strip — 7 small rects rotating through HSL.
    strip_y = h - 22.0
    strip_h = 14.0
    for i in range(7):
        tile = skia.Paint()
        tile.setColor4f(_hsl_to_color4f(t * 120.0 + i * 51.0, 0.95, 0.55), None)
        x0 = 16.0 + i * 22.0
        canvas.drawRect(
            skia.Rect.MakeLTRB(x0, strip_y, x0 + 18.0, strip_y + strip_h),
            tile,
        )
