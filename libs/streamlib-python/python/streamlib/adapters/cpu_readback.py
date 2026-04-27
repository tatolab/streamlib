# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Explicit GPU→CPU surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-cpu-readback`` (#514, #529,
#533). The subprocess's actual GPU→CPU copy is performed by the host
(the adapter runs in-process on the host and issues
``vkCmdCopyImageToBuffer`` against per-plane HOST_VISIBLE staging
buffers). This module provides the customer-facing shapes:

  * ``CpuReadbackPlaneView`` / ``CpuReadbackPlaneViewMut`` — per-plane
    byte slices and dimensions. For BGRA/RGBA there's exactly one
    plane; for NV12 there are two (Y at index 0, UV at index 1).
  * ``CpuReadbackReadView`` / ``CpuReadbackWriteView`` — surface-level
    metadata plus the tuple of plane views the customer sees inside
    ``acquire_read`` / ``acquire_write`` scopes. Each plane view
    exposes ``bytes`` (a ``memoryview``) and ``numpy`` (a
    ``numpy.ndarray`` aliasing the same memory).
  * ``CpuReadbackContext`` — concrete subprocess runtime. Wraps an
    escalate channel (for the host-side ``acquire`` / release op)
    and a ``gpu_limited_access`` (for ``check_out`` of each plane's
    staging buffer FD). Customers obtain one via the SDK; tests
    construct one directly with ``CpuReadbackContext.from_runtime``.
  * Acquire-time logging line ``cpu-readback: GPU→CPU copy of NxN
    {format} surface, M bytes total (P planes)`` — emitted by the host
    adapter so customers see they paid for the copy.

