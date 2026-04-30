# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot Skia canvas processor — Python.

End-to-end gate for the subprocess :class:`SkiaContext` runtime
(#577). The host pre-allocates a render-target-capable DMA-BUF
surface AND an exportable timeline semaphore, registers both with
surface-share, and installs no bridge (Skia composes on the OpenGL
adapter, which doesn't need one). This processor receives a trigger
``Videoframe``, opens the host surface through
``SkiaContext.acquire_write`` (which under the hood opens
``OpenGLContext.acquire_write``, imports the DMA-BUF as a
``GL_TEXTURE_2D`` via EGL, builds a Skia ``GrBackendTexture``, and
yields a ``skia.Surface``), draws a known shape (red disc on blue
background), and releases — Skia's flush+submit drains the GPU and
the inner OpenGL adapter runs ``glFinish`` so the host's pre-stop
readback sees the drawing.

Config keys:
    skia_surface_uuid (str, required)
        Surface-share UUID the host registered the render-target image
        + timeline semaphore under.
    width / height (int, required)
        Surface dimensions.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.skia import SkiaContext
from streamlib.surface_adapter import SurfaceFormat, SurfaceUsage


@dataclass(frozen=True)
class _SurfaceDescriptor:
    """Minimal duck-typed descriptor for `SkiaContext.acquire_*`.

    The Skia wrapper needs `id` (surface-share UUID), `width`, `height`,
    `format` to build a `GrBackendTexture`. `streamlib.surface_adapter`
    exports `StreamlibSurface` only as a `typing.Protocol` (not a
    concrete class), so processors construct their own descriptor; any
    object with these attributes is accepted.
    """

    id: str
    width: int
    height: int
    format: int
    usage: int


class SkiaCanvasProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._uuid = str(cfg["skia_surface_uuid"])
        self._width = int(cfg["width"])
        self._height = int(cfg["height"])
        self._skia_ctx = SkiaContext.from_runtime(ctx)
        self._drawn = False
        self._error: Optional[str] = None
        self._surface = _SurfaceDescriptor(
            id=self._uuid,
            width=self._width,
            height=self._height,
            format=int(SurfaceFormat.BGRA8),
            usage=int(SurfaceUsage.RENDER_TARGET | SurfaceUsage.SAMPLED),
        )
        print(
            f"[SkiaCanvas/py] setup uuid={self._uuid} "
            f"size={self._width}x{self._height}",
            flush=True,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return
        if self._drawn:
            return
        try:
            self._draw_once()
            self._drawn = True
            print(
                f"[SkiaCanvas/py] Skia canvas drawn into surface '{self._uuid}'",
                flush=True,
            )
        except Exception as e:
            self._error = str(e)
            print(
                f"[SkiaCanvas/py] draw failed: {e}", flush=True,
            )

    def _draw_once(self) -> None:
        import skia

        with self._skia_ctx.acquire_write(self._surface) as guard:
            sk_surface = guard.surface
            canvas = sk_surface.getCanvas()
            # Background — solid blue.
            canvas.clear(skia.ColorBLUE)
            # Foreground — bright red disc, antialiased, centered.
            paint = skia.Paint()
            paint.setColor(skia.ColorRED)
            paint.setAntiAlias(True)
            cx = self._width / 2.0
            cy = self._height / 2.0
            radius = min(self._width, self._height) * 0.35
            canvas.drawCircle(cx, cy, radius, paint)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print(
            f"[SkiaCanvas/py] teardown drawn={self._drawn} error={self._error}",
            flush=True,
        )
