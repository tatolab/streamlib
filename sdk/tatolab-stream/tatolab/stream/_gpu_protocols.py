# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The GPU capabilities, surfaces, raw handle exports and kernels a node is handed.

Each is a `typing.Protocol`: the runtime's native class is the only
implementation, held to it member for member by the wheel's conformance gate.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping, Sequence
from types import TracebackType
from typing import TYPE_CHECKING, Any, Literal, Protocol, TypeVar

from ._runtime_lend import runtime_backed_protocol

if TYPE_CHECKING:
    from ._node_owned_window_protocols import NodeOwnedWindow

_EscalateResult = TypeVar("_EscalateResult")

StorageBufferDtype = Literal["float32", "float16", "uint8", "int32"]


class _GpuContextMembersBothCapabilitiesShare(Protocol):
    """The GPU methods the limited and the full capability both carry."""

    def acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> GpuSurfaceHandle: ...

    def acquire_texture(
        self, width: int, height: int, format: str, usage: list[str]
    ) -> GpuSurfaceHandle:
        """Acquire a pooled device texture, named by the surface id the engine minted.

        The id is the whole handle: a kernel dispatch binds it, and a downstream
        node resolves it. `copy_src` and `copy_dst` ride every request, so
        the CPU doors reach the pixels — over the surface's host-visible staging
        on Linux, through its own IOSurface on macOS — without the caller
        spelling a transfer usage.
        """
        ...

    def acquire_texture_from_node_output_pool(
        self,
        pool_key: str,
        rotation_depth: int,
        width: int,
        height: int,
        format: str,
        usage: list[str],
    ) -> GpuSurfaceHandle:
        """The texture this frame publishes into, from the node output pool `pool_key`.

        Every call names a new frame, `<slot>#<generation>`, in a slot the pool
        owns; a slot any consumer still holds is skipped, never rewritten. The
        pool rotates through `rotation_depth` slots, grows while consumers hold
        frames, and at its cap raises naming the pool — the producer drops its
        own frame rather than wait. `NodeOutputTextureRing` is the
        spelling a node reaches for.
        """
        ...

    def acquire_storage_buffer(
        self,
        shape: Sequence[int],
        dtype: StorageBufferDtype,
    ) -> GpuSurfaceHandle:
        """A tensor storage buffer of `shape` and `dtype`, named by the surface id the engine minted.

        Contiguous row-major, every dimension non-zero. The handle states
        `shape` and `dtype` and no pixel geometry; `torch.from_dlpack` writes it
        on the GPU in place, over the engine's own memory with no staging and
        no copy — `kDLCUDA` on Linux, `kDLMetal` on macOS, which torch imports
        as `mps` and MLX as an array. Closing the handle (or leaving its `with`
        block) orders torch's writes ahead of every other holder's read, so
        publish the id after it; an MLX write is ordered by the `mx.eval` it
        owes inside the block. A one-off is released at close; a tensor
        published downstream comes from
        `acquire_storage_buffer_from_node_output_pool`.
        """
        ...

    def acquire_storage_buffer_from_node_output_pool(
        self,
        pool_key: str,
        rotation_depth: int,
        shape: Sequence[int],
        dtype: StorageBufferDtype,
    ) -> GpuSurfaceHandle:
        """The tensor this frame publishes into, from the node output pool `pool_key`.

        The pool contract `acquire_texture_from_node_output_pool` states:
        a new `<slot>#<generation>` per call, a slot a consumer still holds is
        never rewritten, and at the cap the call raises naming the pool.
        """
        ...

    def copy_surface_to_surface(
        self, source_surface_id: str, destination_surface: GpuSurfaceHandle
    ) -> None:
        """Copy one surface's pixels into another, same format and extent.

        Any backing pair: the engine picks the copy the two need and converts
        nothing. A pixel buffer's `rgba` and a texture's `rgba8_unorm` are one
        format; two textures must match exactly (`rgba8_unorm` is not
        `rgba8_unorm_srgb`). Returns once the destination's next reader would
        see the copied pixels. A format or extent mismatch, a retired frame,
        a destination that cannot take a write-back, and a source and
        destination that are one allocation each raise naming the reason.
        The copy reads the source as it is when the copy runs: hold
        `claim_surface_against_producer_reuse` on a frame whose producer may
        recycle it, so the pixels copied are the ones its id named.
        """
        ...


