# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Skia surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib_adapter_skia::SkiaGlContext`` (#576,
#577) by composing on the Python ``opengl`` adapter
(``streamlib.adapters.opengl``) plus ``skia-python`` to wrap the
underlying ``GL_TEXTURE_2D`` as a ``skia.Surface`` (write) or
``skia.Image`` (read).

The cdylib stays small — there is no ``slpn_skia_*`` FFI surface; every
line of GPU work routes through the existing ``slpn_opengl_*`` symbols
the ``opengl`` adapter wires up.

Why GL, not Vulkan, in Python
-----------------------------

skia-python's pybind11 Vulkan binding is unimplemented: ``GrVkBackendContext``
exposes only the no-arg constructor (no setters for ``fInstance`` /
``fDevice`` / ``fQueue`` / ``fGetProc``), ``GrVkAlloc`` and
``GrVkDrawableInfo`` are upstream stubs, and ``GrVkImageInfo``'s
``fImage`` / ``fImageTiling`` / ``fImageLayout`` / ``fFormat`` are
commented out. Verified against ``v144.0.post2`` and ``main`` on
2026-04-29.

The Rust binding (``skia-safe``) is fully functional on Vulkan, so the
Rust adapter's ``MakeVulkan`` path is unaffected — this is purely a
Python-side limitation. The host VkImage allocation, DRM modifier
choice, DMA-BUF export, and host-side readback all stay Vulkan; the
subprocess imports the DMA-BUF as a ``GL_TEXTURE_2D`` via EGL DMA-BUF
import (already wired by ``streamlib-adapter-opengl``) and Skia is
constructed via ``skia.GrDirectContext.MakeGL()`` on top of that GL
context.

