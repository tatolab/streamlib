# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Explicit GPU→CPU surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-cpu-readback`` (#562, Path E
single-pattern shape). The subprocess delegates to
``streamlib-python-native``'s ``slpn_cpu_readback_*`` FFI surface, which
itself wraps the host adapter crate's
``CpuReadbackSurfaceAdapter<ConsumerVulkanDevice>`` against a
subprocess-local Vulkan device. Per-acquire control flow:

1. The Python SDK looks the host's pre-registered cpu-readback surface
   up via surface-share once (``slpn_cpu_readback_register_surface``)
   — that's where the staging-buffer DMA-BUF FDs and the timeline
   OPAQUE_FD enter the cdylib's address space, get imported through
   ``ConsumerVulkanPixelBuffer`` / ``ConsumerVulkanTimelineSemaphore``,
   and stay alive for the surface's lifetime.
2. On every ``acquire_read`` / ``acquire_write`` the cdylib's adapter
   calls back into a Python-installed trigger callback that sends a
   ``run_cpu_readback_copy`` escalate-IPC request to the host. The
   host runs ``vkCmdCopyImageToBuffer`` (or its inverse on write
   release), signals the shared timeline, and replies with the new
   timeline value. The cdylib then waits on the imported timeline
   through the carve-out and hands the customer mapped pointers into
   the staging buffers as ``memoryview`` / ``numpy.ndarray``.

Customer-facing shapes:

  * ``CpuReadbackPlaneView`` / ``CpuReadbackPlaneViewMut`` — per-plane
    byte slices and dimensions. For BGRA/RGBA there's exactly one
    plane; for NV12 there are two (Y at index 0, UV at index 1).
  * ``CpuReadbackReadView`` / ``CpuReadbackWriteView`` — surface-level
    metadata plus the tuple of plane views the customer sees inside
    ``acquire_read`` / ``acquire_write`` scopes. Each plane view
    exposes ``bytes`` (a ``memoryview``) and ``numpy`` (a
    ``numpy.ndarray`` aliasing the same memory).
  * ``CpuReadbackContext`` — concrete subprocess runtime. One per
    subprocess; obtain via :meth:`CpuReadbackContext.from_runtime`.

