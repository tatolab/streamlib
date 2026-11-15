# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The GPU surfaces and raw handle exports a node is handed.

Each is a `typing.Protocol`: the runtime's native class is the only
implementation, held to it member for member by the wheel's conformance gate.
"""

from __future__ import annotations

from types import TracebackType
from typing import Any, Literal, Protocol

from ._runtime_lend import runtime_backed_protocol


@runtime_backed_protocol
class GpuSurfaceHandle(Protocol):
    """An owned GPU surface: pixels, or a tensor storage buffer.

    A tensor surface states `shape` and `dtype`; its pixel accessors (`width`,
    `height`, `format`, `bytes_per_row`, `lock`, `as_numpy`,
    `as_device_tensor`) raise naming it, and `__dlpack__` is its one door.
    """

    @property
    def surface_id(self) -> str: ...
    @property
    def width(self) -> int: ...
    @property
    def height(self) -> int: ...
    @property
    def format(self) -> str: ...
    @property
    def shape(self) -> list[int] | None:
        """A tensor surface's dimensions, outermost first; None for pixels."""
        ...

    @property
    def dtype(self) -> str | None:
        """A tensor surface's element type; None for pixels."""
        ...

    @property
    def bytes_per_row(self) -> int:
        """Row pitch in bytes, including any padding the allocation carries.

        Over a texture backing it is the staging's pitch, not the tiled
        texture's — the staging is the allocation the CPU addresses. Asking
        maps that staging, which needs no lock and costs one checkout the
        first time this process asks about the surface.
        """
        ...

    @property
    def base_address(self) -> int | None:
        """Base address of the host mapping, or None when not locked.

        A surface the CPU cannot address directly opens its staged door here,
        as `as_numpy` and `__dlpack__` do — so the address is the staging's,
        and reading it has read this frame in.
        """
        ...

    def close(self) -> None:
        """Release the underlying GPU resource. Idempotent."""
        ...

    def __enter__(self) -> GpuSurfaceHandle: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = None,
        exception: BaseException | None = None,
        traceback: TracebackType | None = None,
    ) -> Literal[False]: ...
    def lock(self, read_only: bool = True) -> None:
        """Open CPU access, declaring read or write intent.

        Performs no wait — ordering against the producer comes from
        publication, since a source finishes its GPU work before it sends the
        frame on. `read_only=False` marks an exported tensor writable.

        Never refused for want of a host mapping: a texture-backed surface
        reaches its pixels through the engine's host-visible staging, which
        the first host-side accessor inside the lock checks out and reads this
        frame into.
        """
        ...

    def unlock(self) -> None:
        """Close CPU access, publishing any pending write first. Idempotent.

        On Linux the edit publishes from whichever staging holds it. On macOS
        a writable Metal capsule's stores are already in the surface, so the
        unlock drains torch's MPS queue before it returns.
        """
        ...

    def as_numpy(self) -> Any:
        """A numpy view over the surface's pixels. Requires a lock.

        Shares memory with a surface the CPU can address directly; over a
        texture backing it is the surface's staging, read in on entry and
        published at `unlock()` when the lock declared a write.
        """
        ...

    def as_device_tensor(self) -> GpuSurfaceDeviceTensorScope:
        """The scoped device-tensor view over this surface's pixels, which a
        third-party GPU package writes in place. `GpuSurfaceDeviceTensorScope`
        states each floor's write rule.
        """
        ...

    def __dlpack_device__(self) -> tuple[int, int]: ...
    def __dlpack__(
        self,
        stream: Any | None = None,
        max_version: tuple[int, int] | None = None,
        dl_device: tuple[int, int] | None = None,
        copy: bool | None = None,
    ) -> Any:
        """A DLPack capsule over the pixels. Requires a lock.

        A graph frame's natural side is the device. On Linux, with a usable
        CUDA runtime, it is a `kDLCUDA` tensor over one engine-side blit into
        an exportable staging buffer — zero CPU copies, never claimed
        copy-free. On macOS it is a `kDLMetal` tensor over a no-copy Metal
        buffer on the frame's own IOSurface, which torch ≥ 2.10 (measured on
        2.14) imports as `mps` and MLX ≥ 0.32 as an array. Without a device
        side, or with `dl_device=(1, 0)`, it is the host mapping — on macOS
        the same IOSurface pages. `copy=True` is refused: the export is in
        place. A writable device tensor's edits publish at `unlock()`. The
        CUDA Array Interface is not offered on macOS.

        The tensor may outlive this handle: it holds its own share of the
        surface, so the pool slot is not reused until the tensor is released.

        A tensor surface needs no lock: its capsule is `kDLCUDA` on Linux and
        `kDLMetal` on macOS, in its declared shape straight over the engine's
        memory — writable for the node
        that acquired it, read-only for one that resolved it. The acquirer's
        writes are ordered at the handle's close, so publish the id after it;
        a write through a tensor kept past that close is out of contract and
        the engine does not revoke it. A host capsule is refused; copy with
        torch's `.cpu()`.
        """
        ...


