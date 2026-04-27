# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Explicit GPU→CPU surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-cpu-readback`` (#514). The
subprocess's actual GPU→CPU copy is performed by the host (the
adapter runs in-process on the host and issues
``vkCmdCopyImageToBuffer`` against a HOST_VISIBLE staging buffer).
This module provides the type shapes a Python customer programs
against:

  * ``CpuReadbackReadView`` / ``CpuReadbackWriteView`` — views the
    customer sees inside ``acquire_read`` / ``acquire_write`` scopes.
    Both expose ``bytes`` (a tightly-packed ``bytes`` slice or
    ``memoryview``) and ``numpy`` (a ``numpy.ndarray`` view of the
    same memory, shape ``(height, width, bytes_per_pixel)``, dtype
    ``numpy.uint8``). Reads on either property are O(1) — the GPU→CPU
    copy already happened at acquire time.
  * ``CpuReadbackContext`` Protocol — the subprocess runtime
    implements this and hands a customer-facing context out.
  * Acquire-time logging line ``cpu-readback: GPU→CPU copy of NxN
    surface, M bytes`` — emitted by the host adapter so customers see
    they paid for the copy.

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
from typing import TYPE_CHECKING, Optional, Protocol, runtime_checkable

from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    StreamlibSurface,
)

if TYPE_CHECKING:  # pragma: no cover - type-only import
    import numpy as np

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "CpuReadbackReadView",
    "CpuReadbackWriteView",
    "CpuReadbackSurfaceAdapter",
    "CpuReadbackContext",
]


@dataclass(frozen=True)
class CpuReadbackReadView:
    """View handed back inside an ``acquire_read`` scope.

    ``bytes`` is a read-only ``memoryview`` over the
    ``width * height * bytes_per_pixel`` staging buffer. ``numpy`` is a
    ``numpy.ndarray`` aliasing the same memory (no extra copy); shape
    ``(height, width, bytes_per_pixel)``, dtype ``numpy.uint8``.
    """

    width: int
    height: int
    bytes_per_pixel: int
    bytes: memoryview
    numpy: "np.ndarray"

    @property
    def row_stride(self) -> int:
        """Row stride in bytes — always tightly packed
        (``width * bytes_per_pixel``)."""
        return self.width * self.bytes_per_pixel


@dataclass(frozen=True)
class CpuReadbackWriteView:
    """View handed back inside an ``acquire_write`` scope.

    ``bytes`` is a writable ``memoryview``; ``numpy`` is a
    ``numpy.ndarray`` aliasing the same memory. Edits are flushed back
    to the host ``VkImage`` via ``vkCmdCopyBufferToImage`` on guard
    drop.
    """

    width: int
    height: int
    bytes_per_pixel: int
    bytes: memoryview
    numpy: "np.ndarray"

    @property
    def row_stride(self) -> int:
        return self.width * self.bytes_per_pixel


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
            arr = view.numpy  # shape (H, W, 4), dtype uint8
            arr[..., :] = my_image  # mutate in-place; flushed on exit
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
