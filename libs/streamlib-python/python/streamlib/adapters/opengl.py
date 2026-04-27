# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""OpenGL/EGL surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-opengl`` (#512). The
subprocess's actual EGL+GL handling lives in the runtime's native
binding; this module provides:

  * Typed views the subprocess sees inside ``acquire_*`` scopes —
    ``OpenGLReadView`` / ``OpenGLWriteView`` exposing a single
    integer ``gl_texture_id`` and the constant ``target =
    GL_TEXTURE_2D``.
  * An ``OpenGLContext`` Protocol the subprocess runtime implements —
    customers call ``with ctx.acquire_write(surface) as view:`` and
    bind ``view.gl_texture_id`` to their PyOpenGL / ModernGL stack.

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

import os
from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import Optional, Protocol, runtime_checkable

from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    StreamlibSurface,
)

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "GL_TEXTURE_2D",
    "OpenGLReadView",
    "OpenGLWriteView",
    "OpenGLSurfaceAdapter",
    "OpenGLContext",
    "configure_pyopengl_for_streamlib_subprocess",
]


# `GL_TEXTURE_2D` enumerant — re-exported so customers don't have to
# import a GL binding just to compare `view.target`. Matches the
# Rust crate's `GL_TEXTURE_2D` constant.
GL_TEXTURE_2D: int = 0x0DE1


@dataclass(frozen=True)
class OpenGLReadView:
    """View handed back inside an ``acquire_read`` scope.

    ``gl_texture_id`` is an integer the customer feeds into PyOpenGL
    / ModernGL: ``glBindTexture(GL_TEXTURE_2D, view.gl_texture_id)``.
    The surface is sample-able for the guard's lifetime; the runtime
    handles all underlying EGL/DMA-BUF plumbing.
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
class OpenGLContext(Protocol):
    """Customer-facing handle the subprocess runtime hands out.

    Equivalent shape to the Rust ``OpenGlContext`` — thin wrapper
    over an ``OpenGLSurfaceAdapter`` so customer code can write::

        with ctx.acquire_write(surface) as view:
            do_gl_work(view.gl_texture_id)

    The customer never types the words "DMA-BUF" or "modifier."
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