@runtime_backed_protocol
class GpuSurfaceDeviceTensorScope(Protocol):
    """A scope handing a surface's pixels to a third-party GPU package.

    On Linux, entering blits the surface to a linear CUDA view; leaving
    normally blits any write back, ordered ahead of the engine's next read;
    leaving by a propagating exception discards the write and the surface
    keeps the frame it already held. No `torch.cuda.synchronize()` is owed.

    On macOS the view is a `kDLMetal` tensor over the surface's own IOSurface
    pages (torch ≥ 2.10, measured on 2.14; MLX ≥ 0.32), so a write lands in
    the surface itself, and publication is per store: a raise leaves the
    stores that already landed. Leaving either way drains torch's MPS queue,
    so no `torch.mps.synchronize()` is owed. MLX's queue is not drained,
    because `mx.synchronize()` holds the GIL while an MLX completion handler
    may need it. An MLX write therefore reaches the frame only when all of
    these hold:

    - it is `mx.eval`ed inside the scope;
    - it stores through a partial slice — MLX turns `a[:] = ...` and
      `a[...] = ...` into a new array, and the frame never sees it;
    - no other array derived from the imported one is alive at the store,
      since MLX then writes a buffer of its own.

    A GPU package other than torch and MLX must finish its own queue before
    the scope ends.

    Independent of `lock()` by design: entering the scope is the write
    declaration. A surface whose export cannot take a write-back — on Linux,
    a pool member its producer still owns, or a texture acquired without
    `copy_dst` usage — refuses at `__enter__` rather than discarding edits
    silently.
    """

    def __enter__(self) -> GpuSurfaceDeviceTensorScope: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = None,
        exception: BaseException | None = None,
        traceback: TracebackType | None = None,
    ) -> Literal[False]: ...
    def __dlpack_device__(self) -> tuple[int, int]: ...
    def __dlpack__(
        self,
        stream: Any | None = None,
        max_version: tuple[int, int] | None = None,
        dl_device: tuple[int, int] | None = None,
        copy: bool | None = None,
    ) -> Any:
        """A DLPack capsule over the scope's device view — what
        `torch.from_dlpack` and `mx.from_dlpack` consume. Always writable: a
        read-only export was refused at `__enter__`. `copy=True` and a host
        `dl_device` are refused; the host side is the handle's own.
        """
        ...


@runtime_backed_protocol
class GpuSurfaceCheckOutLease(Protocol):
    """A claim on a published surface, held for as long as this object is.

    While a claim is outstanding the pool never rehands that surface's slot to
    its producer, and dropping this object is the release — there is nothing to
    call. Claims are counted, so holding one and resolving the same surface for
    its pixels are independent.
    """

    @property
    def surface_id(self) -> str:
        """The surface this claim holds still."""
        ...


