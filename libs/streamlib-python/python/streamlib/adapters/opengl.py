# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""OpenGL/EGL surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-opengl`` (#512, #530). The
subprocess's actual EGL+GL handling lives in `streamlib-python-native`
(via the `slpn_opengl_*` FFI symbols, which delegate to the host
adapter crate's `EglRuntime` and `OpenGlSurfaceAdapter`). This module
provides:

  * Typed views the subprocess sees inside ``acquire_*`` scopes —
    ``OpenGLReadView`` / ``OpenGLWriteView`` exposing a single
    integer ``gl_texture_id`` and the constant ``target =
    GL_TEXTURE_2D``.
  * ``OpenGLContext`` — the concrete subprocess runtime. Customers
    call ``with ctx.acquire_write(surface) as view:`` and bind
    ``view.gl_texture_id`` to their PyOpenGL / ModernGL / raw
    ctypes-against-libGLESv2 stack. The adapter's EGL context is
    current on the calling thread for the lifetime of the acquire,
    so any GL library that latches onto the current context "just
    works."

Customers never see DMA-BUF FDs, fourcc codes, plane offsets,
strides, or DRM modifiers. Per the NVIDIA EGL DMA-BUF render-target
learning, the host allocator picks a tiled, render-target-capable
modifier so the resulting GL texture is always a regular
``GL_TEXTURE_2D`` — never ``GL_TEXTURE_EXTERNAL_OES``.

PyOpenGL configuration
----------------------

PyOpenGL has known interaction issues with non-default GL contexts.
The ``configure_pyopengl_for_streamlib_subprocess`` helper sets the
three environment variables PyOpenGL reads at import time:

  * ``PYOPENGL_PLATFORM=egl`` — bind to the same EGL stack the
    runtime owns instead of GLX/AGL.
  * ``PYOPENGL_CONTEXT_CHECKING=False`` — skip PyOpenGL's per-call
    "is this the current context?" probe; the runtime's
    make-current discipline handles it.
  * ``PYOPENGL_ERROR_CHECKING=False`` — disable PyOpenGL's
    glGetError-after-every-call wrapper, which is a 5–10× cost.

Customers SHOULD call this helper before importing PyOpenGL — the
typical place is the subprocess processor's ``setup`` hook.
"""

from __future__ import annotations

import ctypes
import itertools
import os
from contextlib import AbstractContextManager, contextmanager
from dataclasses import dataclass
from typing import Iterator, Optional, Protocol, runtime_checkable

from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    StreamlibSurface,
)

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "GL_TEXTURE_2D",
    "GL_TEXTURE_EXTERNAL_OES",
    "OpenGLReadView",
    "OpenGLWriteView",
    "OpenGLSurfaceAdapter",
    "OpenGLContextProtocol",
    "OpenGLContext",
    "configure_pyopengl_for_streamlib_subprocess",
]


# `GL_TEXTURE_2D` enumerant — re-exported so customers don't have to
# import a GL binding just to compare `view.target`. Matches the
# Rust crate's `GL_TEXTURE_2D` constant.
GL_TEXTURE_2D: int = 0x0DE1

# `GL_TEXTURE_EXTERNAL_OES` enumerant from `GL_OES_EGL_image_external`.
# Returned in `view.target` for surfaces acquired via
# :meth:`OpenGLContext.acquire_read_external_oes` — the consumer's GLSL
# must `#extension GL_OES_EGL_image_external_essl3 : require` and
# sample via `samplerExternalOES`. Matches the Rust crate's
# `GL_TEXTURE_EXTERNAL_OES` constant.
GL_TEXTURE_EXTERNAL_OES: int = 0x8D65


@dataclass(frozen=True)
class OpenGLReadView:
    """View handed back inside an ``acquire_read`` /
    ``acquire_read_external_oes`` scope.

    ``gl_texture_id`` is an integer the customer feeds into PyOpenGL
    / ModernGL: ``glBindTexture(view.target, view.gl_texture_id)``.
    ``target`` is :data:`GL_TEXTURE_2D` for surfaces acquired via
    :meth:`OpenGLContext.acquire_read` (host render-target-capable
    DMA-BUFs) or :data:`GL_TEXTURE_EXTERNAL_OES` for surfaces acquired
    via :meth:`OpenGLContext.acquire_read_external_oes` (sampler-only
    DMA-BUFs, e.g. linear camera ring textures on NVIDIA).
    """

    gl_texture_id: int
    target: int = GL_TEXTURE_2D


@dataclass(frozen=True)
class OpenGLWriteView:
    """View handed back inside an ``acquire_write`` scope.

    Same shape as [`OpenGLReadView`] but distinguished at the type
    level so static checkers can keep "I have a read guard but
    tried to write" a static error.
    """

    gl_texture_id: int
    target: int = GL_TEXTURE_2D


@runtime_checkable
class OpenGLSurfaceAdapter(Protocol):
    """Protocol an in-process Python OpenGL adapter implements.

    Mirrors the trait shape the Rust ``OpenGlSurfaceAdapter``
    exposes — both flavors of acquisition (blocking and
    non-blocking) and the surface-id-keyed registry.
    """

    def acquire_read(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[OpenGLReadView]: ...

    def acquire_write(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[OpenGLWriteView]: ...

    def try_acquire_read(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[OpenGLReadView]]: ...

    def try_acquire_write(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[OpenGLWriteView]]: ...


@runtime_checkable
class OpenGLContextProtocol(Protocol):
    """Customer-facing handle the subprocess runtime hands out
    (Protocol shape — :class:`OpenGLContext` below is the concrete
    implementation).
    """

    def acquire_read(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[OpenGLReadView]: ...

    def acquire_write(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[OpenGLWriteView]: ...

    def try_acquire_read(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[OpenGLReadView]]: ...

    def try_acquire_write(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[OpenGLWriteView]]: ...


def configure_pyopengl_for_streamlib_subprocess() -> None:
    """Set the env vars PyOpenGL reads at import time so it binds to
    the runtime's EGL stack and skips its own context/error-check
    wrappers.

    Idempotent — safe to call multiple times. Must be called BEFORE
    importing ``OpenGL.GL`` (PyOpenGL inspects these at module load
    time, not on every call).
    """
    os.environ.setdefault("PYOPENGL_PLATFORM", "egl")
    os.environ.setdefault("PYOPENGL_CONTEXT_CHECKING", "False")
    os.environ.setdefault("PYOPENGL_ERROR_CHECKING", "False")


# =============================================================================
# Concrete OpenGLContext implementation (#530)
# =============================================================================

# Surface-id namespace inside this subprocess. Counted up by `from_runtime` —
# the host's pool_id (string UUID) is mapped to a u64 the adapter uses
# internally; customers never see the u64.
_SURFACE_ID_COUNTER = itertools.count(start=1)


class OpenGLContext:
    """Subprocess-side OpenGL adapter runtime (#530).

    Bring up the adapter's EGL display + GL context inside this
    subprocess and expose scoped acquire/release that hands customers
    a real ``GL_TEXTURE_2D`` id. The adapter's EGL context is current
    on the calling thread for the lifetime of an ``acquire_*`` scope —
    any GL library that latches onto the current EGL context (PyOpenGL
    with ``PYOPENGL_PLATFORM=egl``, ModernGL, raw ctypes against
    ``libGLESv2.so``, a Deno-FFI game-engine binding, etc.) will see
    the texture id as live.

    Construct via :meth:`from_runtime` — pass the typed runtime context
    you receive in ``setup``/``process``. Single :class:`OpenGLContext`
    per subprocess; :meth:`from_runtime` returns the cached instance on
    repeat calls.

    Acquire/release MUST happen on the same thread. The EGL spec
    pins a context's "current" state to a thread; releasing on a
    different thread leaks the context binding. Python processors
    typically run on a single thread, so this is the natural shape.
    """

    _shared_instance: Optional["OpenGLContext"] = None

    def __init__(self, gpu_limited_access) -> None:
        # Reuse the cdylib the limited-access view has already loaded —
        # `slpn_opengl_*` symbols are wired up alongside `slpn_surface_*`
        # in `processor_context.load_native_lib`.
        self._lib = gpu_limited_access.native_lib
        self._gpu = gpu_limited_access
        rt = self._lib.slpn_opengl_runtime_new()
        if not rt:
            raise RuntimeError(
                "OpenGLContext: slpn_opengl_runtime_new returned NULL — the "
                "subprocess could not bring up an EGL display + GL context. "
                "Check that libEGL.so.1 is installed and the driver supports "
                "EGL_EXT_image_dma_buf_import_modifiers."
            )
        self._rt = ctypes.c_void_p(rt)
        # Map host pool_id (UUID) → local u64 surface_id.
        self._surface_ids: dict[str, int] = {}
        # Map host pool_id (UUID) → GL target the surface was registered
        # under (`GL_TEXTURE_2D` or `GL_TEXTURE_EXTERNAL_OES`). Used so
        # acquire_*_external_oes can refuse a surface registered under
        # the 2D path and vice versa, instead of silently mismatching
        # the customer's GLSL.
        self._surface_targets: dict[str, int] = {}
        # Pin the resolved gpu surface handles for the runtime's lifetime so
        # the underlying DMA-BUF FDs stay alive — the Rust adapter dups them
        # via EGL on register, but holding the handle Python-side keeps the
        # unlock/release contract consistent.
        self._resolved_handles: dict[str, object] = {}

    @classmethod
    def from_runtime(cls, runtime_context) -> "OpenGLContext":
        """Build (or fetch the cached) :class:`OpenGLContext` for this
        subprocess.

        The subprocess hosts at most one EGL display + GL context — calling
        this twice with the same runtime returns the same instance.
        """
        if cls._shared_instance is None:
            cls._shared_instance = cls(runtime_context.gpu_limited_access)
        return cls._shared_instance

    def _resolve_and_register(
        self, pool_id: str, target: int = GL_TEXTURE_2D
    ) -> int:
        """Resolve `pool_id` via surface-share, register with the OpenGL
        adapter under the requested GL `target`, and return the local
        u64 surface_id. Idempotent — repeat calls with the same `target`
        return the cached id; calls with a different `target` raise
        :class:`RuntimeError` (the registration target is fixed at
        first-call time and the underlying `SurfaceState` only carries
        one binding)."""
        cached = self._surface_ids.get(pool_id)
        if cached is not None:
            cached_target = self._surface_targets.get(pool_id)
            if cached_target != target:
                raise RuntimeError(
                    f"OpenGLContext: surface '{pool_id}' was already registered "
                    f"under target 0x{cached_target:04X}; refusing to "
                    f"re-register under target 0x{target:04X}. Use the "
                    f"matching acquire method (acquire_read[_external_oes])."
                )
            return cached
        handle = self._gpu.resolve_surface(pool_id)
        # Adapter pulls the underlying *mut SurfaceHandle pointer out of the
        # SDK's NativeGpuSurfaceHandle — see streamlib.gpu_surface for the
        # shape. Public accessor on the SDK handle exposes the raw FFI
        # pointer for adapter integration.
        handle_ptr = handle.native_handle_ptr
        if not handle_ptr:
            raise RuntimeError(
                f"OpenGLContext: resolve_surface('{pool_id}') returned a handle "
                "with a null native pointer"
            )
        surface_id = next(_SURFACE_ID_COUNTER)
        if target == GL_TEXTURE_EXTERNAL_OES:
            register_fn = self._lib.slpn_opengl_register_external_oes_surface
            register_name = "register_external_oes_surface"
        elif target == GL_TEXTURE_2D:
            register_fn = self._lib.slpn_opengl_register_surface
            register_name = "register_surface"
        else:
            raise ValueError(
                f"OpenGLContext: unsupported registration target 0x{target:04X} "
                f"(expected GL_TEXTURE_2D or GL_TEXTURE_EXTERNAL_OES)"
            )
        rc = register_fn(
            self._rt,
            ctypes.c_uint64(surface_id),
            ctypes.c_void_p(handle_ptr),
        )
        if rc != 0:
            raise RuntimeError(
                f"OpenGLContext: {register_name} failed for pool_id "
                f"'{pool_id}' (rc={rc}). Check the subprocess log for "
                "EGL/DMA-BUF import errors — typically a wrong DRM modifier "
                "or an unsupported pixel format."
            )
        self._surface_ids[pool_id] = surface_id
        self._surface_targets[pool_id] = target
        # Hold the SDK handle so its FDs stay alive for the runtime's life.
        self._resolved_handles[pool_id] = handle
        return surface_id

    @staticmethod
    def _surface_pool_id(surface) -> str:
        """Extract the surface-share pool id (string UUID) from either a
        `StreamlibSurface`-shaped object or a bare string."""
        if isinstance(surface, str):
            return surface
        sid = getattr(surface, "id", None)
        if sid is None:
            raise TypeError(
                f"OpenGLContext: expected StreamlibSurface or str pool_id, got {surface!r}"
            )
        return str(sid)

    @contextmanager
    def acquire_write(
        self, surface
    ) -> "Iterator[OpenGLWriteView]":
        """Acquire write access. The adapter's EGL context is current on
        the calling thread for the scope; ``view.gl_texture_id`` is a
        ``GL_TEXTURE_2D`` valid in that context.

        On scope exit the adapter drains GL (`glFinish`) so cross-API
        consumers see the writes through the underlying DMA-BUF.
        """
        pool_id = self._surface_pool_id(surface)
        surface_id = self._resolve_and_register(pool_id)
        texture_id = int(
            self._lib.slpn_opengl_acquire_write(self._rt, ctypes.c_uint64(surface_id))
        )
        if texture_id == 0:
            raise RuntimeError(
                f"OpenGLContext.acquire_write: slpn_opengl_acquire_write "
                f"returned 0 for surface '{pool_id}' (contention or "
                "EGL/GL failure — check the subprocess log)"
            )
        try:
            yield OpenGLWriteView(gl_texture_id=texture_id)
        finally:
            self._lib.slpn_opengl_release_write(
                self._rt, ctypes.c_uint64(surface_id)
            )

    @contextmanager
    def acquire_read(
        self, surface
    ) -> "Iterator[OpenGLReadView]":
        """Acquire read access — same shape as :meth:`acquire_write`,
        but the resulting texture is sample-only (multiple readers may
        coexist; no writer can be active)."""
        pool_id = self._surface_pool_id(surface)
        surface_id = self._resolve_and_register(pool_id, GL_TEXTURE_2D)
        texture_id = int(
            self._lib.slpn_opengl_acquire_read(self._rt, ctypes.c_uint64(surface_id))
        )
        if texture_id == 0:
            raise RuntimeError(
                f"OpenGLContext.acquire_read: slpn_opengl_acquire_read "
                f"returned 0 for surface '{pool_id}'"
            )
        try:
            yield OpenGLReadView(
                gl_texture_id=texture_id, target=GL_TEXTURE_2D,
            )
        finally:
            self._lib.slpn_opengl_release_read(
                self._rt, ctypes.c_uint64(surface_id)
            )

    @contextmanager
    def acquire_read_external_oes(
        self, surface
    ) -> "Iterator[OpenGLReadView]":
        """Acquire read access against a surface registered under
        :data:`GL_TEXTURE_EXTERNAL_OES` — the path for sampler-only
        DMA-BUFs (camera ring textures, linear surfaces on NVIDIA per
        ``docs/learnings/nvidia-egl-dmabuf-render-target.md``).

        First call for a `pool_id` registers the surface as
        EXTERNAL_OES; subsequent calls return the same texture id.
        Mixing this with :meth:`acquire_read` for the same `pool_id`
        is rejected (one binding target per registration).

        The returned ``view.target`` is :data:`GL_TEXTURE_EXTERNAL_OES`
        — ModernGL's ``external_texture`` does not accept this target,
        so customers bind manually via raw PyOpenGL or ctypes:

        .. code-block:: python

            from OpenGL import GL
            with ctx.acquire_read_external_oes(surface) as view:
                GL.glActiveTexture(GL.GL_TEXTURE0)
                GL.glBindTexture(view.target, view.gl_texture_id)
                # ... draw with samplerExternalOES shader ...

        The shader must declare
        ``#extension GL_OES_EGL_image_external_essl3 : require`` (or
        ``GL_OES_EGL_image_external`` for older GLSL profiles) and
        sample the texture via ``samplerExternalOES``.
        """
        pool_id = self._surface_pool_id(surface)
        surface_id = self._resolve_and_register(pool_id, GL_TEXTURE_EXTERNAL_OES)
        texture_id = int(
            self._lib.slpn_opengl_acquire_read(self._rt, ctypes.c_uint64(surface_id))
        )
        if texture_id == 0:
            raise RuntimeError(
                f"OpenGLContext.acquire_read_external_oes: "
                f"slpn_opengl_acquire_read returned 0 for surface '{pool_id}'"
            )
        try:
            yield OpenGLReadView(
                gl_texture_id=texture_id,
                target=GL_TEXTURE_EXTERNAL_OES,
            )
        finally:
            self._lib.slpn_opengl_release_read(
                self._rt, ctypes.c_uint64(surface_id)
            )


