# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Explicit GPU→CPU surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-cpu-readback`` (#514, #533).
The subprocess's actual GPU→CPU copy is performed by the host (the
adapter runs in-process on the host and issues
``vkCmdCopyImageToBuffer`` against per-plane HOST_VISIBLE staging
buffers). This module provides the type shapes a Python customer
programs against:

  * ``CpuReadbackPlaneView`` / ``CpuReadbackPlaneViewMut`` — per-plane
    byte slices and dimensions. For BGRA/RGBA there's exactly one
    plane; for NV12 there are two (Y at index 0, UV at index 1).
  * ``CpuReadbackReadView`` / ``CpuReadbackWriteView`` — surface-level
    metadata plus the tuple of plane views the customer sees inside
    ``acquire_read`` / ``acquire_write`` scopes. Each plane view
    exposes ``bytes`` (a ``bytes`` slice or ``memoryview``) and
    ``numpy`` (a ``numpy.ndarray`` aliasing the same memory).
  * ``CpuReadbackContext`` Protocol — the subprocess runtime
    implements this and hands a customer-facing context out.
  * Acquire-time logging line ``cpu-readback: GPU→CPU copy of NxN
    {format} surface, M bytes total (P planes)`` — emitted by the host
    adapter so customers see they paid for the copy.

This is the **single sanctioned CPU exit** in the surface-adapter
architecture. GPU adapters (``streamlib.adapters.vulkan`` /
``opengl`` / ``skia``) deliberately do not expose CPU bytes —
switching to this adapter is the contractual signal that you've
opted in to a host-side GPU→CPU roundtrip. Do not use this in
performance-critical pipelines; the copy is per-acquire and blocks
on ``vkQueueWaitIdle``.
"""

from __future__ import annotations

from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import TYPE_CHECKING, Optional, Protocol, Tuple, runtime_checkable

from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    StreamlibSurface,
    SurfaceFormat,
)

if TYPE_CHECKING:  # pragma: no cover - type-only import
    import numpy as np

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "CpuReadbackPlaneView",
    "CpuReadbackPlaneViewMut",
    "CpuReadbackReadView",
    "CpuReadbackWriteView",
    "CpuReadbackSurfaceAdapter",
    "CpuReadbackContext",
]


@dataclass(frozen=True)
class CpuReadbackPlaneView:
    """Read-only view of a single plane of an acquired surface.

    Exposes ``bytes`` (a read-only ``memoryview``) and ``numpy`` (a
    ``numpy.ndarray`` aliasing the same memory; shape
    ``(height, width, bytes_per_pixel)``, dtype ``numpy.uint8``).
    Plane dimensions are in plane texels — for NV12's UV plane that
    means half the surface width × half the surface height.
    """

    width: int
    height: int
    bytes_per_pixel: int
    bytes: memoryview
    numpy: "np.ndarray"

    @property
    def row_stride(self) -> int:
        """Tightly-packed row stride in bytes
        (``width * bytes_per_pixel``)."""
        return self.width * self.bytes_per_pixel


@dataclass(frozen=True)
class CpuReadbackPlaneViewMut:
    """Mutable view of a single plane of an acquired surface.

    ``bytes`` is a writable ``memoryview``; ``numpy`` is a
    ``numpy.ndarray`` aliasing the same memory. Edits to any plane are
    flushed back to the host ``VkImage`` via per-plane
    ``vkCmdCopyBufferToImage`` on guard drop.
    """

    width: int
    height: int
    bytes_per_pixel: int
    bytes: memoryview
    numpy: "np.ndarray"

    @property
    def row_stride(self) -> int:
        return self.width * self.bytes_per_pixel


@dataclass(frozen=True)
class CpuReadbackReadView:
    """View handed back inside an ``acquire_read`` scope.

    ``planes`` is a tuple of [`CpuReadbackPlaneView`] in declaration
    order — for NV12 that's ``(Y, UV)``. ``plane_count`` reflects the
    surface's [`SurfaceFormat`]: 1 for BGRA8/RGBA8, 2 for NV12.
    """

    width: int
    height: int
    format: SurfaceFormat
    planes: Tuple[CpuReadbackPlaneView, ...]

    @property
    def plane_count(self) -> int:
        return len(self.planes)

    def plane(self, index: int) -> CpuReadbackPlaneView:
        """Borrow plane ``index``. Raises ``IndexError`` on out-of-range."""
        return self.planes[index]


@dataclass(frozen=True)
class CpuReadbackWriteView:
    """View handed back inside an ``acquire_write`` scope.

    Same shape as [`CpuReadbackReadView`] but each plane's ``bytes`` /
    ``numpy`` are mutable; on guard drop the modified bytes are
    flushed back to the host ``VkImage`` via per-plane
    ``vkCmdCopyBufferToImage``.
    """

    width: int
    height: int
    format: SurfaceFormat
    planes: Tuple[CpuReadbackPlaneViewMut, ...]

    @property
    def plane_count(self) -> int:
        return len(self.planes)

    def plane(self, index: int) -> CpuReadbackPlaneViewMut:
        """Borrow plane ``index``. Raises ``IndexError`` on out-of-range."""
        return self.planes[index]


@runtime_checkable
class CpuReadbackSurfaceAdapter(Protocol):
    """Protocol an in-process Python cpu-readback adapter implements."""

    def acquire_read(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[CpuReadbackReadView]: ...

    def acquire_write(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[CpuReadbackWriteView]: ...

    def try_acquire_read(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[CpuReadbackReadView]]: ...

    def try_acquire_write(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[CpuReadbackWriteView]]: ...


@runtime_checkable
class CpuReadbackContext(Protocol):
    """Customer-facing handle the subprocess runtime hands out.

    Equivalent shape to the Rust ``CpuReadbackContext`` — thin wrapper
    over a ``CpuReadbackSurfaceAdapter`` so customer code can write::

        with ctx.acquire_write(surface) as view:
            # Single-plane (BGRA): one entry in view.planes.
            arr = view.plane(0).numpy  # shape (H, W, 4), dtype uint8
            arr[..., :] = my_image     # mutate in-place; flushed on exit

        with ctx.acquire_write(nv12_surface) as view:
            # Multi-plane (NV12): view.plane_count == 2.
            y  = view.plane(0).numpy   # shape (H, W,   1)
            uv = view.plane(1).numpy   # shape (H/2, W/2, 2)
            y[...] = luma
            uv[...] = chroma_uv
    """

    def acquire_read(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[CpuReadbackReadView]: ...

    def acquire_write(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[CpuReadbackWriteView]: ...

    def try_acquire_read(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[CpuReadbackReadView]]: ...

    def try_acquire_write(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[CpuReadbackWriteView]]: ...