@runtime_backed_protocol
class OpaqueFdAllocationExport(Protocol):
    """A raw OPAQUE_FD memory handle — a texture's or a pixel buffer's: the
    allocation's memory fd plus the allocation-stable shape a foreign Vulkan
    or CUDA external-memory import must reproduce.

    Deliberately outside the `GpuSurface*` family prefix: the object names an
    allocation, never a frame-bearing surface — the surface-id lifetime
    guarantees end at export.
    """

    @property
    def fd(self) -> int:
        """The exported memory fd. The caller owns it: a successful foreign
        import adopts it — never close it after one; always close it after a
        failed one.
        """
        ...

    @property
    def allocation_byte_size(self) -> int:
        """Byte size of the whole `VkDeviceMemory` at offset zero — what the
        foreign import states, never a tight width x height x bpp figure.
        """
        ...

    @property
    def width(self) -> int:
        """Surface width in pixels."""
        ...

    @property
    def height(self) -> int:
        """Surface height in pixels."""
        ...

    @property
    def format(self) -> str:
        """The engine's format name for the surface, e.g. `"rgba16_float"` or
        `"rgba32"`.
        """
        ...

    @property
    def vk_image_tiling(self) -> int | None:
        """Raw `VkImageTiling` the exporter created the image with; `None`
        when the allocation is a pixel buffer.
        """
        ...

    @property
    def vk_image_usage_flags(self) -> int | None:
        """Raw `VkImageUsageFlags` bitfield the exporter created the image
        with; `None` when the allocation is a pixel buffer.
        """
        ...

    @property
    def vk_image_mip_levels(self) -> int | None:
        """`VkImageCreateInfo.mipLevels` of the exporter's image; `None` when
        the allocation is a pixel buffer.
        """
        ...

    @property
    def vk_image_array_layers(self) -> int | None:
        """`VkImageCreateInfo.arrayLayers` of the exporter's image; `None`
        when the allocation is a pixel buffer.
        """
        ...

    @property
    def vk_image_samples(self) -> int | None:
        """Raw `VkSampleCountFlagBits` of the exporter's image; `None` when the
        allocation is a pixel buffer.
        """
        ...

    @property
    def dedicated_allocation(self) -> bool:
        """Whether the allocation is dedicated — always true for this
        flavour, a texture's and a pixel buffer's alike. A Vulkan importer
        chains `VkMemoryDedicatedAllocateInfo`, a CUDA importer sets
        `cudaExternalMemoryDedicated`; omitting either is undefined
        behaviour, not leniency.
        """
        ...

    @property
    def vk_memory_type_index(self) -> int:
        """The exporter's Vulkan memory type index, for the importer-side
        `vkAllocateMemory(VkImportMemoryFdInfoKHR)`.
        """
        ...

    @property
    def exporting_device_uuid(self) -> bytes:
        """The exporting device's `VkPhysicalDeviceIDProperties.deviceUUID`,
        16 bytes. An OPAQUE_FD is device-bound: importing on the wrong GPU of
        a multi-GPU rig corrupts silently, so match this against the
        importer's own device UUID first.
        """
        ...


@runtime_backed_protocol
class IOSurfaceMachPortExport(Protocol):
    """A raw IOSurface handle: a Mach send right to the allocation's
    IOSurface plus the allocation-stable shape native code needs to address
    it.

    Deliberately outside the `GpuSurface*` family prefix: the object names an
    allocation, never a frame-bearing surface — the surface-id lifetime
    guarantees end at export.
    """

    @property
    def port(self) -> int:
        """The Mach port name of the send right. The caller owns it and
        deallocates it with `mach_port_deallocate`; a held right keeps the
        surface reading as in use.
        """
        ...

    @property
    def allocation_byte_size(self) -> int:
        """Byte size of the whole IOSurface allocation."""
        ...

    @property
    def bytes_per_row(self) -> int:
        """The IOSurface's row pitch in bytes — at least `width` pixels wide,
        often padded past it.
        """
        ...

    @property
    def width(self) -> int:
        """Surface width in pixels."""
        ...

    @property
    def height(self) -> int:
        """Surface height in pixels."""
        ...

    @property
    def format(self) -> str:
        """The engine's format name for the surface, e.g. `"bgra32"` or
        `"rgba8_unorm"`.
        """
        ...

    @property
    def vk_image_tiling(self) -> int | None:
        """Raw `VkImageTiling` the engine created the image with; `None` when
        the surface is a pixel buffer.
        """
        ...

    @property
    def vk_image_usage_flags(self) -> int | None:
        """Raw `VkImageUsageFlags` the engine created the image with; `None`
        when the surface is a pixel buffer.
        """
        ...

    @property
    def vk_image_mip_levels(self) -> int | None:
        """`VkImageCreateInfo.mipLevels` of the engine's image; `None` when
        the surface is a pixel buffer.
        """
        ...

    @property
    def vk_image_array_layers(self) -> int | None:
        """`VkImageCreateInfo.arrayLayers` of the engine's image; `None` when
        the surface is a pixel buffer.
        """
        ...

    @property
    def vk_image_samples(self) -> int | None:
        """Raw `VkSampleCountFlagBits` of the engine's image; `None` when the
        surface is a pixel buffer.
        """
        ...