@runtime_backed_protocol
class GpuContextLimitedAccess(_GpuContextMembersBothCapabilitiesShare, Protocol):
    """Non-allocating GPU capability, valid for the node's whole life."""

    def resolve_surface(self, surface_id: str) -> GpuSurfaceHandle: ...

    def claim_surface_against_producer_reuse(
        self, surface_id: str
    ) -> GpuSurfaceCheckOutLease:
        """Claim a published surface until the returned lease is dropped.

        The cheap half of `resolve_surface`: it holds the frame still without
        importing its memory, so an object that wants only the pixels it was
        handed to stay put can keep the lease in a field and let its own
        lifetime do the releasing.
        """
        ...

    def surface_can_take_write_back(self, surface_id: str) -> bool:
        """Whether an edit written back into this surface publishes at all.

        The engine's one answer for every write door: a write-back belongs to
        a pooled frame whose allocation is its only backing, or to a
        registered texture that takes a recorded copy in; a frame backed by
        neither answers False. Every texture this node acquired answers
        True — it can take the copy — so False narrows to a pooled frame its
        producer still owns and a foreign registration without transfer usage.
        `writable()` refuses on this answer; `cpu()` hands its array out
        read-only on it.
        """
        ...

    def escalate(
        self, privileged_callback: Callable[[GpuContextFullAccess], _EscalateResult]
    ) -> _EscalateResult:
        """Refuses: the callback's one atomic privileged scope cannot span a
        process boundary. The operations it wrapped are methods on this
        capability and on `ctx.gpu_full_access` — call them directly."""
        ...


