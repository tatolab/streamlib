# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Vulkan-native surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-vulkan`` (#511, #531). The
subprocess's actual Vulkan handling delegates to
``streamlib-python-native``'s ``slpn_vulkan_*`` FFI surface, which
itself wraps the host adapter crate's ``VulkanSurfaceAdapter`` against
a subprocess-local ``VulkanDevice``. There is **no** parallel Vulkan
implementation per language — every line of layout-transition,
timeline-wait, and queue-mutex coordination lives in
``streamlib-adapter-vulkan`` and runs in the subprocess process.

This module provides:

  * Typed views the subprocess sees inside ``acquire_*`` scopes —
    ``VulkanReadView`` / ``VulkanWriteView`` exposing ``vk_image`` (an
    integer handle) plus the current ``vk_image_layout``.
  * The ``VulkanContext`` class — built via :meth:`VulkanContext.from_runtime`
    inside a polyglot processor's ``setup`` hook. Customers acquire
    scoped read / write access via ``with ctx.acquire_write(surface)
    as view:`` and dispatch their own raw vulkanalia / Deno-FFI work
    against ``view.vk_image``.
  * ``raw_handles()`` — escape hatch returning the cdylib's runtime
    handles (``vk_instance``, ``vk_device``, ``vk_queue``, etc.) as
    integer handles for power-user callers that want to drive Vulkan
    directly. The handles point at the SAME ``VkDevice`` the adapter
    runs on, so customer-driven submissions and adapter-driven layout
    transitions interleave correctly under the device's queue mutex.