This is the **single sanctioned CPU exit** in the surface-adapter
architecture. GPU adapters (``streamlib.adapters.vulkan`` /
``opengl`` / ``skia``) deliberately do not expose CPU bytes —
switching to this adapter is the contractual signal that you've
opted in to a host-side GPU→CPU roundtrip. Do not use this in
performance-critical pipelines; the copy is per-acquire and the
host blocks on a per-submit timeline wait.
"""

from __future__ import annotations

import ctypes
import itertools
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


# ``slpn_cpu_readback_*`` direction wire constants (must match the
# cdylib's ``SLPN_CPU_READBACK_DIRECTION_*`` and the ``run_cpu_readback_copy``
# escalate op direction tokens).
_DIRECTION_IMAGE_TO_BUFFER = 0
_DIRECTION_BUFFER_TO_IMAGE = 1

# Maximum planes the cdylib's view struct exposes. Matches
# ``SLPN_CPU_READBACK_MAX_PLANES`` in ``streamlib-python-native``.
_MAX_PLANES = 4

# ``slpn_cpu_readback_*`` return values.
_RC_OK = 0
_RC_CONTENDED = 1


# Surface-id namespace inside this subprocess. The host's pool_id
# (string) is mapped to a u64 the adapter uses internally; customers
# never see the u64 — they pass StreamlibSurface descriptors whose
# `.id` carries the same string.
_CPU_READBACK_SURFACE_ID_COUNTER = itertools.count(start=1)


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


# ---------------------------------------------------------------------------
# ctypes bindings to the cdylib's slpn_cpu_readback_* FFI surface.
# ---------------------------------------------------------------------------


class _SlpnCpuReadbackPlane(ctypes.Structure):
    """C struct matching ``streamlib_python_native::cpu_readback::SlpnCpuReadbackPlane``."""

    _fields_ = [
        ("mapped_ptr", ctypes.c_void_p),
        ("width", ctypes.c_uint32),
        ("height", ctypes.c_uint32),
        ("bytes_per_pixel", ctypes.c_uint32),
        ("byte_size", ctypes.c_uint64),
    ]


class _SlpnCpuReadbackView(ctypes.Structure):
    """C struct matching ``streamlib_python_native::cpu_readback::SlpnCpuReadbackView``."""

    _fields_ = [
        ("width", ctypes.c_uint32),
        ("height", ctypes.c_uint32),
        ("format", ctypes.c_uint32),
        ("plane_count", ctypes.c_uint32),
        ("planes", _SlpnCpuReadbackPlane * _MAX_PLANES),
    ]


# Trigger callback signature — must match
# ``SlpnCpuReadbackTriggerCallback`` in the cdylib. The cdylib expects
# the callback to RETURN the host's `timeline_value` as a `u64`. The
# host adapter's timelines start at 0 and only ever signal values >= 1,
# so 0 is the sentinel for "trigger failed".
_TriggerCallbackType = ctypes.CFUNCTYPE(
    ctypes.c_uint64,
    ctypes.c_void_p,   # user_data
    ctypes.c_uint64,   # surface_id
    ctypes.c_uint32,   # direction
)


def _surface_pool_id(surface) -> str:
    """Extract the surface-share pool id (string) from either a
    ``StreamlibSurface``-shaped object or a bare string / int."""
    if isinstance(surface, str):
        return surface
    if isinstance(surface, int):
        return str(surface)
    sid = getattr(surface, "id", None)
    if sid is None:
        raise TypeError(
            f"CpuReadbackContext: expected StreamlibSurface or pool_id, got {surface!r}"
        )
    return str(sid)


def _surface_format_from(surface) -> SurfaceFormat:
    """Best-effort SurfaceFormat extraction from a StreamlibSurface
    descriptor. Falls back to BGRA8 if the descriptor doesn't carry a
    format (rare — mostly bare-pool-id passthrough)."""
    fmt = getattr(surface, "format", None)
    if fmt is None:
        return SurfaceFormat.BGRA8
    if isinstance(fmt, SurfaceFormat):
        return fmt
    return SurfaceFormat(int(fmt))


# ---------------------------------------------------------------------------
# CpuReadbackContext — concrete subprocess runtime.
# ---------------------------------------------------------------------------


class CpuReadbackContext:
    """Customer-facing handle bound to the subprocess's cpu-readback
    cdylib runtime.

    Use as a context manager::

        with ctx.acquire_write(surface) as view:
            # Single-plane (BGRA): one entry in view.planes.
            arr = view.plane(0).numpy  # shape (H, W, 4), dtype uint8
            arr[..., :] = my_image     # mutate in-place; flushed on exit

        with ctx.acquire_write(nv12_surface) as view:
            # Multi-plane (NV12): view.plane_count == 2.
            y  = view.plane(0).numpy   # shape (H, W, 1)
            uv = view.plane(1).numpy   # shape (H/2, W/2, 2)
            y[...] = luma
            uv[...] = chroma_uv
    """

    _shared_instance: Optional["CpuReadbackContext"] = None

    def __init__(self, gpu_limited_access, escalate_channel) -> None:
        self._gpu = gpu_limited_access
        self._escalate = escalate_channel
        self._lib = gpu_limited_access.native_lib
        self._wire_signatures()
        rt = self._lib.slpn_cpu_readback_runtime_new()
        if not rt:
            raise RuntimeError(
                "CpuReadbackContext: slpn_cpu_readback_runtime_new returned NULL "
                "— the subprocess could not bring up a Vulkan device. Check that "
                "libvulkan.so.1 is installed and the driver supports "
                "VK_KHR_external_memory_fd, VK_EXT_external_memory_dma_buf, and "
                "VK_KHR_external_semaphore_fd."
            )
        self._rt = ctypes.c_void_p(rt)

        # Wire up the trigger callback. We must hold a reference to the
        # CFUNCTYPE wrapper for as long as the cdylib could call it
        # (i.e. until `_rt` is freed) — otherwise ctypes garbage-collects
        # the trampoline and the next callback invocation crashes.
        self._trigger_cb = _TriggerCallbackType(self._dispatch_trigger)
        rc = self._lib.slpn_cpu_readback_set_trigger_callback(
            self._rt, self._trigger_cb, ctypes.c_void_p(0)
        )
        if rc != 0:
            raise RuntimeError(
                f"CpuReadbackContext: set_trigger_callback returned {rc}"
            )

        # Map host pool_id (string) → local u64 surface_id.
        self._surface_ids: dict[str, int] = {}
        # Pin resolved surface-share handles so the imported DMA-BUF
        # plane fds stay alive for the runtime's lifetime. The cdylib
        # `register_surface` consumes the sync_fd; the plane fds remain
        # the SurfaceHandle's responsibility.
        self._resolved_handles: dict[str, object] = {}

    @classmethod
    def from_runtime(cls, runtime_context) -> "CpuReadbackContext":
        """Build (or fetch the cached) :class:`CpuReadbackContext` for
        this subprocess. The subprocess hosts at most one cpu-readback
        runtime — calling this twice with the same runtime returns the
        same instance.
        """
        if cls._shared_instance is None:
            from streamlib.escalate import channel as _escalate_channel

            cls._shared_instance = cls(
                runtime_context.gpu_limited_access, _escalate_channel()
            )
        return cls._shared_instance

    def _wire_signatures(self) -> None:
        lib = self._lib

        lib.slpn_cpu_readback_runtime_new.restype = ctypes.c_void_p
        lib.slpn_cpu_readback_runtime_new.argtypes = []

        lib.slpn_cpu_readback_runtime_free.restype = None
        lib.slpn_cpu_readback_runtime_free.argtypes = [ctypes.c_void_p]

        lib.slpn_cpu_readback_set_trigger_callback.restype = ctypes.c_int32
        lib.slpn_cpu_readback_set_trigger_callback.argtypes = [
            ctypes.c_void_p,
            _TriggerCallbackType,
            ctypes.c_void_p,
        ]

        lib.slpn_cpu_readback_register_surface.restype = ctypes.c_int32
        lib.slpn_cpu_readback_register_surface.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint64,
            ctypes.c_void_p,
            ctypes.c_uint32,
        ]

        lib.slpn_cpu_readback_unregister_surface.restype = ctypes.c_int32
        lib.slpn_cpu_readback_unregister_surface.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint64,
        ]

        for name in (
            "slpn_cpu_readback_acquire_read",
            "slpn_cpu_readback_acquire_write",
            "slpn_cpu_readback_try_acquire_read",
            "slpn_cpu_readback_try_acquire_write",
        ):
            fn = getattr(lib, name)
            fn.restype = ctypes.c_int32
            fn.argtypes = [
                ctypes.c_void_p,
                ctypes.c_uint64,
                ctypes.POINTER(_SlpnCpuReadbackView),
            ]

        for name in (
            "slpn_cpu_readback_release_read",
            "slpn_cpu_readback_release_write",
        ):
            fn = getattr(lib, name)
            fn.restype = ctypes.c_int32
            fn.argtypes = [ctypes.c_void_p, ctypes.c_uint64]

    def _dispatch_trigger(
        self,
        user_data,
        surface_id: int,
        direction: int,
    ) -> int:
        """Trigger callback — invoked from the cdylib on every acquire
        / write release. Sends a ``run_cpu_readback_copy`` escalate IPC
        and returns the host's timeline_value as a ``u64``.

        Returns the timeline value (>= 1) on success; returns 0 on
        failure (the cdylib propagates the failure as an
        AdapterError::IpcDisconnected back through the acquire path).
        """
        try:
            direction_str = (
                "image_to_buffer"
                if direction == _DIRECTION_IMAGE_TO_BUFFER
                else "buffer_to_image"
            )
            response = self._escalate.run_cpu_readback_copy(
                int(surface_id), direction_str
            )
            return int(response["timeline_value"])
        except Exception as e:  # pragma: no cover - logged on the host
            from streamlib import log

            log.error(
                "cpu-readback trigger callback failed",
                surface_id=int(surface_id),
                direction=int(direction),
                error=str(e),
            )
            return 0

    def _resolve_and_register(self, pool_id: str, format_: SurfaceFormat) -> int:
        """Resolve `pool_id` via surface-share, register with the
        cpu-readback adapter, and return the local u64 surface_id.
        Idempotent — repeat calls return the cached id."""
        cached = self._surface_ids.get(pool_id)
        if cached is not None:
            return cached
        handle = self._gpu.resolve_surface(pool_id)
        handle_ptr = handle.native_handle_ptr
        if not handle_ptr:
            raise RuntimeError(
                f"CpuReadbackContext: resolve_surface('{pool_id}') returned a "
                "handle with a null native pointer"
            )
        surface_id = next(_CPU_READBACK_SURFACE_ID_COUNTER)
        rc = self._lib.slpn_cpu_readback_register_surface(
            self._rt,
            ctypes.c_uint64(surface_id),
            ctypes.c_void_p(handle_ptr),
            ctypes.c_uint32(int(format_)),
        )
        if rc != 0:
            raise RuntimeError(
                f"CpuReadbackContext: register_surface failed for pool_id "
                f"'{pool_id}' (rc={rc}). Check the subprocess log — typically "
                "a missing sync_fd (host did not register the staging buffers "
                "with an exportable timeline) or a plane-count mismatch with "
                f"format {format_!r}."
            )
        self._surface_ids[pool_id] = surface_id
        self._resolved_handles[pool_id] = handle
        return surface_id

    @contextmanager
    def acquire_read(self, surface) -> "Iterator[CpuReadbackReadView]":
        """Block until the host has copied the surface into staging
        buffers, hand back a read view; on exit release the adapter
        guard so the timeline can advance."""
        with self._acquire(surface, write=False, blocking=True) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def acquire_write(self, surface) -> "Iterator[CpuReadbackWriteView]":
        """Block until the host has copied the surface into staging
        buffers, hand back a write view; on exit (a) flush mutated
        bytes back into the host ``VkImage`` via
        ``vkCmdCopyBufferToImage``, then (b) release the guard so the
        timeline release-value signals."""
        with self._acquire(surface, write=True, blocking=True) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def try_acquire_read(self, surface) -> "Iterator[Optional[CpuReadbackReadView]]":
        """Non-blocking read acquire. Yields a [`CpuReadbackReadView`]
        on success or ``None`` on contention.
        """
        with self._acquire(surface, write=False, blocking=False) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def try_acquire_write(
        self, surface
    ) -> "Iterator[Optional[CpuReadbackWriteView]]":
        """Non-blocking write acquire. Yields a [`CpuReadbackWriteView`]
        on success or ``None`` on contention."""
        with self._acquire(surface, write=True, blocking=False) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def _acquire(self, surface, write: bool, blocking: bool) -> "Iterator[object]":
        pool_id = _surface_pool_id(surface)
        format_ = _surface_format_from(surface)
        surface_id = self._resolve_and_register(pool_id, format_)
        view_struct = _SlpnCpuReadbackView()
        if blocking:
            fn = (
                self._lib.slpn_cpu_readback_acquire_write
                if write
                else self._lib.slpn_cpu_readback_acquire_read
            )
        else:
            fn = (
                self._lib.slpn_cpu_readback_try_acquire_write
                if write
                else self._lib.slpn_cpu_readback_try_acquire_read
            )
        rc = fn(self._rt, ctypes.c_uint64(surface_id), ctypes.byref(view_struct))
        if rc == _RC_CONTENDED:
            # Non-blocking miss — yield None so callers can skip this
            # frame without distinguishing happy-path from contention
            # via exception handling.
            yield None
            return
        if rc != _RC_OK:
            raise RuntimeError(
                f"CpuReadbackContext.{'try_' if not blocking else ''}"
                f"acquire_{'write' if write else 'read'}: rc={rc} for "
                f"surface '{pool_id}'"
            )
        try:
            yield self._build_view(view_struct, write)
        finally:
            release_fn = (
                self._lib.slpn_cpu_readback_release_write
                if write
                else self._lib.slpn_cpu_readback_release_read
            )
            release_fn(self._rt, ctypes.c_uint64(surface_id))

    def _build_view(self, view_struct: _SlpnCpuReadbackView, writable: bool):
        import numpy as np

        plane_count = int(view_struct.plane_count)
        if plane_count == 0:
            raise RuntimeError("cpu-readback acquire returned zero planes")
        format_ = SurfaceFormat(int(view_struct.format))
        plane_views = []
        for idx in range(plane_count):
            p = view_struct.planes[idx]
            byte_size = int(p.byte_size)
            base = int(p.mapped_ptr or 0)
            if not base:
                raise RuntimeError(
                    f"cpu-readback plane {idx} has null mapped_ptr — the host's "
                    "staging buffer registration may be stale"
                )
            raw = (ctypes.c_uint8 * byte_size).from_address(base)
            mv = memoryview(raw).cast("B")
            if not writable:
                mv = mv.toreadonly()
            arr = np.ndarray(
                shape=(int(p.height), int(p.width), int(p.bytes_per_pixel)),
                dtype=np.uint8,
                buffer=raw,
                strides=(
                    int(p.width) * int(p.bytes_per_pixel),
                    int(p.bytes_per_pixel),
                    1,
                ),
            )
            if writable:
                plane_views.append(
                    CpuReadbackPlaneViewMut(
                        width=int(p.width),
                        height=int(p.height),
                        bytes_per_pixel=int(p.bytes_per_pixel),
                        bytes=mv,
                        numpy=arr,
                    )
                )
            else:
                plane_views.append(
                    CpuReadbackPlaneView(
                        width=int(p.width),
                        height=int(p.height),
                        bytes_per_pixel=int(p.bytes_per_pixel),
                        bytes=mv,
                        numpy=arr,
                    )
                )
        if writable:
            return CpuReadbackWriteView(
                width=int(view_struct.width),
                height=int(view_struct.height),
                format=format_,
                planes=tuple(plane_views),
            )
        return CpuReadbackReadView(
            width=int(view_struct.width),
            height=int(view_struct.height),
            format=format_,
            planes=tuple(plane_views),
        )