@runtime_backed_protocol
class GpuContextFullAccess(_GpuContextMembersBothCapabilitiesShare, Protocol):
    """The privileged GPU capability a full-access hook receives.

    Each method is its own escalate round trip to the parent, which runs the
    privileged work against the engine and answers with a handle.
    """

    def create_window(
        self, title: str, width: int = 1280, height: int = 720
    ) -> NodeOwnedWindow:
        """Request a window this node owns, presented by the engine.

        `width` and `height` are the window's initial size in the desktop's
        logical pixels, so it is the same size on a 1x and a 2x display.

        Constructed once in `setup()`, named frames per frame in `process()`.
        The window lives in the app process on its own present loop, so it
        keeps its frame rate whatever this node's pace is, and naming no
        frame leaves the last one up.

        Raises when the process can get no window at all — no display server,
        or a window event pump that has already failed — rather than handing
        back a window that would show nothing. An author for whom the window
        is optional writes the `try/except`.
        """
        ...

    def create_compute_kernel(
        self,
        source: str | None = None,
        spirv: bytes | None = None,
        push_constant_size: int = 0,
        bindings: dict[str, str] | None = None,
        entry_point: str = "main",
    ) -> ComputeKernel:
        """Build a compute kernel from GLSL `source`, or from pre-compiled SPIR-V.

        Constructed once in `setup()`, dispatched per frame in `process()`. The
        engine compiles the source and reflects the shader at construction,
        taking its binding names from it — those names are what `dispatch`
        resolves against. Re-creating an identical kernel is free of
        compilation. Authoring needs no shader toolchain: the compiler is in
        the wheel.

        `source` and `spirv` are alternatives — supply exactly one. A GLSL
        entry point is always `main`; `entry_point` is meaningful only with
        `spirv`.

        `bindings` optionally asserts `{name: kind}` against reflection; each
        kind is one of `sampled_image`, `sampled_texture`, `storage_buffer`,
        `storage_image`, `uniform_buffer`.

        A shader using subgroup operations the device's driver does not serve
        raises here, naming the driver.
        """
        ...

    def create_graphics_kernel(
        self,
        color_attachment_formats: Sequence[str],
        vertex_source: str | None = None,
        vertex_spirv: bytes | None = None,
        vertex_entry_point: str = "main",
        fragment_source: str | None = None,
        fragment_spirv: bytes | None = None,
        fragment_entry_point: str = "main",
        push_constant_size: int = 0,
        bindings: dict[str, str | tuple[str, Sequence[str]]] | None = None,
        label: str = "",
        topology: str = "triangle_list",
        polygon_mode: str = "fill",
        cull_mode: str = "none",
        front_face: str = "counter_clockwise",
        line_width: float = 1.0,
        color_write_channels: str = "rgba",
        color_blend: Mapping[str, str] | None = None,
        dynamic_state: str = "viewport_scissor",
    ) -> GraphicsKernel:
        """Build a graphics kernel from GLSL sources, or from pre-compiled SPIR-V.

        Constructed once in `setup()`, drawn per frame in `process()`. The
        engine compiles both stages and reflects them at construction, taking
        its binding names from them — those names are what `draw` resolves
        against. Re-creating an identical kernel is free of compilation.

        Each stage takes `*_source` or `*_spirv`, never both. The vertices are
        the shaders' own: no vertex or index buffer is reachable from a Python
        node, so a vertex stage fabricates its positions from
        `gl_VertexIndex`. The pass attaches colour targets only, so the
        pipeline carries no depth state.

        `bindings` optionally asserts the shape against reflection — `{name:
        kind}`, or `{name: (kind, stages)}` to assert which stages read a
        binding. Each kind is one of `sampled_texture`, `storage_buffer`,
        `storage_image`, `uniform_buffer`; each stage is `vertex` or
        `fragment`.

        `color_blend` is `None` for no blending, or a mapping of any of
        `src_color_factor`, `dst_color_factor`, `color_op`,
        `src_alpha_factor`, `dst_alpha_factor`, `alpha_op` — the rest default
        to source-alpha-over.

        A stage using subgroup operations the device's driver does not serve
        in that stage raises here, naming the driver — MoltenVK serves none in
        the vertex stage.
        """
        ...

    def create_ray_tracing_kernel(
        self,
        stages: Sequence[Mapping[str, Any]],
        groups: Sequence[Mapping[str, Any]],
        max_recursion_depth: int = 1,
        push_constant_size: int = 0,
        bindings: dict[str, str | tuple[str, Sequence[str]]] | None = None,
        label: str = "",
    ) -> RayTracingKernel:
        """Build a ray-tracing kernel from GLSL sources, or from pre-compiled SPIR-V.

        `stages` is one mapping per shader module — `{"stage": "ray_gen",
        "source": …}`, where `stage` is one of `ray_gen`, `miss`,
        `closest_hit`, `any_hit`, `intersection`, `callable`, and the module
        itself is `source` or `spirv` with an optional `entry_point`.

        `groups` says how the shader binding table is laid out over them:
        `{"kind": "general", "general_stage": 0}`, `{"kind": "triangles_hit",
        "closest_hit_stage": 2}`, or `{"kind": "procedural_hit",
        "intersection_stage": 3}`. A group names its modules by index into
        `stages`, because two modules can fill the same stage.

        `bindings` takes the same shape `create_graphics_kernel` does, plus the
        `acceleration_structure` kind.

        Ray tracing is a tier a device has or lacks. Without it — every macOS
        device, since MoltenVK exposes no `VK_KHR_ray_tracing_pipeline` — this,
        `build_triangles_blas` and `build_tlas` raise naming the absent tier.
        """
        ...

    def build_triangles_blas(
        self,
        vertices: Sequence[float],
        indices: Sequence[int],
        label: str = "",
    ) -> AccelerationStructureHandle:
        """Build a bottom-level acceleration structure over triangle geometry.

        `vertices` is `[x, y, z, x, y, z, …]` and `indices` is three per
        triangle. The returned handle is what `build_tlas` places in a scene.
        """
        ...

    def build_tlas(
        self,
        instances: Sequence[Mapping[str, Any]],
        label: str = "",
    ) -> AccelerationStructureHandle:
        """Build the top-level acceleration structure a trace binds.

        Each instance names its `blas` and, optionally, the row-major 3×4
        `transform` that places it (12 floats, identity by default), its 8-bit
        `mask`, its 24-bit `custom_index`, its `sbt_record_offset`, and its
        geometry `flags` — some of `triangle_facing_cull_disable`,
        `triangle_flip_facing`, `force_opaque`, `force_no_opaque`.

        The structure keeps every bottom-level one it references alive.
        """
        ...

    def kernel_dispatch_batch(self) -> KernelDispatchBatch:
        """Open a scope that records several dispatches and runs them as one.

        The Python equivalent of the engine's command-recorder flow, and why
        dispatch has two entry points: `kernel.dispatch()` for a single pass,
        this for several. A two-pass filter costs one round trip, one
        submission and one fence wait instead of two of each.

        Leaving the scope runs the batch and returns when the GPU work has
        retired, exactly as a single dispatch does. Leaving it by a raise runs
        nothing.
        """
        ...

    def export_dma_buf(self, surface: GpuSurfaceHandle) -> tuple[int, int]:
        """Export a DMA-BUF file descriptor for `surface`, as `(fd, byte_size)`.

        Linux only. On macOS it refuses by name, pointing at
        `export_iosurface` — a surface there is an IOSurface, not an fd.

        The caller owns the fd and must close it, or hand it to something that
        takes ownership. Answered without leaving this process: the fds arrived
        over SCM_RIGHTS when the surface was checked out, and they are the same
        ones a host-side export would mint.

        Refuses by name for an OPAQUE_FD-flavoured texture — that fd imports
        through Vulkan or CUDA external memory, not as a DMA-BUF; export it
        through `export_opaque_fd` instead — and for a pooled-texture handle
        whose memory was never checked out into this process (resolve the
        surface id first).
        """
        ...

    def export_opaque_fd(self, surface: GpuSurfaceHandle) -> OpaqueFdTextureExport:
        """Export the OPAQUE_FD texture handle for `surface`, for native code
        that runs its own Vulkan or CUDA external-memory import against the
        allocation.

        Linux only. On macOS it refuses by name, pointing at
        `export_iosurface` — a surface there is an IOSurface, not an fd.

        The caller owns the returned object's fd: a successful foreign import
        adopts it — never close it after one; always close it after a failed
        one. Consume the texture as an image (CUDA maps the mipmapped array;
        Vulkan recreates the image from the carried recipe) — a linear buffer
        mapping over OPTIMAL-tiled memory yields block-linear bytes, never
        pixels.

        A raw handle names the allocation, never the frame: the surface-id
        lifetime guarantees end at export, and per-frame reach stays with
        surface ids and `as_device_tensor()`. Answered without leaving this
        process: the fd arrived over SCM_RIGHTS when the surface was checked
        out.

        Refuses by name for a DMA-BUF-flavoured texture (use
        `export_dma_buf`), for a pixel buffer, and for a pooled-texture
        handle whose memory was never checked out into this process (resolve
        the surface id first).
        """
        ...

    def export_iosurface(self, surface: GpuSurfaceHandle) -> IOSurfaceMachPortExport:
        """Export a Mach send right to `surface`'s IOSurface, for native code
        that looks the surface up itself — `IOSurfaceLookupFromMachPort`, then
        Metal, CoreVideo, or a Vulkan `VkImportMetalIOSurfaceInfoEXT` import.

        macOS only. On Linux it refuses by name, pointing at `export_dma_buf`
        and `export_opaque_fd` — a surface there is a file-descriptor
        allocation.

        The caller owns the returned object's send right and deallocates it
        with `mach_port_deallocate`. While the right is held the surface
        reads as in use (`IOSurfaceIsInUse`), which pins its pool slot
        exactly as a held fd pins a DMA-BUF on Linux — that is the contract.
        Pixel buffers and textures both export; a texture's carries the image
        recipe the engine created it with.

        A raw handle names the allocation, never the frame: the surface-id
        lifetime guarantees end at export, and per-frame reach stays with
        surface ids and `as_device_tensor()`. Answered without leaving this
        process: the helper already holds the IOSurface from the surface's
        checkout and mints the right itself.
        """
        ...

    def import_dma_buf(
        self,
        fd: int,
        width: int,
        height: int,
        format: str = "bgra",
        byte_size: int | None = None,
    ) -> GpuSurfaceHandle:
        """Adopt a foreign single-plane DMA-BUF fd as a surface this graph can
        resolve.

        The fd crosses to the engine's surface-share service over SCM_RIGHTS —
        the caller keeps ownership and may close it once this returns. The
        returned handle maps the same memory and travels under a freshly minted
        surface id; closing its last holder removes the registration. When
        `byte_size` is omitted a tight plane is assumed — pass the exporter's
        own byte size whenever the buffer carries row padding. The fd must
        reference host-mappable linear memory (a pixel-buffer export); a
        tiled or device-local exporter's fd fails at the Vulkan import.
        """
        ...

    def wait_device_idle(self) -> None: ...

    def escalate(
        self, privileged_callback: Callable[[GpuContextFullAccess], _EscalateResult]
    ) -> _EscalateResult:
        """Refuses: the callback's one atomic privileged scope cannot span a
        process boundary. The operations it wrapped are methods on this
        capability and on `ctx.gpu_limited_access` — call them directly."""
        ...


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
class OpaqueFdTextureExport(Protocol):
    """A raw OPAQUE_FD texture handle: the allocation's memory fd plus the
    allocation-stable shape a foreign Vulkan or CUDA external-memory import
    must reproduce.

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
        """Texture width in pixels."""
        ...

    @property
    def height(self) -> int:
        """Texture height in pixels."""
        ...

    @property
    def format(self) -> str:
        """The engine's format name for the texture, e.g. `"rgba16_float"`."""
        ...

    @property
    def vk_image_tiling(self) -> int:
        """Raw `VkImageTiling` the exporter created the image with."""
        ...

    @property
    def vk_image_usage_flags(self) -> int:
        """Raw `VkImageUsageFlags` bitfield the exporter created the image
        with.
        """
        ...

    @property
    def vk_image_mip_levels(self) -> int:
        """`VkImageCreateInfo.mipLevels` of the exporter's image."""
        ...

    @property
    def vk_image_array_layers(self) -> int:
        """`VkImageCreateInfo.arrayLayers` of the exporter's image."""
        ...

    @property
    def vk_image_samples(self) -> int:
        """Raw `VkSampleCountFlagBits` of the exporter's image."""
        ...

    @property
    def dedicated_allocation(self) -> bool:
        """Whether the allocation is dedicated — always true for this
        flavour. A Vulkan importer chains `VkMemoryDedicatedAllocateInfo`, a
        CUDA importer sets `cudaExternalMemoryDedicated`; omitting either is
        undefined behaviour, not leniency.
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


@runtime_backed_protocol
class ComputeKernel(Protocol):
    """A compute kernel the engine built and holds, dispatched by name.

    Constructed in `setup()` where the capability is Full, dispatched per frame
    in `process()`. No kernel handle string, fence, timeline or slot number
    reaches Python — the object is the handle.
    """

    @property
    def binding_names(self) -> list[str]:
        """The shader's own names for this kernel's bindings, in slot order."""
        ...

    def dispatch(
        self,
        bindings: dict[str, GpuSurfaceHandle | str],
        group_count: tuple[int, int, int],
        push_constants: bytes | None = None,
    ) -> None:
        """Dispatch, binding each of the shader's declared resources by name.

        Bindings never persist on the kernel, so every dispatch supplies all of
        them: there is no implicit default and no value carried over from the
        previous frame. Supplying an unknown name or omitting a declared one
        raises before anything is submitted. Each binding's kind comes from the
        shader's own reflection, never from the caller. A `storage_buffer`
        binding takes a tensor surface from `acquire_storage_buffer`; a
        `uniform_buffer` binding raises naming its kind. Bind the handle, not
        its id string: the dispatch orders the writes torch took through a
        handle's tensor ahead of the kernel's reads.

        Returns when the GPU work has retired and the writes are visible — a
        tensor the dispatch wrote reads back through `torch.from_dlpack`.
        """
        ...


@runtime_backed_protocol
class GraphicsKernel(Protocol):
    """A graphics kernel the engine built and holds, drawn by name.

    Constructed in `setup()` where the capability is Full, drawn per frame in
    `process()`. No kernel handle string, fence, timeline or slot number
    reaches Python — the object is the handle.
    """

    @property
    def binding_names(self) -> list[str]:
        """The shaders' own names for this kernel's bindings, in slot order."""
        ...

    def draw(
        self,
        bindings: dict[str, GpuSurfaceHandle | str],
        color_targets: Sequence[GpuSurfaceHandle | str],
        extent: tuple[int, int],
        vertex_count: int,
        instance_count: int = 1,
        first_vertex: int = 0,
        first_instance: int = 0,
        push_constants: bytes | None = None,
    ) -> None:
        """Render one offscreen pass, binding each declared resource by name.

        Exactly one colour target, `extent` pixels of it. The pass discards
        what the target held and starts from transparent black, so a draw
        paints the whole frame it publishes.

        Bindings never persist on the kernel, so every draw supplies all of
        them. Supplying an unknown name or omitting a declared one raises
        before anything is submitted. Each binding's kind comes from the
        shaders' own reflection, never from the caller. A `storage_buffer`
        binding takes a tensor surface, as `ComputeKernel.dispatch` does.

        Returns when the GPU work has retired and the pixels are visible.
        """
        ...


@runtime_backed_protocol
class RayTracingKernel(Protocol):
    """A ray-tracing kernel the engine built and holds, traced by name.

    Constructed in `setup()` where the capability is Full, traced per frame in
    `process()`. No kernel handle string, fence, timeline or slot number
    reaches Python — the object is the handle.
    """

    @property
    def binding_names(self) -> list[str]:
        """The shaders' own names for this kernel's bindings, in slot order."""
        ...

    def trace(
        self,
        bindings: dict[str, GpuSurfaceHandle | AccelerationStructureHandle | str],
        grid: tuple[int, int, int],
        push_constants: bytes | None = None,
    ) -> None:
        """Trace a `(width, height, depth)` grid of rays.

        An `acceleration_structure` binding takes the handle `build_tlas`
        returned; every other kind takes a surface. Bindings never persist on
        the kernel, so every trace supplies all of them, and an unknown or
        omitted name raises before anything is submitted.

        Returns when the GPU work has retired and the writes are visible.
        """
        ...


@runtime_backed_protocol
class AccelerationStructureHandle(Protocol):
    """An acceleration structure the engine built and holds.

    The object is the handle: a bottom-level structure is placed in a scene by
    `build_tlas`, and the top-level one it returns is what a trace binds. No id
    string reaches Python, and nothing publishes an acceleration structure for
    another node to resolve.

    The engine holds the structure's device memory for as long as this object
    lives, and releases it when the last reference goes away. A scene keeps
    every bottom-level structure it instances alive, so dropping a BLAS a live
    TLAS uses frees nothing until the TLAS goes too.
    """

    @property
    def label(self) -> str:
        """The name this structure was built under, as engine logs show it."""
        ...


@runtime_backed_protocol
class KernelDispatchBatch(Protocol):
    """Several dispatches recorded as one: one submission, one fence wait.

    A two-pass filter dispatching on its own pays the round trip, the
    submission and the stall twice; inside this scope it pays each once.
    Leaving the scope normally runs the batch — leaving it by a raise runs
    nothing, because half of a multi-pass filter is not what the author wrote.

    Nothing about the synchronous contract changes: the scope returns when the
    GPU work has retired and the writes are visible.
    """

    def __enter__(self) -> KernelDispatchBatch: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = None,
        exception: BaseException | None = None,
        traceback: TracebackType | None = None,
    ) -> Literal[False]: ...
    def dispatch(
        self,
        kernel: ComputeKernel,
        bindings: dict[str, GpuSurfaceHandle | str],
        group_count: tuple[int, int, int],
        push_constants: bytes | None = None,
    ) -> None:
        """Add a dispatch to this batch.

        The receiver is explicit because a batch dispatches several kernels;
        `kernel.dispatch()` names its own. Bindings are checked here, so a name
        the shader does not declare or a wrong push-constant size raises at
        this line rather than when the scope closes.

        One kernel may appear only once per batch: a kernel owns a single
        descriptor set, so dispatching it again would give its earlier dispatch
        these bindings.
        """
        ...