"""

from __future__ import annotations

import ctypes
import itertools
from contextlib import AbstractContextManager, contextmanager
from dataclasses import dataclass
from typing import Iterator, Optional, Protocol, runtime_checkable

from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    StreamlibSurface,
)

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "RawVulkanHandles",
    "VulkanImageInfo",
    "VulkanReadView",
    "VulkanWriteView",
    "VulkanSurfaceAdapter",
    "VulkanContextProtocol",
    "VulkanContext",
]


# Vulkan layout integer values mirroring vk::ImageLayout. Used so
# customer code doesn't need to import a Vulkan binding just to read
# `view.vk_image_layout`.
class VkImageLayout:
    UNDEFINED = 0
    GENERAL = 1
    COLOR_ATTACHMENT_OPTIMAL = 2
    SHADER_READ_ONLY_OPTIMAL = 5
    TRANSFER_SRC_OPTIMAL = 6
    TRANSFER_DST_OPTIMAL = 7


@dataclass(frozen=True)
class VulkanImageInfo:
    """Per-image VkImageInfo descriptor for a registered surface.

    Mirrors ``streamlib_adapter_abi::VkImageInfo`` field-for-field —
    customers wrapping the underlying ``VkImage`` as a framework-native
    handle (Skia's ``GrVkImageInfo``, vulkano's ``Image``, etc.) read
    this once on registration to populate their backend-context state.
    The per-acquire layout still flows through :class:`VulkanReadView`
    / :class:`VulkanWriteView`'s ``vk_image_layout`` field.

    All Vulkan enum / bitmask fields are kept as raw integers (the
    underlying types are ``i32`` / ``u32`` / ``u64`` per the spec) so
    this dataclass stays binding-agnostic.
    """

    format: int
    tiling: int
    usage_flags: int
    sample_count: int
    level_count: int
    queue_family: int
    memory_handle: int
    memory_offset: int
    memory_size: int
    memory_property_flags: int
    protected: int
    ycbcr_conversion: int


@dataclass(frozen=True)
class RawVulkanHandles:
    """Power-user escape hatch — raw Vulkan handles as integers.

    Customers wrap with their preferred Python Vulkan binding (e.g.
    ``vulkan.vkInstance(handle=...)``). Returned handles are valid
    for the lifetime of the streamlib runtime; using them after
    runtime shutdown is undefined.
    """

    vk_instance: int
    vk_physical_device: int
    vk_device: int
    vk_queue: int
    vk_queue_family_index: int
    api_version: int


@dataclass(frozen=True)
class VulkanReadView:
    """View handed back inside an ``acquire_read`` scope.

    ``vk_image`` is the integer Vulkan handle the customer feeds into
    their binding (``vulkan.VkImage(value=view.vk_image)`` or
    equivalent). ``vk_image_layout`` is the layout the adapter just
    transitioned the image to (``SHADER_READ_ONLY_OPTIMAL`` on read).
    """

    vk_image: int
    vk_image_layout: int


@dataclass(frozen=True)
class VulkanWriteView:
    """View handed back inside an ``acquire_write`` scope.

    Layout is ``GENERAL`` so the customer can use it as a transfer
    destination, color attachment, or compute storage image without
    re-transitioning.
    """

    vk_image: int
    vk_image_layout: int


@runtime_checkable
class VulkanSurfaceAdapter(Protocol):
    """Protocol an in-process Python Vulkan adapter implements.

    ``surface_id`` based, so the subprocess can pass just the
    ``StreamlibSurface.id`` from a descriptor it received over the
    surface-share IPC.
    """

    def acquire_read(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[VulkanReadView]: ...

    def acquire_write(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[VulkanWriteView]: ...

    def try_acquire_read(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[VulkanReadView]]: ...

    def try_acquire_write(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[VulkanWriteView]]: ...

    def raw_handles(self) -> RawVulkanHandles: ...


@runtime_checkable
class VulkanContextProtocol(Protocol):
    """Customer-facing handle the subprocess runtime hands out
    (Protocol shape — :class:`VulkanContext` below is the concrete
    implementation).
    """

    def acquire_read(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[VulkanReadView]: ...

    def acquire_write(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[VulkanWriteView]: ...

    def raw_handles(self) -> RawVulkanHandles: ...


# =============================================================================
# Concrete VulkanContext implementation (#531)
# =============================================================================
#
# Mirrors `streamlib.adapters.opengl.OpenGLContext` exactly: cached
# singleton per subprocess, surface-share `pool_id` → local `surface_id`
# mapping, FFI calls into `slpn_vulkan_*` symbols loaded by the runner.

# `slpn_vulkan_register_surface` owns the SurfaceHandle's sync_fd and
# DMA-BUF fds on success — the cdylib's `VulkanSurfaceAdapter::register_host_surface`
# transfers ownership into the subprocess `VkDevice`'s imported VkImage and
# imported timeline-semaphore. A successful return therefore zeroes out
# the SurfaceHandle's sync_fd slot and the SDK keeps a Python-side
# reference to the resolved handle so its remaining DMA-BUF fds stay alive.

# Surface-id namespace inside this subprocess. Counted up by
# `_resolve_and_register` — the host's pool_id (string UUID) is mapped to
# a u64 the adapter uses internally; customers never see the u64.
_VULKAN_SURFACE_ID_COUNTER = itertools.count(start=1)


class _SlpnVulkanView(ctypes.Structure):
    """C struct matching `streamlib_python_native::vulkan::SlpnVulkanView`."""

    _fields_ = [
        ("vk_image", ctypes.c_uint64),
        ("vk_image_layout", ctypes.c_int32),
    ]


class _SlpnVulkanRawHandles(ctypes.Structure):
    """C struct matching `streamlib_python_native::vulkan::SlpnVulkanRawHandles`."""

    _fields_ = [
        ("vk_instance", ctypes.c_uint64),
        ("vk_physical_device", ctypes.c_uint64),
        ("vk_device", ctypes.c_uint64),
        ("vk_queue", ctypes.c_uint64),
        ("vk_queue_family_index", ctypes.c_uint32),
        ("api_version", ctypes.c_uint32),
    ]


class _SlpnVulkanImageInfo(ctypes.Structure):
    """C struct matching `streamlib_python_native::vulkan::SlpnVulkanImageInfo`."""

    _fields_ = [
        ("format", ctypes.c_int32),
        ("tiling", ctypes.c_int32),
        ("usage_flags", ctypes.c_uint32),
        ("sample_count", ctypes.c_uint32),
        ("level_count", ctypes.c_uint32),
        ("queue_family", ctypes.c_uint32),
        ("memory_handle", ctypes.c_uint64),
        ("memory_offset", ctypes.c_uint64),
        ("memory_size", ctypes.c_uint64),
        ("memory_property_flags", ctypes.c_uint32),
        ("protected", ctypes.c_uint32),
        ("ycbcr_conversion", ctypes.c_uint64),
        ("_reserved", ctypes.c_uint8 * 16),
    ]


class VulkanContext:
    """Subprocess-side Vulkan adapter runtime (#531).

    Brings up `streamlib_consumer_rhi::ConsumerVulkanDevice` +
    ``streamlib_adapter_vulkan::VulkanSurfaceAdapter`` inside this
    subprocess and exposes scoped acquire/release that hands customers
    a real ``VkImage`` handle plus the layout the adapter transitioned
    to. The acquire/release calls reuse every line of host-RHI logic
    (timeline wait, layout transition, queue-mutex coordination,
    contention checking) — the Python side is a thin FFI shim.

    Construct via :meth:`from_runtime` — pass the typed runtime context
    you receive in ``setup`` / ``process``. Single :class:`VulkanContext`
    per subprocess; :meth:`from_runtime` returns the cached instance on
    repeat calls.

    Customers dispatch their own Vulkan work (compute, transfer, blit,
    etc.) using their preferred Vulkan binding — pyvulkan, raw ctypes
    against ``libvulkan.so.1``, etc. The cdylib's runtime exposes its
    raw handles through :meth:`raw_handles` so the customer's
    submissions interleave correctly with the adapter's layout
    transitions on the same ``VkQueue``.
    """

    _shared_instance: Optional["VulkanContext"] = None

    def __init__(self, gpu_limited_access) -> None:
        # Reuse the cdylib the limited-access view has already loaded —
        # `slpn_vulkan_*` symbols are wired up alongside `slpn_surface_*`
        # in `processor_context.load_native_lib`.
        self._lib = gpu_limited_access.native_lib
        self._gpu = gpu_limited_access
        self._wire_signatures()
        rt = self._lib.slpn_vulkan_runtime_new()
        if not rt:
            raise RuntimeError(
                "VulkanContext: slpn_vulkan_runtime_new returned NULL — the "
                "subprocess could not bring up a Vulkan device. Check that "
                "libvulkan.so.1 is installed and the driver supports "
                "VK_KHR_external_memory_fd, VK_EXT_external_memory_dma_buf, "
                "VK_EXT_image_drm_format_modifier, and "
                "VK_KHR_external_semaphore_fd."
            )
        self._rt = ctypes.c_void_p(rt)
        # Map host pool_id (UUID) → local u64 surface_id.
        self._surface_ids: dict[str, int] = {}
        # Pin the resolved SDK handles so the SurfaceShare-owned plane fds
        # stay alive for the runtime's lifetime — `register_surface`
        # transfers the sync_fd into the cdylib's adapter, but the plane
        # fds remain the SurfaceHandle's responsibility.
        self._resolved_handles: dict[str, object] = {}
        # Per-`VulkanContext` cache: `id(spv)` → (spv_strong_ref, kernel_id).
        # The strong ref pins the bytes so Python can't recycle the id for
        # a different object while we still hold the cached kernel_id.
        # Identity-keyed (no per-call hashing) so the hot path stays O(1)
        # even for multi-MB ML SPIR-V — customers should reuse the same
        # `bytes` object across dispatches (stash it on the processor at
        # `setup()`); fresh `bytes` instances are a cache miss and
        # re-register through escalate IPC (host-side cache hit, but the
        # IPC payload is sent again).
        self._compute_kernel_ids: dict[int, tuple[bytes, str]] = {}

    def _wire_signatures(self) -> None:
        """Set ctypes signatures on every `slpn_vulkan_*` entry point.

        Doing this once at construction lets every call site stay terse
        and gives ctypes the type info it needs to coerce Python
        ``int`` / ``bytes`` arguments into the right C widths.
        """
        lib = self._lib

        lib.slpn_vulkan_runtime_new.restype = ctypes.c_void_p
        lib.slpn_vulkan_runtime_new.argtypes = []

        lib.slpn_vulkan_runtime_free.restype = None
        lib.slpn_vulkan_runtime_free.argtypes = [ctypes.c_void_p]

        lib.slpn_vulkan_register_surface.restype = ctypes.c_int32
        lib.slpn_vulkan_register_surface.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint64,
            ctypes.c_void_p,
        ]

        lib.slpn_vulkan_unregister_surface.restype = ctypes.c_int32
        lib.slpn_vulkan_unregister_surface.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint64,
        ]

        for name in (
            "slpn_vulkan_acquire_write",
            "slpn_vulkan_acquire_read",
        ):
            fn = getattr(lib, name)
            fn.restype = ctypes.c_int32
            fn.argtypes = [
                ctypes.c_void_p,
                ctypes.c_uint64,
                ctypes.POINTER(_SlpnVulkanView),
            ]

        for name in (
            "slpn_vulkan_release_write",
            "slpn_vulkan_release_read",
        ):
            fn = getattr(lib, name)
            fn.restype = ctypes.c_int32
            fn.argtypes = [ctypes.c_void_p, ctypes.c_uint64]

        lib.slpn_vulkan_release_to_foreign.restype = ctypes.c_int32
        lib.slpn_vulkan_release_to_foreign.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint64,
            ctypes.c_int32,
        ]

        lib.slpn_vulkan_raw_handles.restype = ctypes.c_int32
        lib.slpn_vulkan_raw_handles.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(_SlpnVulkanRawHandles),
        ]

        lib.slpn_vulkan_get_image_info.restype = ctypes.c_int32
        lib.slpn_vulkan_get_image_info.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint64,
            ctypes.POINTER(_SlpnVulkanImageInfo),
        ]

        # Compute dispatch routes through escalate IPC (`register_compute_kernel`
        # + `run_compute_kernel`) — no cdylib FFI for compute. See `dispatch_compute`.

    @classmethod
    def from_runtime(cls, runtime_context) -> "VulkanContext":
        """Build (or fetch the cached) :class:`VulkanContext` for this
        subprocess.

        The subprocess hosts at most one Vulkan adapter runtime — calling
        this twice with the same runtime returns the same instance.
        """
        if cls._shared_instance is None:
            cls._shared_instance = cls(runtime_context.gpu_limited_access)
        return cls._shared_instance

    def _resolve_and_register(self, pool_id: str) -> int:
        """Resolve `pool_id` via surface-share, register with the Vulkan
        adapter, and return the local u64 surface_id. Idempotent — repeat
        calls return the cached id."""
        cached = self._surface_ids.get(pool_id)
        if cached is not None:
            return cached
        handle = self._gpu.resolve_surface(pool_id)
        handle_ptr = handle.native_handle_ptr
        if not handle_ptr:
            raise RuntimeError(
                f"VulkanContext: resolve_surface('{pool_id}') returned a handle "
                "with a null native pointer"
            )
        surface_id = next(_VULKAN_SURFACE_ID_COUNTER)
        rc = self._lib.slpn_vulkan_register_surface(
            self._rt,
            ctypes.c_uint64(surface_id),
            ctypes.c_void_p(handle_ptr),
        )
        if rc != 0:
            raise RuntimeError(
                f"VulkanContext: register_surface failed for pool_id "
                f"'{pool_id}' (rc={rc}). Check the subprocess log for "
                "import errors — typically a missing sync_fd (host did not "
                "register the texture with an exportable timeline), an "
                "unsupported DRM modifier, or an unsupported pixel format."
            )
        self._surface_ids[pool_id] = surface_id
        # Hold the SDK handle so its DMA-BUF plane fds stay alive for the
        # runtime's lifetime. The sync_fd was transferred into the cdylib
        # by `register_surface` and is now owned by Vulkan.
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
                f"VulkanContext: expected StreamlibSurface or str pool_id, got {surface!r}"
            )
        return str(sid)

    @contextmanager
    def acquire_write(
        self, surface
    ) -> "Iterator[VulkanWriteView]":
        """Acquire write access. Returns a view exposing the imported
        ``VkImage`` handle and the layout the adapter transitioned to
        (``GENERAL``). On scope exit the adapter signals the host's
        timeline so the next consumer can wake up; the customer is
        responsible for ``vkQueueWaitIdle``-ing or chaining a binary
        semaphore on their own submission BEFORE leaving the scope so
        their writes are visible.
        """
        pool_id = self._surface_pool_id(surface)
        surface_id = self._resolve_and_register(pool_id)
        view = _SlpnVulkanView()
        rc = self._lib.slpn_vulkan_acquire_write(
            self._rt,
            ctypes.c_uint64(surface_id),
            ctypes.byref(view),
        )
        if rc != 0:
            raise RuntimeError(
                f"VulkanContext.acquire_write: slpn_vulkan_acquire_write "
                f"returned {rc} for surface '{pool_id}' (contention or "
                "adapter failure — check the subprocess log)"
            )
        try:
            yield VulkanWriteView(
                vk_image=int(view.vk_image),
                vk_image_layout=int(view.vk_image_layout),
            )
        finally:
            self._lib.slpn_vulkan_release_write(
                self._rt, ctypes.c_uint64(surface_id)
            )

    @contextmanager
    def acquire_read(
        self, surface
    ) -> "Iterator[VulkanReadView]":
        """Acquire read access — same shape as :meth:`acquire_write`,
        but the resulting image is in ``SHADER_READ_ONLY_OPTIMAL``
        (multiple readers may coexist; no writer can be active)."""
        pool_id = self._surface_pool_id(surface)
        surface_id = self._resolve_and_register(pool_id)
        view = _SlpnVulkanView()
        rc = self._lib.slpn_vulkan_acquire_read(
            self._rt,
            ctypes.c_uint64(surface_id),
            ctypes.byref(view),
        )
        if rc != 0:
            raise RuntimeError(
                f"VulkanContext.acquire_read: slpn_vulkan_acquire_read "
                f"returned {rc} for surface '{pool_id}'"
            )
        try:
            yield VulkanReadView(
                vk_image=int(view.vk_image),
                vk_image_layout=int(view.vk_image_layout),
            )
        finally:
            self._lib.slpn_vulkan_release_read(
                self._rt, ctypes.c_uint64(surface_id)
            )

    def dispatch_compute(
        self,
        surface,
        spirv: bytes,
        push_constants: bytes,
        group_count_x: int,
        group_count_y: int,
        group_count_z: int,
    ) -> None:
        """Dispatch a compute shader against the surface's host-side
        ``VkImage`` via escalate IPC. The surface MUST currently be held
        in WRITE mode (call inside an ``acquire_write`` ``with`` block).

        The shader's `binding=0` is bound to the surface's `VkImage` as
        a storage image. Push constants are forwarded byte-for-byte;
        their length must match the kernel's reflected push-constant
        range size.

        Compute is synchronous host-side: when this returns, the GPU
        work has retired and the host's writes are visible. The
        ``VulkanComputeKernel`` is built once on the host (SPIR-V
        reflection, on-disk pipeline cache via
        ``$STREAMLIB_PIPELINE_CACHE_DIR`` /
        ``$XDG_CACHE_HOME/streamlib/pipeline-cache``) and re-used
        across dispatches with the same SPIR-V.
        """
        from streamlib.escalate import channel as _escalate_channel

        pool_id = self._surface_pool_id(surface)
        cached = self._surface_ids.get(pool_id)
        if cached is None:
            raise RuntimeError(
                f"VulkanContext.dispatch_compute: surface '{pool_id}' is not "
                "registered — call acquire_write inside a `with` block first."
            )
        ch = _escalate_channel()
        # Identity-keyed kernel-id cache: `id(spv)` is O(1) per dispatch,
        # so multi-MB ML SPIR-V doesn't pay a SHA-256 cost on the hot
        # path. The cache holds a strong reference to the bytes so
        # Python can't recycle the id for a different object.
        spv_id = id(spirv)
        cached_entry = self._compute_kernel_ids.get(spv_id)
        if cached_entry is not None and cached_entry[0] is spirv:
            kernel_id = cached_entry[1]
        else:
            response = ch.register_compute_kernel(spirv, len(push_constants))
            kernel_id = response["handle_id"]
            self._compute_kernel_ids[spv_id] = (spirv, kernel_id)
        # Send the surface-share UUID, not the cdylib's local u64
        # surface_id — the host bridge resolves UUID → host
        # `StreamTexture` via an application-provided map.
        ch.run_compute_kernel(
            kernel_id=kernel_id,
            surface_uuid=pool_id,
            push_constants=push_constants,
            group_count_x=int(group_count_x),
            group_count_y=int(group_count_y),
            group_count_z=int(group_count_z),
        )

    def release_for_cross_process(
        self, surface, post_release_layout: int
    ) -> None:
        """Issue a producer-side queue-family-ownership-transfer (QFOT)
        release barrier on this subprocess's ``ConsumerVulkanDevice`` and
        publish the post-release ``VkImageLayout`` to surface-share so
        the next cross-process consumer's ``acquire_from_foreign`` sees
        the right source layout.

        Call this *after* the matching :meth:`acquire_write` /
        :meth:`acquire_read` ``with`` block has exited and after the
        producer's queue submission has actually retired (e.g. the
        customer has signalled their own timeline or
        ``vkQueueWaitIdle``-ed). The adapter's QFOT release barrier
        carries ``srcAccessMask = MEMORY_WRITE_BIT`` and assumes
        producer-side hazard coverage upstream.

        Also serves the **dual-registration** path used by non-Vulkan
        adapters that need cross-process release wiring (OpenGL via
        :meth:`streamlib.adapters.opengl.OpenGLContext.release_for_cross_process`,
        and Skia GL by extension). In that mode the surface may not
        have been touched by an explicit ``acquire_*`` on this Vulkan
        context — the release barrier still issues correctly because
        the surface-share registration carries the producer's
        post-write layout as the Vulkan adapter's initial layout, so
        the QFOT source layout matches what the OpenGL writes left
        the DMA-BUF in.

        ``post_release_layout`` is a Vulkan ``VkImageLayout`` enumerant
        as an integer (use :class:`VkImageLayout` constants). Picking
        ``GENERAL`` is the safest default for cross-process handoffs
        — the consumer's ``acquire_from_foreign`` re-transitions to
        whatever layout it actually needs.

        On NVIDIA Linux drivers without
        ``VK_EXT_external_memory_acquire_unmodified`` (current state
        as of 2026-05-03), the host consumer side falls back to a
        bridging ``UNDEFINED → target`` transition; content
        preservation is empirical (see
        ``docs/learnings/cross-process-vkimage-layout.md``). The
        producer-side path here is correct under both modes.
        """
        pool_id = self._surface_pool_id(surface)
        # Lazily resolve+register so dual-registration callers
        # (OpenGL, Skia GL) don't have to issue a no-op acquire first.
        # Idempotent — repeat calls return the cached id.
        surface_id = self._resolve_and_register(pool_id)
        rc = self._lib.slpn_vulkan_release_to_foreign(
            self._rt,
            ctypes.c_uint64(surface_id),
            ctypes.c_int32(int(post_release_layout)),
        )
        if rc != 0:
            raise RuntimeError(
                f"VulkanContext.release_for_cross_process: "
                f"slpn_vulkan_release_to_foreign returned {rc} for "
                f"surface '{pool_id}' (check subprocess log for the "
                "underlying adapter error)"
            )
        # Pair the QFOT release with the surface-share `update_layout`
        # publish so the next host-side consumer's `acquire_from_foreign`
        # picks up the new layout instead of the cached registration one.
        self._gpu.update_image_layout(pool_id, int(post_release_layout))

    def raw_handles(self) -> RawVulkanHandles:
        """Return the cdylib runtime's raw Vulkan handles — same shape
        as the Rust ``streamlib_adapter_vulkan::raw_handles()``. Use
        these to drive your preferred Vulkan binding against the SAME
        ``VkDevice`` the adapter manages."""
        h = _SlpnVulkanRawHandles()
        rc = self._lib.slpn_vulkan_raw_handles(self._rt, ctypes.byref(h))
        if rc != 0:
            raise RuntimeError(
                f"VulkanContext.raw_handles: slpn_vulkan_raw_handles returned {rc}"
            )
        return RawVulkanHandles(
            vk_instance=int(h.vk_instance),
            vk_physical_device=int(h.vk_physical_device),
            vk_device=int(h.vk_device),
            vk_queue=int(h.vk_queue),
            vk_queue_family_index=int(h.vk_queue_family_index),
            api_version=int(h.api_version),
        )

    def image_info(self, surface) -> VulkanImageInfo:
        """Return the per-image VkImageInfo descriptor for a registered
        surface. Resolves and registers the surface lazily if it
        hasn't been touched yet (same shape as
        :meth:`acquire_write` / :meth:`acquire_read`).

        Per-image: the descriptor is fixed at registration time and
        does NOT change across acquires. Customers that wrap the
        underlying ``VkImage`` as a framework-native handle (Skia's
        ``GrVkImageInfo``, vulkano's ``Image``, etc.) call this once
        per surface and cache the result; the per-acquire layout
        flows through the ``vk_image_layout`` field on
        :class:`VulkanReadView` / :class:`VulkanWriteView`.
        """
        pool_id = self._surface_pool_id(surface)
        surface_id = self._resolve_and_register(pool_id)
        info = _SlpnVulkanImageInfo()
        rc = self._lib.slpn_vulkan_get_image_info(
            self._rt,
            ctypes.c_uint64(surface_id),
            ctypes.byref(info),
        )
        if rc != 0:
            raise RuntimeError(
                f"VulkanContext.image_info: slpn_vulkan_get_image_info "
                f"returned {rc} for surface '{pool_id}'"
            )
        return VulkanImageInfo(
            format=int(info.format),
            tiling=int(info.tiling),
            usage_flags=int(info.usage_flags),
            sample_count=int(info.sample_count),
            level_count=int(info.level_count),
            queue_family=int(info.queue_family),
            memory_handle=int(info.memory_handle),
            memory_offset=int(info.memory_offset),
            memory_size=int(info.memory_size),
            memory_property_flags=int(info.memory_property_flags),
            protected=int(info.protected),
            ycbcr_conversion=int(info.ycbcr_conversion),
        )