This is the **single sanctioned CPU exit** in the surface-adapter
architecture. GPU adapters (``streamlib.adapters.vulkan`` /
``opengl`` / ``skia``) deliberately do not expose CPU bytes —
switching to this adapter is the contractual signal that you've
opted in to a host-side GPU→CPU roundtrip. Do not use this in
performance-critical pipelines; the copy is per-acquire and the
host blocks on a per-submit fence.
"""

from __future__ import annotations

import ctypes
from contextlib import contextmanager
from dataclasses import dataclass
from typing import TYPE_CHECKING, Iterator, Optional, Tuple

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


_FORMAT_FROM_WIRE = {
    "bgra8": SurfaceFormat.BGRA8,
    "rgba8": SurfaceFormat.RGBA8,
    "nv12": SurfaceFormat.NV12,
}


def _surface_id_from(surface) -> int:
    """Extract the host-assigned surface_id from a StreamlibSurface
    Protocol object or an int."""
    if isinstance(surface, int):
        return surface
    sid = getattr(surface, "id", None)
    if sid is None:
        raise TypeError(
            f"CpuReadbackContext: expected StreamlibSurface or int, got {surface!r}"
        )
    return int(sid)


def _format_from_wire(wire: Optional[str]) -> SurfaceFormat:
    if wire is None:
        # Defensive: the host always sets `format` on
        # acquire_cpu_readback responses, but if a buggy host omits it
        # we fall back to BGRA8 — the most common single-plane shape.
        return SurfaceFormat.BGRA8
    fmt = _FORMAT_FROM_WIRE.get(wire.lower())
    if fmt is None:
        raise ValueError(f"unknown cpu-readback surface format on the wire: {wire!r}")
    return fmt


def _build_plane_view_pair(
    handle, width: int, height: int, bytes_per_pixel: int
) -> Tuple["memoryview", "np.ndarray"]:
    """Construct a `(memoryview, ndarray)` aliasing the locked staging
    buffer's bytes. Both views share the same backing kernel mmap.

    `handle` is a `NativeGpuSurfaceHandle` already locked for the
    requested mode. Caller is responsible for ``unlock()`` after the
    context exits.
    """
    import numpy as np

    base = handle.base_address
    if not base:
        raise RuntimeError(
            "cpu-readback: staging surface base address is null after lock — "
            "the host's surface-share registration may be stale"
        )
    byte_size = handle.bytes_per_row * height
    raw = (ctypes.c_uint8 * byte_size).from_address(base)
    mv = memoryview(raw).cast("B")
    # Plane shape: (height, width, bytes_per_pixel). For NV12 UV the
    # caller sees (H/2, W/2, 2) — half-width × half-height ×
    # 2 bpp interleaved. Strides match the staging buffer's
    # tightly-packed row pitch (= width * bpp = bytes_per_row).
    arr = np.ndarray(
        shape=(height, width, bytes_per_pixel),
        dtype=np.uint8,
        buffer=raw,
        strides=(handle.bytes_per_row, bytes_per_pixel, 1),
    )
    return mv, arr


class CpuReadbackContext:
    """Customer-facing handle bound to the subprocess's escalate channel
    and surface-share client.

    Customers obtain one via the SDK's ``adapters`` factory; tests build
    one directly from a ``gpu_limited_access`` and an ``EscalateChannel``
    via :meth:`from_runtime`.

    Use as a context manager::

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

    def __init__(self, gpu_limited_access, escalate_channel) -> None:
        self._gpu = gpu_limited_access
        self._escalate = escalate_channel

    @classmethod
    def from_runtime(cls, runtime_context) -> "CpuReadbackContext":
        """Build from a typed runtime context (limited or full access).

        The runtime exposes ``gpu_limited_access`` for surface-share
        lookups; the escalate channel is the process-wide singleton
        installed by the subprocess runner.
        """
        from streamlib.escalate import channel as _escalate_channel

        return cls(runtime_context.gpu_limited_access, _escalate_channel())

    @contextmanager
    def acquire_read(
        self, surface
    ) -> "Iterator[CpuReadbackReadView]":
        """Block until the host has copied the surface into staging
        buffers, hand back a read view, and on exit release the host's
        adapter guard so the timeline can advance."""
        with self._acquire(surface, mode="read", writable=False) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def acquire_write(
        self, surface
    ) -> "Iterator[CpuReadbackWriteView]":
        """Block until the host has copied the surface into staging
        buffers, hand back a write view, and on exit (a) flush mutated
        bytes back into the host's `VkImage` via
        `vkCmdCopyBufferToImage`, then (b) release the adapter guard
        so the timeline release-value signals."""
        with self._acquire(surface, mode="write", writable=True) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def try_acquire_read(self, surface) -> "Iterator[Optional[CpuReadbackReadView]]":
        """Non-blocking read acquire. Yields a [`CpuReadbackReadView`] on
        success or ``None`` on contention (another writer holds the
        surface). Customers should pattern: ::

            with ctx.try_acquire_read(surface) as view:
                if view is None:
                    return  # skip this frame
                ...
        """
        with self._acquire(surface, mode="read", writable=False, blocking=False) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def try_acquire_write(self, surface) -> "Iterator[Optional[CpuReadbackWriteView]]":
        """Non-blocking write acquire. Yields a [`CpuReadbackWriteView`]
        on success or ``None`` on contention. See :meth:`try_acquire_read`
        for the pattern."""
        with self._acquire(surface, mode="write", writable=True, blocking=False) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def _acquire(
        self, surface, mode: str, writable: bool, blocking: bool = True,
    ) -> "Iterator[object]":
        surface_id = _surface_id_from(surface)
        if blocking:
            response = self._escalate.acquire_cpu_readback(surface_id, mode)
        else:
            response = self._escalate.try_acquire_cpu_readback(surface_id, mode)
        if response is None:
            # Contended: host registered nothing, customer has nothing
            # to release. Yield None so `with` callers can skip the
            # frame without distinguishing "host-said-no" from
            # "happy-path acquire" via exception-handling.
            yield None
            return

        handle_id = response["handle_id"]

        try:
            yield from self._build_view(response, writable)
        finally:
            # Always release the host-side adapter guard. Releases happen
            # in reverse order of acquire — surface-share unmap+release
            # for each plane (handled by the per-handle release in the
            # try block above; if we got here without yielding, the
            # plane handles weren't constructed yet) plus the final
            # adapter-guard release via release_handle.
            self._escalate.release_handle(handle_id)

    def _build_view(self, response, writable: bool):
        """Resolve each plane's staging buffer, lock it, build a view,
        and yield the assembled read/write view to the caller. Surface
        unmaps and unlocks happen in reverse order in the finally
        block."""
        format_ = _format_from_wire(response.get("format"))
        width = int(response.get("width", 0))
        height = int(response.get("height", 0))
        plane_descriptors = response.get("cpu_readback_planes") or []
        if not plane_descriptors:
            raise RuntimeError(
                "cpu-readback acquire response missing cpu_readback_planes"
            )

        locked_handles = []
        plane_views = []
        try:
            for descriptor in plane_descriptors:
                staging_id = descriptor["staging_surface_id"]
                pwidth = int(descriptor["width"])
                pheight = int(descriptor["height"])
                pbpp = int(descriptor["bytes_per_pixel"])
                handle = self._gpu.resolve_surface(staging_id)
                handle.lock(read_only=not writable)
                locked_handles.append(handle)
                mv, arr = _build_plane_view_pair(
                    handle, pwidth, pheight, pbpp
                )
                if writable:
                    plane_views.append(
                        CpuReadbackPlaneViewMut(
                            width=pwidth,
                            height=pheight,
                            bytes_per_pixel=pbpp,
                            bytes=mv,
                            numpy=arr,
                        )
                    )
                else:
                    plane_views.append(
                        CpuReadbackPlaneView(
                            width=pwidth,
                            height=pheight,
                            bytes_per_pixel=pbpp,
                            bytes=mv,
                            numpy=arr,
                        )
                    )

            if writable:
                view = CpuReadbackWriteView(
                    width=width,
                    height=height,
                    format=format_,
                    planes=tuple(plane_views),
                )
            else:
                view = CpuReadbackReadView(
                    width=width,
                    height=height,
                    format=format_,
                    planes=tuple(plane_views),
                )
            yield view
        finally:
            # Release every plane handle we managed to lock — even on
            # partial failure. Reverse order matches acquire order.
            for handle in reversed(locked_handles):
                try:
                    handle.unlock(read_only=not writable)
                except Exception:
                    # Best-effort: the host's release_handle in the
                    # outer finally still runs and clears the underlying
                    # surface-share entries.
                    pass
                try:
                    handle.release()
                except Exception:
                    pass
