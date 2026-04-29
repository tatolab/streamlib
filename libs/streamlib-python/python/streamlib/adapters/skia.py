# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Skia surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-skia`` (#513) by
**composing on the Python ``vulkan`` adapter** (``streamlib.adapters.vulkan``)
plus ``skia-python`` to wrap the underlying ``VkImage`` as a
``skia.Surface`` (write) or ``skia.Image`` (read). The cdylib stays
small — there is no ``slpn_skia_*`` FFI surface; every line of
Vulkan handling routes through the existing ``slpn_vulkan_*`` symbols
the ``vulkan`` adapter wires up.

This is the canonical adapter-on-adapter composition shape, mirrored
in Python: ``SkiaContext`` constrains its inner type to
:class:`streamlib.adapters.vulkan.VulkanContext` (the public
customer-facing handle, NOT the FFI runtime), so the Skia wrapper
inherits every line of layout-transition + timeline-wait the host
adapter does. The customer never sees ``GrVkImageInfo``,
``VkImageLayout``, or any Vulkan handle — only ``skia.Surface``
(write) and ``skia.Image`` (read) come out of the public API.

Polyglot coverage: Python only. ``skia-python`` is the only
maintained cross-language Skia binding for the runtimes streamlib
supports — there is no Deno equivalent. The Deno wrapper for this
adapter is deliberately deferred (same construction-language
argument as the abandoned ``#481`` polyglot deferral); a Deno
customer that needs Skia today should drive Skia themselves against
:meth:`VulkanContext.raw_handles` until a Deno Skia binding emerges.
"""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
from typing import Iterator, Optional, Protocol, runtime_checkable

from streamlib.adapters.vulkan import (
    VulkanContext,
    VulkanImageInfo,
    VulkanReadView,
    VulkanWriteView,
)
from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    StreamlibSurface,
)

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "SkiaReadView",
    "SkiaWriteView",
    "SkiaContextProtocol",
    "SkiaContext",
]


# Vulkan format / image-tiling integer constants. Matches Vulkan spec
# values; bindgen-generated `skia.VkFormat` / `skia.VkImageLayout`
# enums use the same values, so callers can transmute via int.
class _VkFormat:
    UNDEFINED = 0
    R8G8B8A8_UNORM = 37
    R8G8B8A8_SRGB = 43
    B8G8R8A8_UNORM = 44
    B8G8R8A8_SRGB = 50
    R16G16B16A16_SFLOAT = 97
    R32G32B32A32_SFLOAT = 109


class _VkImageTiling:
    OPTIMAL = 0
    LINEAR = 1
    DRM_FORMAT_MODIFIER_EXT = 1000158000


@dataclass(frozen=True)
class SkiaWriteView:
    """View handed back inside an ``acquire_write`` scope.

    ``surface`` is a live ``skia.Surface`` wrapping the host's
    ``VkImage``. The customer draws into ``surface.canvas()``;
    on scope exit the wrapper flushes Skia's command stream and
    releases the underlying Vulkan acquire (which signals the host's
    timeline so the next consumer wakes up).
    """

    surface: object  # skia.Surface — typed `object` so this module imports cleanly without skia-python


@dataclass(frozen=True)
class SkiaReadView:
    """View handed back inside an ``acquire_read`` scope.

    ``image`` is a live ``skia.Image`` referencing the host's
    ``VkImage``. The customer samples it; on scope exit the wrapper
    drops the image and releases the underlying Vulkan acquire.
    """

    image: object  # skia.Image


@runtime_checkable
class SkiaContextProtocol(Protocol):
    """Customer-facing handle the subprocess runtime hands out.

    Same shape as the Rust ``streamlib_adapter_skia::SkiaContext``:
    scoped acquire/release returning a typed view. The trait
    composition pitch from #509 / #511 — Skia composes on Vulkan via
    the capability marker traits — translates to "uses
    :class:`VulkanContext` internally" in Python, since runtime
    checkability replaces the trait-bound machinery.
    """

    def acquire_read(self, surface): ...

    def acquire_write(self, surface): ...


class SkiaContext:
    """Subprocess-side Skia adapter runtime (#513).

    Composes on :class:`streamlib.adapters.vulkan.VulkanContext` —
    every Vulkan operation (host surface registration, timeline-wait,
    layout transitions) routes through the inner Vulkan adapter, and
    Skia is the framework that wraps the resulting ``VkImage``.

    Construct via :meth:`from_runtime`. Single :class:`SkiaContext`
    per subprocess; :meth:`from_runtime` returns the cached instance
    on repeat calls. The wrapper builds its ``GrDirectContext`` once
    against the inner Vulkan device's raw handles; per-acquire
    construction wraps the live ``VkImage`` as a Skia
    ``BackendRenderTarget`` and flushes on scope exit.

    Importing this module requires ``skia-python``. The import is
    deferred to first construction so that loading
    ``streamlib.adapters`` doesn't pay the ``skia-python`` import
    cost for customers that don't use Skia.
    """

    _shared_instance: Optional["SkiaContext"] = None

    def __init__(self, vulkan_ctx: VulkanContext) -> None:
        # Defer the skia-python import to construction so loading the
        # `streamlib.adapters` package doesn't drag skia-python in for
        # customers that don't use Skia. Failures (skia-python not
        # installed) surface here with a clear message instead of at
        # import time.
        try:
            import skia  # noqa: F401  (used in helper methods below)
        except ImportError as e:
            raise RuntimeError(
                "SkiaContext requires `skia-python` (>= m120 for Vulkan "
                "backend support). Install via `pip install skia-python`."
            ) from e
        self._skia = skia  # cache the module
        self._vulkan = vulkan_ctx
        self._direct_context = self._build_direct_context(vulkan_ctx)
        # Cache `VulkanImageInfo` per surface — fixed per registration,
        # avoid re-fetching on every acquire.
        self._image_info_cache: dict[str, VulkanImageInfo] = {}

    @classmethod
    def from_runtime(cls, runtime_context) -> "SkiaContext":
        """Build (or fetch the cached) :class:`SkiaContext` for this
        subprocess. The inner :class:`VulkanContext` is fetched via
        its own :meth:`VulkanContext.from_runtime` — a single
        ``VulkanContext`` is shared between this Skia adapter and any
        other Vulkan-using adapter the same subprocess hosts.
        """
        if cls._shared_instance is None:
            vulkan_ctx = VulkanContext.from_runtime(runtime_context)
            cls._shared_instance = cls(vulkan_ctx)
        return cls._shared_instance

    def _build_direct_context(self, vulkan_ctx: VulkanContext):
        """Build a ``skia.GrDirectContext`` from the inner Vulkan
        device's raw handles. Invoked once at construction; the
        resulting context is shared across every acquire.

        Skia's Vulkan backend needs a ``GetProc`` callback to resolve
        Vulkan function pointers; we reuse the streamlib-loaded
        ``libvulkan.so.1`` via ``ctypes`` and forward calls into
        ``vkGetInstanceProcAddr`` / ``vkGetDeviceProcAddr``.
        """
        skia = self._skia
        handles = vulkan_ctx.raw_handles()

        # Build GrVkBackendContext — populate fields directly since
        # skia-python's `GrVkBackendContext()` ctor takes no args.
        backend_ctx = skia.GrVkBackendContext()
        backend_ctx.fInstance = handles.vk_instance
        backend_ctx.fPhysicalDevice = handles.vk_physical_device
        backend_ctx.fDevice = handles.vk_device
        backend_ctx.fQueue = handles.vk_queue
        backend_ctx.fGraphicsQueueIndex = handles.vk_queue_family_index
        # Skia accepts `fMaxAPIVersion = 0` as "use whatever the
        # device supports"; setting it explicitly to the device's
        # version avoids subtle init-time fallbacks.
        backend_ctx.fMaxAPIVersion = handles.api_version
        backend_ctx.fGetProc = self._make_skia_get_proc()

        ctx = skia.GrDirectContext.MakeVulkan(backend_ctx)
        if ctx is None:
            raise RuntimeError(
                "SkiaContext: skia.GrDirectContext.MakeVulkan returned None — "
                "Skia could not bring up a Vulkan backend against the "
                "subprocess's VkDevice. Verify skia-python was built with "
                "Vulkan support (m120+) and that the device exposes the "
                "extensions Skia requires (sync2, dynamic-rendering, "
                "sampler-ycbcr-conversion are enabled by streamlib's "
                "ConsumerVulkanDevice)."
            )
        return ctx

    def _make_skia_get_proc(self):
        """Build the ``GetProc`` callback skia-python uses to resolve
        Vulkan function pointers.

        Loaded once per :class:`SkiaContext`. Skia copies the resolved
        procs into its own command tables during ``MakeVulkan`` so the
        callback only needs to be live across that single call.
        """
        import ctypes

        # Open libvulkan and grab vkGetInstanceProcAddr.
        libvulkan = ctypes.CDLL("libvulkan.so.1", mode=ctypes.RTLD_GLOBAL)
        get_instance_proc_addr = libvulkan.vkGetInstanceProcAddr
        get_instance_proc_addr.restype = ctypes.c_void_p
        get_instance_proc_addr.argtypes = [ctypes.c_void_p, ctypes.c_char_p]

        # vkGetDeviceProcAddr is itself fetched via vkGetInstanceProcAddr —
        # we resolve once at setup so the per-call closure stays cheap.
        vk_dev_proc_name = b"vkGetDeviceProcAddr"
        # We need an instance to resolve get_device_proc_addr. Use the
        # Vulkan adapter's instance.
        handles = self._vulkan.raw_handles()
        vk_instance = ctypes.c_void_p(handles.vk_instance)
        get_device_proc_addr_addr = get_instance_proc_addr(
            vk_instance, vk_dev_proc_name
        )
        if not get_device_proc_addr_addr:
            raise RuntimeError(
                "SkiaContext: vkGetInstanceProcAddr could not resolve "
                "vkGetDeviceProcAddr on the active VkInstance"
            )
        # Cast the address to the right callable shape.
        get_device_proc_addr = ctypes.CFUNCTYPE(
            ctypes.c_void_p, ctypes.c_void_p, ctypes.c_char_p
        )(get_device_proc_addr_addr)

        def get_proc(name: str, instance, device) -> int:
            """Resolve a Vulkan proc address.

            skia-python invokes this with ``(name, instance, device)``
            where exactly one of ``instance`` / ``device`` is non-zero.
            Returns the function pointer as an integer (skia-python
            treats it as ``intptr_t``).
            """
            name_bytes = name.encode("ascii") if isinstance(name, str) else name
            if device:
                addr = get_device_proc_addr(
                    ctypes.c_void_p(int(device)), name_bytes
                )
            else:
                addr = get_instance_proc_addr(
                    ctypes.c_void_p(int(instance)) if instance else None,
                    name_bytes,
                )
            return int(addr) if addr else 0

        return get_proc

    @staticmethod
    def _surface_pool_id(surface) -> str:
        if isinstance(surface, str):
            return surface
        sid = getattr(surface, "id", None)
        if sid is None:
            raise TypeError(
                f"SkiaContext: expected StreamlibSurface or str pool_id, got {surface!r}"
            )
        return str(sid)

    def _vk_image_info(self, surface) -> VulkanImageInfo:
        pool_id = self._surface_pool_id(surface)
        cached = self._image_info_cache.get(pool_id)
        if cached is not None:
            return cached
        info = self._vulkan.image_info(surface)
        self._image_info_cache[pool_id] = info
        return info

    def _build_skia_vk_image_info(
        self, vk_image: int, vk_image_layout: int, info: VulkanImageInfo
    ):
        """Translate a per-acquire ``VulkanWriteView`` / ``VulkanReadView``
        + the surface's static :class:`VulkanImageInfo` into a
        ``skia.GrVkImageInfo`` populated for ``GrBackendRenderTarget``.
        """
        skia = self._skia

        alloc = skia.GrVkAlloc()
        alloc.fMemory = info.memory_handle
        alloc.fOffset = info.memory_offset
        alloc.fSize = info.memory_size
        alloc.fFlags = 0  # NONE — Skia treats borrowed memory as opaque

        vk_info = skia.GrVkImageInfo()
        vk_info.fImage = vk_image
        vk_info.fAlloc = alloc
        # Skia's render-target paths reject `DRM_FORMAT_MODIFIER_EXT`;
        # the host always allocates with OPTIMAL or
        # `DRM_FORMAT_MODIFIER_EXT` (for cross-process tiled DMA-BUF).
        # Tiled-modifier images behave like OPTIMAL for the GPU
        # commands Skia issues, so we report OPTIMAL to keep Skia
        # happy. Pure LINEAR (sampler-only on NVIDIA) is preserved.
        if info.tiling == _VkImageTiling.DRM_FORMAT_MODIFIER_EXT:
            vk_info.fImageTiling = _VkImageTiling.OPTIMAL
        else:
            vk_info.fImageTiling = info.tiling
        vk_info.fImageLayout = vk_image_layout
        vk_info.fFormat = info.format
        vk_info.fImageUsageFlags = info.usage_flags
        vk_info.fSampleCount = info.sample_count
        vk_info.fLevelCount = info.level_count
        vk_info.fCurrentQueueFamily = info.queue_family
        vk_info.fProtected = bool(info.protected)
        return vk_info

    @staticmethod
    def _color_type_for_vk_format(skia, vk_format: int):
        if vk_format in (_VkFormat.B8G8R8A8_UNORM, _VkFormat.B8G8R8A8_SRGB):
            return skia.kBGRA_8888_ColorType
        if vk_format in (_VkFormat.R8G8B8A8_UNORM, _VkFormat.R8G8B8A8_SRGB):
            return skia.kRGBA_8888_ColorType
        if vk_format == _VkFormat.R16G16B16A16_SFLOAT:
            return skia.kRGBA_F16_ColorType
        if vk_format == _VkFormat.R32G32B32A32_SFLOAT:
            return skia.kRGBA_F32_ColorType
        raise RuntimeError(
            f"SkiaContext: unsupported vk::Format {vk_format} — Skia adapter "
            "currently maps BGRA8/RGBA8/RGBAF16/RGBAF32 only"
        )

    @staticmethod
    def _surface_dims(surface) -> tuple[int, int]:
        if isinstance(surface, StreamlibSurface):
            return int(surface.width), int(surface.height)
        # Fall back to attribute access for duck-typed surface descriptors.
        w = getattr(surface, "width", None)
        h = getattr(surface, "height", None)
        if w is None or h is None:
            raise TypeError(
                "SkiaContext: surface must carry width/height (StreamlibSurface "
                f"or compatible), got {surface!r}"
            )
        return int(w), int(h)

    @contextmanager
    def acquire_write(self, surface) -> Iterator[SkiaWriteView]:
        """Acquire write access. Yields a :class:`SkiaWriteView` whose
        ``surface`` is a live ``skia.Surface`` wrapping the host's
        ``VkImage``. The customer draws into ``surface.canvas()``;
        on scope exit the wrapper:

        1. Calls ``surface.flushAndSubmit(syncCpu=True)`` to drain
           Skia's GPU work.
        2. Drops the Skia ``Surface`` so its DirectContext refcount
           is released.
        3. Exits the inner ``VulkanContext.acquire_write`` scope,
           which signals the host's timeline so the next consumer
           wakes up.
        """
        skia = self._skia
        info = self._vk_image_info(surface)
        width, height = self._surface_dims(surface)
        color_type = self._color_type_for_vk_format(skia, info.format)
        with self._vulkan.acquire_write(surface) as vk_view:
            sk_surface = self._wrap_as_skia_surface(
                vk_view, info, width, height, color_type
            )
            try:
                yield SkiaWriteView(surface=sk_surface)
            finally:
                # SyncCpu::Yes — block until the GPU has executed all
                # of Skia's command submissions. Without this, the
                # vulkan release below host-signals the timeline
                # before the GPU is done, racing the next consumer.
                sk_surface.flushAndSubmit(syncCpu=True)
                # Releasing the Surface refcount decrements the
                # DirectContext's pinning. The vulkan release in the
                # outer `with` then runs and signals the timeline.
                del sk_surface

    @contextmanager
    def acquire_read(self, surface) -> Iterator[SkiaReadView]:
        """Acquire read access. Yields a :class:`SkiaReadView` whose
        ``image`` is a live ``skia.Image`` referencing the host's
        ``VkImage``. Read-only — no flush needed on scope exit.
        """
        skia = self._skia
        info = self._vk_image_info(surface)
        width, height = self._surface_dims(surface)
        color_type = self._color_type_for_vk_format(skia, info.format)
        with self._vulkan.acquire_read(surface) as vk_view:
            sk_image = self._wrap_as_skia_image(
                vk_view, info, width, height, color_type
            )
            try:
                yield SkiaReadView(image=sk_image)
            finally:
                del sk_image

    def _wrap_as_skia_surface(
        self,
        vk_view: VulkanWriteView,
        info: VulkanImageInfo,
        width: int,
        height: int,
        color_type,
    ):
        """Build a ``skia.Surface`` from a write-acquired ``VkImage``."""
        skia = self._skia
        vk_info = self._build_skia_vk_image_info(
            vk_view.vk_image, vk_view.vk_image_layout, info
        )
        backend_render_target = skia.GrBackendRenderTarget(
            width, height, vk_info
        )
        sk_surface = skia.Surface.MakeFromBackendRenderTarget(
            self._direct_context,
            backend_render_target,
            skia.kTopLeft_GrSurfaceOrigin,
            color_type,
            None,  # color_space
            None,  # surface_props
        )
        if sk_surface is None:
            raise RuntimeError(
                f"SkiaContext.acquire_write: skia.Surface.MakeFromBackendRenderTarget "
                f"returned None for surface (vk_image=0x{vk_view.vk_image:x}, "
                f"format={info.format}, usage_flags=0x{info.usage_flags:x}). "
                "Common causes: missing VK_IMAGE_USAGE_TRANSFER_DST_BIT on the "
                "host VkImage allocation; format / color-type mismatch; or "
                "Skia's GrVkCaps doesn't list the format as renderable."
            )
        return sk_surface

    def _wrap_as_skia_image(
        self,
        vk_view: VulkanReadView,
        info: VulkanImageInfo,
        width: int,
        height: int,
        color_type,
    ):
        """Build a ``skia.Image`` from a read-acquired ``VkImage``."""
        skia = self._skia
        vk_info = self._build_skia_vk_image_info(
            vk_view.vk_image, vk_view.vk_image_layout, info
        )
        backend_texture = skia.GrBackendTexture(width, height, vk_info)
        sk_image = skia.Image.MakeFromTexture(
            self._direct_context,
            backend_texture,
            skia.kTopLeft_GrSurfaceOrigin,
            color_type,
            skia.kPremul_AlphaType,
            None,  # color_space
        )
        if sk_image is None:
            raise RuntimeError(
                f"SkiaContext.acquire_read: skia.Image.MakeFromTexture returned "
                f"None for surface (vk_image=0x{vk_view.vk_image:x})"
            )
        return sk_image
