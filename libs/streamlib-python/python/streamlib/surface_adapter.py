# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Python mirror of streamlib_adapter_abi.

Provides:
  - StreamlibSurface, SurfaceFormat, SurfaceUsage, AccessMode (Protocols
    and IntFlag/IntEnum types matching the Rust shape).
  - _SurfaceTransportHandleC, _SurfaceSyncStateC, _StreamlibSurfaceC
    ctypes mirrors locked to the Rust #[repr(C)] layout. Twin tests in
    libs/streamlib-adapter-abi/src/surface.rs (Rust) and
    tests/test_surface_adapter.py (Python) enforce that the offsets
    match — both must be updated in lockstep.
  - SurfaceAdapter Protocol — the trait shape Python adapter authors
    implement against (context-manager `acquire_read` / `acquire_write`
    that hand back framework-native views).

Adapters live in dedicated packages (e.g. streamlib_adapter_vulkan_py);
this module is the contract they implement.
"""

from __future__ import annotations

import ctypes
import enum
from typing import Iterator, Protocol, runtime_checkable

# ABI version major. Subprocess SDK refuses adapters with a different
# major. Mirrors STREAMLIB_ADAPTER_ABI_VERSION in lib.rs.
STREAMLIB_ADAPTER_ABI_VERSION: int = 1

# Maximum DMA-BUF planes the descriptor carries — mirrors the Rust
# constant of the same name.
MAX_DMA_BUF_PLANES: int = 4


class SurfaceFormat(enum.IntEnum):
    """Mirror of Rust `SurfaceFormat` (`#[repr(u32)]`)."""

    BGRA8 = 0
    RGBA8 = 1
    NV12 = 2


class SurfaceUsage(enum.IntFlag):
    """Mirror of Rust `SurfaceUsage` (`bitflags!` `#[repr(transparent)] u32`)."""

    RENDER_TARGET = 1 << 0
    SAMPLED = 1 << 1
    CPU_READBACK = 1 << 2


class AccessMode(enum.IntEnum):
    """Wire-format access mode used by the IPC and polyglot mirrors."""

    READ = 0
    WRITE = 1


# ---------------------------------------------------------------------------
# ctypes mirrors of the #[repr(C)] descriptor types
# ---------------------------------------------------------------------------


class _SurfaceTransportHandleC(ctypes.Structure):
    """Mirror of Rust `SurfaceTransportHandle`."""

    _fields_ = [
        ("plane_count", ctypes.c_uint32),
        ("dma_buf_fds", ctypes.c_int32 * MAX_DMA_BUF_PLANES),
        ("plane_offsets", ctypes.c_uint64 * MAX_DMA_BUF_PLANES),
        ("plane_strides", ctypes.c_uint64 * MAX_DMA_BUF_PLANES),
        ("drm_format_modifier", ctypes.c_uint64),
    ]


class _SurfaceSyncStateC(ctypes.Structure):
    """Mirror of Rust `SurfaceSyncState`.

    Subprocess adapters wait/signal via the imported `SYNC_FD`
    (`vkImportSemaphoreFdKHR`) — they cannot dereference
    `timeline_semaphore_handle` (a host-side `VkSemaphore`).
    """

    _fields_ = [
        ("timeline_semaphore_handle", ctypes.c_uint64),
        ("timeline_semaphore_sync_fd", ctypes.c_int32),
        ("_pad_a", ctypes.c_uint32),
        ("last_acquire_value", ctypes.c_uint64),
        ("last_release_value", ctypes.c_uint64),
        ("current_image_layout", ctypes.c_int32),
        ("_pad_b", ctypes.c_uint32),
        ("_reserved", ctypes.c_uint8 * 16),
    ]


class _StreamlibSurfaceC(ctypes.Structure):
    """Mirror of Rust `StreamlibSurface`."""

    _fields_ = [
        ("id", ctypes.c_uint64),
        ("width", ctypes.c_uint32),
        ("height", ctypes.c_uint32),
        ("format", ctypes.c_uint32),
        ("usage", ctypes.c_uint32),
        ("transport", _SurfaceTransportHandleC),
        ("sync", _SurfaceSyncStateC),
    ]


# ---------------------------------------------------------------------------
# Adapter trait shape (Protocols)
# ---------------------------------------------------------------------------


@runtime_checkable
class StreamlibSurface(Protocol):
    """Customer-visible view of a `StreamlibSurface` descriptor.

    Adapter implementations may also expose the underlying ctypes
    struct (`_StreamlibSurfaceC`) for FFI calls — keep that path
    private to the adapter, customers never see it.
    """

    id: int
    width: int
    height: int
    format: int
    usage: int


@runtime_checkable
class SurfaceAdapter(Protocol):
    """Public ABI for a Python streamlib surface adapter.

    Implementations expose scoped `acquire_read` / `acquire_write` as
    context managers — the customer writes:

        with adapter.acquire_write(surface) as view:
            view.draw_into(...)

    and never types the word "semaphore". The adapter handles all
    timeline-semaphore signaling on `__enter__` / `__exit__`.

    Two acquisition flavors mirror the Rust trait:

    - Blocking `acquire_read` / `acquire_write` — the context manager
      blocks on `__enter__` until the timeline semaphore wait
      completes (and, for write, until any contended reader/writer
      releases).
    - Non-blocking `try_acquire_read` / `try_acquire_write` — return
      `None` immediately if the surface is contended; never block.
      Right shape for processor-graph nodes that must not stall.
    """

    def acquire_read(self, surface: StreamlibSurface) -> "Iterator[object]":
        """Return a context manager handing out a read view."""

    def acquire_write(self, surface: StreamlibSurface) -> "Iterator[object]":
        """Return a context manager handing out a write view."""

    def try_acquire_read(
        self, surface: StreamlibSurface
    ) -> "Iterator[object] | None":
        """Return a context manager handing out a read view, or None if
        a writer currently holds the surface."""

    def try_acquire_write(
        self, surface: StreamlibSurface
    ) -> "Iterator[object] | None":
        """Return a context manager handing out a write view, or None
        if any reader or another writer currently holds the surface."""