Polyglot coverage: Python only. ``skia-python`` is the only maintained
cross-language Skia binding for the runtimes streamlib supports —
there is no Deno equivalent. A Deno customer that needs Skia today
should drive Skia themselves against
:meth:`streamlib.adapters.opengl.OpenGLContext.acquire_*` until a Deno
Skia binding emerges.
"""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
from typing import Iterator, Optional, Protocol, runtime_checkable

from streamlib.adapters.opengl import (
    GL_TEXTURE_2D,
    OpenGLContext,
)
from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    SurfaceFormat,
)

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "SkiaReadView",
    "SkiaWriteView",
    "SkiaContextProtocol",
    "SkiaContext",
]


# OpenGL ``GL_RGBA8`` internal format. The OpenGL adapter imports
# DMA-BUFs as ``GL_RGBA8``-typed ``GL_TEXTURE_2D``s regardless of
# whether the host allocated BGRA8 or RGBA8; the byte order is then
# disambiguated downstream by Skia's ``ColorType``. Mirrors the Rust GL
# backend's ``gpu_gl::Format::RGBA8`` choice in
# ``streamlib-adapter-skia/src/gl_adapter.rs``.
_GL_RGBA8: int = 0x8058


@dataclass(frozen=True)
class SkiaWriteView:
    """View handed back inside an ``acquire_write`` scope.

    ``surface`` is a live ``skia.Surface`` wrapping the host's
    ``GL_TEXTURE_2D``. The customer draws into ``surface.getCanvas()``;
    on scope exit the wrapper flushes Skia's command stream and
    releases the underlying OpenGL acquire (which runs ``glFinish`` so
    the next consumer sees the writes).
    """

    # ``object`` (rather than ``skia.Surface``) so this module imports
    # cleanly even when ``skia-python`` is not installed — failures
    # then surface at :class:`SkiaContext` construction time with a
    # clear message.
    surface: object


@dataclass(frozen=True)
class SkiaReadView:
    """View handed back inside an ``acquire_read`` scope.

    ``image`` is a live ``skia.Image`` referencing the host's
    ``GL_TEXTURE_2D``. The customer samples it; on scope exit the
    wrapper drops the image and releases the underlying OpenGL acquire.
    """

    image: object


@runtime_checkable
class SkiaContextProtocol(Protocol):
    """Customer-facing handle the subprocess runtime hands out.

    Same shape as the Rust ``streamlib_adapter_skia::SkiaGlContext``:
    scoped acquire/release returning a typed view.
    """

    def acquire_read(self, surface): ...

    def acquire_write(self, surface): ...


class SkiaContext:
    """Subprocess-side Skia adapter runtime.

    Composes on :class:`streamlib.adapters.opengl.OpenGLContext` —
    every GL operation (host surface registration, EGL/DMA-BUF import,
    ``glFinish`` handoff) routes through the inner OpenGL adapter, and
    Skia is the framework that wraps the resulting ``GL_TEXTURE_2D``.

    Construct via :meth:`from_runtime`. Single :class:`SkiaContext` per
    subprocess; :meth:`from_runtime` returns the cached instance on
    repeat calls. The wrapper builds its ``GrDirectContext`` once
    against the OpenGL adapter's EGL+GL stack via
    ``skia.GrDirectContext.MakeGL()``; per-acquire construction wraps
    the live ``GL_TEXTURE_2D`` as a Skia ``GrBackendTexture`` and
    flushes on scope exit.

    Importing this module requires ``skia-python``. The import is
    deferred to first construction so that loading
    ``streamlib.adapters`` doesn't pay the ``skia-python`` import cost
    for customers that don't use Skia.
    """

    _shared_instance: Optional["SkiaContext"] = None

    def __init__(self, opengl_ctx: OpenGLContext) -> None:
        # Defer the skia-python import to construction so loading the
        # `streamlib.adapters` package doesn't drag skia-python in for
        # customers that don't use Skia. Failures (skia-python not
        # installed) surface here with a clear message instead of at
        # import time.
        try:
            import skia  # noqa: F401
        except ImportError as e:
            raise RuntimeError(
                "SkiaContext requires `skia-python` (>=120). "
                "Install via `pip install skia-python`."
            ) from e
        self._skia = skia
        self._opengl = opengl_ctx
        # ``GrDirectContext`` build is deferred to first acquire — see
        # :meth:`_ensure_direct_context`.
        self._direct_context = None

    @classmethod
    def from_runtime(cls, runtime_context) -> "SkiaContext":
        """Build (or fetch the cached) :class:`SkiaContext` for this
        subprocess. The inner :class:`OpenGLContext` is fetched via its
        own :meth:`OpenGLContext.from_runtime` — a single
        ``OpenGLContext`` is shared between this Skia adapter and any
        other GL-using adapter the same subprocess hosts.
        """
        if cls._shared_instance is None:
            opengl_ctx = OpenGLContext.from_runtime(runtime_context)
            cls._shared_instance = cls(opengl_ctx)
        return cls._shared_instance

    def _ensure_direct_context(self):
        """Build (lazily, on first acquire) a ``skia.GrDirectContext``
        via ``skia.GrDirectContext.MakeGL(GrGLInterface.MakeEGL())``.

        Two pieces of indirection are load-bearing here:

        1. **EGL-specific interface.** ``MakeGL()`` with no args calls
           ``GrGLMakeNativeInterface()``, which on Linux falls through
           to GLX, NOT EGL. Subprocess GL is brought up against EGL
           (the OpenGL adapter uses ``EGL_EXT_image_dma_buf_import``),
           so we must hand Skia an interface explicitly resolved from
           ``eglGetProcAddress``. ``GrGLInterface.MakeEGL()`` does
           that. Mirror of the Rust GL backend's
           ``Interface::new_load_with(|sym| egl.get_proc_address(sym))``
           in ``streamlib-adapter-skia/src/gl_adapter.rs``.
        2. **Lazy build.** The cdylib's ``slpn_opengl_runtime_new``
           brings the EGL context up but only makes it current on
           the calling thread between ``slpn_opengl_acquire_*`` and
           ``slpn_opengl_release_*``. ``MakeEGL`` works regardless of
           current-state (it asks the EGL impl directly), but we still
           defer the build to inside the first acquire to keep all
           Skia work on a thread where the adapter's EGL context is
           current — that's where every subsequent ``MakeFromBackend*``
           and ``flushAndSubmit`` runs.

        Once built, the DirectContext is cached for the lifetime of
        the :class:`SkiaContext` (Skia copies the resolved proc table
        into its own command buffer, so the context is reusable
        across future acquires of the same subprocess EGL stack).
        """
        if self._direct_context is not None:
            return self._direct_context
        skia = self._skia
        interface = skia.GrGLInterface.MakeEGL()
        if interface is None:
            raise RuntimeError(
                "SkiaContext: skia.GrGLInterface.MakeEGL() returned None — "
                "Skia could not resolve GL function pointers via "
                "eglGetProcAddress. Verify skia-python was built with "
                "EGL support and that libEGL.so.1 is loadable."
            )
        ctx = skia.GrDirectContext.MakeGL(interface)
        if ctx is None:
            raise RuntimeError(
                "SkiaContext: skia.GrDirectContext.MakeGL(MakeEGL()) returned "
                "None — Skia could not bring up a GL backend against the "
                "subprocess's EGL context. Verify the OpenGL adapter's EGL "
                "context is current at this point (this method is called "
                "inside an `OpenGLContext.acquire_*` scope, which is "
                "supposed to make EGL current)."
            )
        self._direct_context = ctx
        return ctx

    @staticmethod
    def _color_type(skia, surface_format: int):
        """Map a :class:`SurfaceFormat` to a Skia ``ColorType``.

        Mirrors ``surface_format_to_color_type`` in the Rust GL backend
        (``gl_adapter.rs``): both BGRA8 and RGBA8 surfaces ride a
        ``GL_RGBA8`` storage format internally; the ``ColorType`` is
        what tells Skia how to interpret the bytes.
        """
        if surface_format == SurfaceFormat.BGRA8:
            return skia.ColorType.kBGRA_8888_ColorType
        if surface_format == SurfaceFormat.RGBA8:
            return skia.ColorType.kRGBA_8888_ColorType
        raise RuntimeError(
            f"SkiaContext: unsupported SurfaceFormat {surface_format} — "
            "Skia GL backend supports BGRA8 / RGBA8 only"
        )

    @staticmethod
    def _surface_format(surface) -> int:
        if isinstance(surface, str):
            raise TypeError(
                "SkiaContext: bare pool_id strings are not enough to "
                "construct a Skia view; pass a StreamlibSurface "
                "(carries format / width / height)."
            )
        fmt = getattr(surface, "format", None)
        if fmt is None:
            raise TypeError(
                "SkiaContext: surface must carry a `format` field "
                f"(StreamlibSurface or compatible), got {surface!r}"
            )
        return int(fmt)

    @staticmethod
    def _surface_dims(surface) -> tuple[int, int]:
        w = getattr(surface, "width", None)
        h = getattr(surface, "height", None)
        if w is None or h is None:
            raise TypeError(
                "SkiaContext: surface must carry width/height "
                f"(StreamlibSurface or compatible), got {surface!r}"
            )
        return int(w), int(h)

    def _build_backend_texture(
        self, gl_texture_id: int, width: int, height: int
    ):
        """Build a Skia ``GrBackendTexture`` wrapping the OpenGL
        adapter's imported ``GL_TEXTURE_2D``.

        Internal format is ``GL_RGBA8`` regardless of host BGRA8/RGBA8
        (the EGL DMA-BUF importer treats the underlying memory as
        ``GL_RGBA8``); channel order is disambiguated by ``ColorType``,
        not the GL internal format. Matches the Rust GL backend's
        ``build_backend_texture`` shape.
        """
        skia = self._skia
        gl_info = skia.GrGLTextureInfo(GL_TEXTURE_2D, gl_texture_id, _GL_RGBA8)
        return skia.GrBackendTexture(
            width, height, skia.GrMipmapped.kNo, gl_info
        )

    @contextmanager
    def acquire_write(self, surface) -> Iterator[SkiaWriteView]:
        """Acquire write access. Yields a :class:`SkiaWriteView` whose
        ``surface`` is a live ``skia.Surface`` wrapping the host's
        ``GL_TEXTURE_2D``. The customer draws into
        ``surface.getCanvas()``; on scope exit the wrapper:

        1. Calls ``surface.flushAndSubmit()`` to drain Skia's command
           stream into GL.
        2. Drops the Skia ``Surface`` so its ``DirectContext`` refcount
           is released.
        3. Exits the inner :meth:`OpenGLContext.acquire_write` scope,
           which runs ``glFinish`` so the next consumer sees the
           writes through the underlying DMA-BUF.
        """
        skia = self._skia
        fmt = self._surface_format(surface)
        width, height = self._surface_dims(surface)
        color_type = self._color_type(skia, fmt)
        with self._opengl.acquire_write(surface) as gl_view:
            direct_context = self._ensure_direct_context()
            backend_texture = self._build_backend_texture(
                gl_view.gl_texture_id, width, height
            )
            sk_surface = skia.Surface.MakeFromBackendTexture(
                direct_context,
                backend_texture,
                skia.GrSurfaceOrigin.kTopLeft_GrSurfaceOrigin,
                0,  # sample_count
                color_type,
                None,  # color_space
                None,  # surface_props
            )
            if sk_surface is None:
                raise RuntimeError(
                    "SkiaContext.acquire_write: "
                    "skia.Surface.MakeFromBackendTexture returned None for "
                    f"surface (gl_texture={gl_view.gl_texture_id}, "
                    f"format={fmt}, size={width}x{height}). Common causes: "
                    "missing render-target capability on the imported GL "
                    "texture; format/color-type mismatch; or Skia's "
                    "GrGLCaps doesn't list RGBA8 as renderable on this "
                    "driver."
                )
            try:
                yield SkiaWriteView(surface=sk_surface)
            finally:
                # Drain Skia's command stream into GL before the inner
                # OpenGL release runs `glFinish`. Without this, the
                # OpenGL release might signal the timeline before
                # Skia's commands have made it into the GL queue.
                sk_surface.flushAndSubmit()
                # Drop the Surface refcount so the DirectContext is
                # not pinned across the inner release.
                del sk_surface

    @contextmanager
    def acquire_read(self, surface) -> Iterator[SkiaReadView]:
        """Acquire read access. Yields a :class:`SkiaReadView` whose
        ``image`` is a live ``skia.Image`` referencing the host's
        ``GL_TEXTURE_2D``. Read-only — no flush needed on scope exit.
        """
        skia = self._skia
        fmt = self._surface_format(surface)
        width, height = self._surface_dims(surface)
        color_type = self._color_type(skia, fmt)
        with self._opengl.acquire_read(surface) as gl_view:
            direct_context = self._ensure_direct_context()
            backend_texture = self._build_backend_texture(
                gl_view.gl_texture_id, width, height
            )
            sk_image = skia.Image.MakeFromTexture(
                direct_context,
                backend_texture,
                skia.GrSurfaceOrigin.kTopLeft_GrSurfaceOrigin,
                color_type,
                # `kOpaque_AlphaType` mirrors the Rust GL backend's
                # `AlphaType::Opaque` choice in `gl_adapter.rs`.
                skia.AlphaType.kOpaque_AlphaType,
                None,  # color_space
            )
            if sk_image is None:
                raise RuntimeError(
                    "SkiaContext.acquire_read: skia.Image.MakeFromTexture "
                    f"returned None for surface "
                    f"(gl_texture={gl_view.gl_texture_id}, format={fmt}, "
                    f"size={width}x{height})"
                )
            try:
                yield SkiaReadView(image=sk_image)
            finally:
                del sk_image
