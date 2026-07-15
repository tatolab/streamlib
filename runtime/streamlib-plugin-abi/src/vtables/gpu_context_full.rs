// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextFullAccessVTable` — extern "C" dispatch for privileged GPU work.

use core::ffi::c_void;

use crate::repr::{
    ColorTraitsRepr, ComputeKernelDescriptorRepr, GpuCapabilitiesRepr, GraphicsKernelDescriptorRepr,
    OpaqueFdExportDescriptorRepr, RawWindowHandleRepr, RayTracingKernelDescriptorRepr,
    VideoDecoderSessionDescriptorRepr, VideoEncoderSessionDescriptorRepr,
};

/// Layout version of [`crate::GpuContextFullAccessVTable`].
///
/// The FullAccess vtable carries the privileged kernel-construction
/// surface (compute / graphics / ray-tracing) plus `create_texture_ring`
/// and `acquire_render_target_dma_buf_image`. Each kernel-construction
/// callback consumes a `#[repr(C)]` descriptor mirror
/// ([`crate::ComputeKernelDescriptorRepr`], [`crate::GraphicsKernelDescriptorRepr`],
/// [`crate::RayTracingKernelDescriptorRepr`]) and returns an Arc-handle PluginAbiObject
/// for the resulting kernel; the matching Arc-lifecycle pairs
/// (`clone_*` / `drop_*`) live on this vtable.
///
/// Reachable from cdylib code only inside an `escalate(|full| ...)`
/// scope established via the LimitedAccess vtable's `escalate_begin` /
/// `escalate_end` pair. Each FullAccess callback's `gpu_handle`
/// argument is the opaque scope token issued by `escalate_begin`; the
/// host validates the token against
/// `escalate_scope_registry::with_scope` before dispatch and returns
/// `Error::InvalidEscalateScope` if it's stale or never-issued.
///
/// - v2: Phase C3 adds `acquire_render_target_dma_buf_image` (the
///   one privileged DMA-BUF render-target allocator not yet on this
///   vtable). The four existing `create_*` callbacks keep their wire
///   signatures unchanged but switch from "`gpu_handle` is
///   `*const Arc<GpuContext>`" to "`gpu_handle` is an opaque scope
///   token" semantically — same `*const c_void`, different lookup
///   path on the host side.
/// - v3: Phase D appends the privileged-only FullAccess methods that
///   previously stayed `host_inner`-only:
///   `wait_device_idle`, `acquire_output_texture`,
///   `upload_pixel_buffer_as_texture`, `color_converter`,
///   `create_command_recorder`, `build_triangles_blas`, `build_tlas`,
///   `supports_ray_tracing_pipeline`, `check_in_surface`. The
///   LimitedAccess-mirror FullAccess methods
///   (`acquire_pixel_buffer`, `register_texture_with_layout`, etc.)
///   inherit through the originating LimitedAccess vtable per the
///   `inherited_lim_*` fields on `GpuContextFullAccess` — they do
///   not get parallel slots here.
/// - v4: PluginAbiObject Phase D return types (`RhiColorConverter`,
///   `VulkanAccelerationStructure`, `RhiCommandRecorder`) gain
///   `clone_*` / `drop_*` slot pairs alongside the existing kernel +
///   texture-ring pairs. The slots activate the layout-stable
///   `(handle, vtable)` PluginAbiObject pattern so cdylibs can hold +
///   refcount + drop these handles without rustc-version coupling.
/// - v5: `gpu_capabilities` slot returning a `#[repr(C)]`
///   `GpuCapabilitiesRepr` struct (vendor name + capability bools
///   the camera processor needs to drop `full.device().vulkan_device()`
///   reach-through). Per the AI agent notes on #914: capability
///   queries are read-once-at-setup, so one struct-returning slot
///   amortizes better than per-method slots.
/// - v6: `create_timeline_semaphore(initial_value)` slot returning
///   an `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)` opaque
///   handle (Arc-raw-pointer transit, NOT PluginAbiObject — the timeline
///   semaphore's PluginAbiObject is its own future lift). Camera processor
///   needs this to drop the `full.device().vulkan_device().device()`
///   reach-through for `HostVulkanTimelineSemaphore::new(...)`.
/// - v7: `import_dma_buf_storage_buffer(fd, byte_size)` slot writing
///   a `StorageBuffer` PluginAbiObject (already layout-stable from C1)
///   into `*out_buffer`. Camera processor's V4L2 zero-copy path
///   uses this; the host consumes the fd on success.
/// - v8: `build_triangles_blas` and `build_tlas` signatures extended
///   with three new out-params each (`out_device_address: *mut u64`,
///   `out_storage_size: *mut u64`, `out_kind: *mut u32`). Prior to
///   v8 cdylib mint paths populated the `VulkanAccelerationStructure`
///   PluginAbiObject's cached POD fields with placeholder zeros; the PluginAbiObject
///   getters (`device_address()`, `storage_size()`, `kind()`) then
///   fell back to `host_inner()` which panics in cdylib mode. v8
///   surfaces the real values across the plugin ABI so the cached fields
///   are populated correctly at mint time. **ABI-breaking** — plugins
///   built against v7 are not load-compatible with a v8 host (the fn
///   pointer signatures differ).
/// - v9: `host_vulkan_device_arc()` slot returning an
///   `Arc::into_raw(Arc<HostVulkanDevice>)` raw pointer the cdylib
///   can `Arc::from_raw` to reconstitute the Arc, enabling in-process
///   workspace plugin cdylibs to construct host-flavor surface
///   adapters (`CpuReadbackSurfaceAdapter<HostVulkanDevice>` etc.)
///   for #1004's dlopen smoke tests.
/// - v10: `host_vulkan_texture_arc(texture_handle)` slot returning an
///   `Arc::into_raw(Arc<HostVulkanTexture>)` raw pointer. Second
///   bridge of the cdylib-side adapter-construction chain — gives
///   workspace plugin cdylibs a non-panicking path to
///   `Arc<HostVulkanTexture>` from a `Texture` PluginAbiObject (the existing
///   `Texture::host_inner()` and `HostTextureExt::vulkan_inner()`
///   panic in cdylib mode). Same rustc-version-coupling caveat as
///   v9: `HostVulkanTexture` is not `#[repr(C)]`, so the Arc-raw
///   pointer transit is safe only when the cdylib shares the host's
///   rustc version and dep graph. Subprocess cdylibs don't dep on
///   `streamlib-engine` and can't reach this slot in the first place.
/// - v11: M32 one-shot slot reservation (#1253). Appends the frozen
///   signatures for the milestone's five engine-free surfaces at the
///   @280 tail, all under this single bump (per-surface blocks' append
///   offsets were provisional; final slot order is assigned here):
///   present-target (`create_present_target` / `drop_present_target` —
///   #1258), hardware video (`create_encoder_session` /
///   `drop_encoder_session` / `create_decoder_session` /
///   `drop_decoder_session` — #1259), exportable timeline
///   (`create_exportable_timeline_semaphore` — #1260), texture readback
///   (`create_texture_readback` / `drop_texture_readback` — #1261), and
///   OPAQUE_FD/CUDA (`create_opaque_fd_export_buffer` /
///   `export_storage_buffer_opaque_fd` /
///   `wrap_storage_buffer_as_pixel_buffer` /
///   `copy_texture_to_storage_buffer_and_signal` — #1262). Every
///   reserved slot ships a typed NotYetProvided-style host body under
///   the panic net; the per-surface fill-in issues #1258–#1262 replace
///   those bodies against these frozen slots without touching the
///   struct again. **ABI-breaking** — plugins built against v10 are not
///   load-compatible with a v11 host.
pub const GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION: u32 = 11;

/// Dispatch table for the host's `GpuContextFullAccess`. The cdylib
/// obtains a handle inside an `escalate(|full| ...)` scope (via the
/// `escalate_begin` / `escalate_end` callbacks landing in C3) and
/// reads the static vtable from
/// [`crate::HostServices::gpu_context_full_access_vtable`].
///
/// C2 lands the descriptor wire format + host-side dispatch + cdylib-
/// side PluginAbiObject. C3 wires the `escalate_begin` / `escalate_end` scope-
/// token machinery that makes the methods reachable from cdylib
/// `escalate(|full| { ... })` call sites.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods append
/// to the end and bump
/// [`GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION`].
///
/// # `!Clone` invariant
///
/// `GpuContextFullAccess` is deliberately **not** `Clone` — the
/// privilege scope ends when the `escalate(...)` closure returns. The
/// vtable carries `drop_handle` (host releases the Arc-handle's
/// refcount) but no `clone_handle`, matching the type-level
/// `!Clone` invariant.
#[repr(C)]
pub struct GpuContextFullAccessVTable {
    /// Vtable layout version. Must equal
    /// [`GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Handle lifetime (drop-only — !Clone invariant)
    // -------------------------------------------------------------------------
    /// Release an owned `GpuContextFullAccess` handle. Host runs
    /// `Box::from_raw + drop` on the `Box<Arc<GpuContext>>`-shaped
    /// handle (host mode) or invalidates the C3 scope token. Calling
    /// on a null pointer is a no-op.
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),

    // -------------------------------------------------------------------------
    // VulkanComputeKernel return-type lifetime
    // -------------------------------------------------------------------------
    /// Bump the refcount on a `VulkanComputeKernel` PluginAbiObject handle.
    /// Called by the cdylib's `Clone for VulkanComputeKernel`. Host
    /// runs `Arc::increment_strong_count(handle as *const VulkanComputeKernelInner)`
    /// against the host-internal Inner type — cdylib never sees the
    /// Inner layout.
    pub clone_compute_kernel: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VulkanComputeKernel` PluginAbiObject handle.
    /// Host runs `Arc::decrement_strong_count` against the Inner type.
    pub drop_compute_kernel: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // VulkanGraphicsKernel return-type lifetime
    // -------------------------------------------------------------------------
    /// Bump the refcount on a `VulkanGraphicsKernel` PluginAbiObject handle.
    /// Host runs `Arc::increment_strong_count(handle as *const VulkanGraphicsKernelInner)`.
    pub clone_graphics_kernel: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VulkanGraphicsKernel` PluginAbiObject handle.
    pub drop_graphics_kernel: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // VulkanRayTracingKernel return-type lifetime
    // -------------------------------------------------------------------------
    /// Bump the refcount on a `VulkanRayTracingKernel` PluginAbiObject handle.
    /// Host runs `Arc::increment_strong_count(handle as *const VulkanRayTracingKernelInner)`.
    pub clone_ray_tracing_kernel: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VulkanRayTracingKernel` PluginAbiObject handle.
    pub drop_ray_tracing_kernel: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // TextureRing return-type lifetime
    // -------------------------------------------------------------------------
    /// Bump the refcount on a `TextureRing` PluginAbiObject handle.
    /// Host runs `Arc::increment_strong_count(handle as *const TextureRingInner)`.
    pub clone_texture_ring: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `TextureRing` PluginAbiObject handle.
    pub drop_texture_ring: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // RhiColorConverter return-type lifetime (v4)
    // -------------------------------------------------------------------------
    /// Bump the refcount on a `RhiColorConverter` handle. Host runs
    /// `Arc::increment_strong_count(handle as *const RhiColorConverterInner)`.
    pub clone_color_converter: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `RhiColorConverter` handle.
    pub drop_color_converter: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // VulkanAccelerationStructure return-type lifetime (v4)
    // -------------------------------------------------------------------------
    /// Bump the refcount on a `VulkanAccelerationStructure` handle.
    pub clone_acceleration_structure: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VulkanAccelerationStructure` handle.
    pub drop_acceleration_structure: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // RhiCommandRecorder return-type lifetime (v4)
    // -------------------------------------------------------------------------
    /// Bump the refcount on a `RhiCommandRecorder` handle.
    pub clone_command_recorder: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `RhiCommandRecorder` handle.
    pub drop_command_recorder: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // Kernel construction
    // -------------------------------------------------------------------------
    /// Create a compute kernel from a SPIR-V shader and binding
    /// declaration. On success writes a fresh
    /// `Arc<VulkanComputeKernel>`-shaped opaque handle into
    /// `*out_kernel` and returns 0; on failure writes a UTF-8 error
    /// message into `err_buf` and returns non-zero.
    ///
    /// The `desc` pointer must be valid for the duration of the call.
    /// All inner slice pointers (label, spv, bindings) must likewise
    /// be valid for the duration of the call.
    ///
    /// Linux-only on the host side; non-Linux stubs return non-zero
    /// with a "not available on this platform" message.
    pub create_compute_kernel: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        desc: *const ComputeKernelDescriptorRepr,
        out_kernel: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Create a graphics kernel from a multi-stage SPIR-V set,
    /// binding declaration, and fixed-function pipeline state.
    pub create_graphics_kernel: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        desc: *const GraphicsKernelDescriptorRepr,
        out_kernel: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Create a ray-tracing kernel from shader stages, shader-group
    /// layout, binding declaration, and push-constant range.
    pub create_ray_tracing_kernel: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        desc: *const RayTracingKernelDescriptorRepr,
        out_kernel: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Pre-allocate a ring of `count` non-exportable DEVICE_LOCAL
    /// textures and register each in the same-process texture cache.
    /// `format_raw` is the `#[repr(u32)]` discriminant of
    /// `streamlib_consumer_rhi::TextureFormat`; `usage_bits` is
    /// `TextureUsages::bits()`.
    pub create_texture_ring: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
        usage_bits: u32,
        count: usize,
        out_ring: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Render-target surface allocation (Phase C3 — Linux-only privileged
    // primitive)
    // -------------------------------------------------------------------------
    /// Allocate a render-target-capable DMA-BUF-backed `VkImage` from
    /// the host's privileged surface path. The cdylib's
    /// `GpuContextFullAccess::acquire_render_target_dma_buf_image`
    /// dispatches through this slot inside an active escalate scope;
    /// the host picks a tiled DRM modifier via the EGL probe, runs the
    /// allocation through the RHI's render-target pool, and writes a
    /// fresh `Texture` PluginAbiObject into `*out_texture` on success.
    ///
    /// `format_raw` is the `#[repr(u32)]` discriminant of
    /// `streamlib_consumer_rhi::TextureFormat`. Linux-only; non-Linux
    /// stubs return non-zero with a "not available on this platform"
    /// message.
    pub acquire_render_target_dma_buf_image: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
        out_texture: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Phase D (#906) — privileged-only FullAccess methods that don't appear on
    // LimitedAccess. Each callback validates the `gpu_handle` scope token via
    // the host's `escalate_scope_registry::with_scope` before dispatching to
    // the resolved `Arc<GpuContext>`.
    // -------------------------------------------------------------------------
    /// Block until the GPU device drains every in-flight submission.
    /// Returns 0 on success, non-zero with an error message on failure
    /// (invalid scope token, `vkDeviceWaitIdle` failure, etc.).
    pub wait_device_idle: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Acquire a new output texture with a freshly-minted surface id,
    /// register the texture in the same-process texture cache, and
    /// return both. `out_id_buf` receives the UTF-8 surface id (
    /// `out_id_len` records the byte count; truncation is an error);
    /// `out_texture` receives the [`crate::Texture`] PluginAbiObject.
    pub acquire_output_texture: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
        out_id_buf: *mut u8,
        out_id_cap: usize,
        out_id_len: *mut usize,
        out_texture: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Upload a HOST_VISIBLE pixel buffer's contents to a new GPU
    /// texture and register it under the caller-provided surface id.
    /// Linux-only on the host side; non-Linux stubs return non-zero
    /// with a "not available on this platform" message.
    pub upload_pixel_buffer_as_texture: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        pixel_buffer: *const c_void,
        width: u32,
        height: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Acquire a cached `(src, dst)`-keyed color converter. Writes an
    /// `Arc::into_raw(Arc<RhiColorConverter>)` raw pointer into
    /// `*out_converter` on success. Linux-only.
    pub color_converter: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        src_format_raw: u32,
        dst_format_raw: u32,
        out_converter: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Build an engine-owned multi-step command-buffer recorder.
    /// Writes a [`crate::RhiCommandRecorder`]-shaped value (host's
    /// `RhiCommandRecorder` struct, layout-stable under the rustc-
    /// version coupling contract in CLAUDE.md) into the caller's
    /// `*out_recorder` `MaybeUninit` slot on success. Linux-only.
    pub create_command_recorder: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        label_ptr: *const u8,
        label_len: usize,
        out_recorder: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Build a triangle-geometry bottom-level acceleration structure
    /// from CPU-side vertex + index data. Writes an
    /// `Arc::into_raw(Arc<VulkanAccelerationStructure>)` raw pointer
    /// into `*out_blas` on success, plus the cached POD descriptors
    /// the cdylib's `VulkanAccelerationStructure` PluginAbiObject carries:
    /// `*out_device_address` (BLAS device address — used as the
    /// `accelerationStructureReference` when wiring into a TLAS),
    /// `*out_storage_size` (build-time storage allocation), and
    /// `*out_kind` (0 = `BottomLevel`, 1 = `TopLevel`; always 0 for
    /// `build_triangles_blas`). Linux-only.
    pub build_triangles_blas: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        label_ptr: *const u8,
        label_len: usize,
        vertices_ptr: *const f32,
        vertices_len: usize,
        indices_ptr: *const u32,
        indices_len: usize,
        out_blas: *mut *const c_void,
        out_device_address: *mut u64,
        out_storage_size: *mut u64,
        out_kind: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Build a top-level acceleration structure from BLAS instances.
    /// `instances_ptr` is a `*const TlasInstanceDesc` carrying the
    /// host's `TlasInstanceDesc` struct (layout-stable under the
    /// rustc-version coupling contract). Writes an
    /// `Arc::into_raw(Arc<VulkanAccelerationStructure>)` raw pointer
    /// into `*out_tlas` on success, plus the cached POD descriptors
    /// (`*out_device_address`, `*out_storage_size`, `*out_kind`) the
    /// cdylib's PluginAbiObject carries — same shape as
    /// `build_triangles_blas`; `*out_kind` is always 1 (`TopLevel`)
    /// for `build_tlas`. Linux-only.
    pub build_tlas: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        label_ptr: *const u8,
        label_len: usize,
        instances_ptr: *const c_void,
        instances_len: usize,
        out_tlas: *mut *const c_void,
        out_device_address: *mut u64,
        out_storage_size: *mut u64,
        out_kind: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Whether the underlying GPU exposes the
    /// `VK_KHR_ray_tracing_pipeline` extension chain. Returns 1 = true,
    /// 0 = false, -1 = invalid scope token or other error (with
    /// message in `err_buf`).
    pub supports_ray_tracing_pipeline: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check in a pixel buffer to the surface-share service and return
    /// the surface id. `out_id_buf` receives the UTF-8 id;
    /// `out_id_len` records the byte count (truncation is an error).
    pub check_in_surface: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        pixel_buffer: *const c_void,
        out_id_buf: *mut u8,
        out_id_cap: usize,
        out_id_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // v5 (#914): GPU capability query — read-once-at-setup struct
    // -------------------------------------------------------------------------
    /// Populate a [`crate::GpuCapabilitiesRepr`] with vendor name + capability
    /// bools. Read-once-at-setup pattern: cdylibs (camera, future
    /// plugins) need device-vendor branching and external-memory /
    /// cross-device-DMA-BUF probe checks at processor setup time;
    /// returning a struct amortizes better than per-method bool slots.
    /// Returns 0 on success, non-zero on failure (with message in
    /// `err_buf`).
    pub gpu_capabilities: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_caps: *mut GpuCapabilitiesRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // v6 (#914 / #920): Timeline-semaphore construction primitive
    // -------------------------------------------------------------------------
    /// Construct a timeline semaphore with the given `initial_value`.
    /// On success writes
    /// `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)` into
    /// `*out_handle` and returns 0.
    ///
    /// **Arc-raw-pointer transit** — not a layout-stable PluginAbiObject.
    /// In-tree consumers (camera, display) ride this freely because
    /// they're built in the same workspace as the engine. Cross-repo
    /// plugin distribution will need a PluginAbiObject lift for
    /// `HostVulkanTimelineSemaphore`; tracked as a future follow-up.
    /// Linux-only on the host side; non-Linux stubs return non-zero.
    pub create_timeline_semaphore: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        initial_value: u64,
        out_handle: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // v7 (#914 / #921): V4L2 DMA-BUF FD import as SSBO
    // -------------------------------------------------------------------------
    /// Import a V4L2 (or otherwise externally-allocated) DMA-BUF FD
    /// as a `StorageBuffer` (SSBO-shaped). On success writes the
    /// `StorageBuffer` PluginAbiObject struct (32 bytes, layout-stable from
    /// C1) into `*out_buffer` and returns 0.
    ///
    /// **The host consumes `fd` on success** (`vkImportMemoryFdInfoKHR`
    /// takes ownership). On failure the caller retains ownership and
    /// must close it.
    ///
    /// Linux-only; non-Linux stubs return non-zero.
    pub import_dma_buf_storage_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        fd: i32,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // v8: cdylib-reachable HostVulkanDevice Arc accessor (#1004)
    // -------------------------------------------------------------------------
    /// Clone the host's `Arc<HostVulkanDevice>` and return the raw
    /// `Arc::into_raw` pointer on success.
    ///
    /// Returned pointer is a valid `Arc<HostVulkanDevice>` with the
    /// refcount bumped by 1 — the caller is responsible for calling
    /// `Arc::from_raw` to reconstitute the Arc and matching its
    /// eventual `Drop`. On failure (null scope token, host_inner panic)
    /// returns `std::ptr::null()`.
    ///
    /// **Rustc-version coupling.** `HostVulkanDevice`'s layout isn't
    /// `#[repr(C)]`; the returned pointer is valid only when the cdylib
    /// shares the host's rustc version AND the engine's dep graph (i.e.
    /// in-process workspace plugin cdylibs, like the
    /// `streamlib-test-fixtures` smoke fixtures the #1004 dlopen tests
    /// load). Subprocess cdylibs (`streamlib-python-native`,
    /// `streamlib-deno-native`) don't dep on `streamlib-engine` and
    /// can't import `HostVulkanDevice` to call this slot in the first
    /// place — the type-system gate covers them.
    ///
    /// Used by surface-adapter integration tests where the in-process
    /// plugin cdylib needs to construct a host-flavor
    /// `XxxSurfaceAdapter<HostVulkanDevice>` and exercise its
    /// `acquire_write` → `view_mut` → release path. Production
    /// processor code shouldn't reach for this — the higher-level
    /// FullAccess vtable methods (kernel construction, buffer/texture
    /// allocation) cover the supported plugin ABI surface.
    pub host_vulkan_device_arc: unsafe extern "C" fn(gpu_handle: *const c_void) -> *const c_void,

    // -------------------------------------------------------------------------
    // v10: cdylib-reachable HostVulkanTexture Arc accessor.
    // -------------------------------------------------------------------------
    /// Clone the host's `Arc<HostVulkanTexture>` backing a `Texture`
    /// PluginAbiObject and return the raw `Arc::into_raw` pointer on success.
    ///
    /// `texture_handle` is the same opaque `Arc::into_raw(Arc<TextureInner>)`
    /// pointer cached on the `Texture` PluginAbiObject's `handle` field; the
    /// host dereferences it without taking ownership, clones the
    /// inner `Arc<HostVulkanTexture>`, and returns its raw pointer
    /// with the strong count bumped by 1. The caller is responsible
    /// for `Arc::from_raw` to reconstitute the Arc and matching its
    /// eventual `Drop`. On a null `texture_handle` (or any host-side
    /// failure caught by `catch_unwind`) returns `std::ptr::null()`.
    ///
    /// **Rustc-version coupling.** `HostVulkanTexture`'s layout isn't
    /// `#[repr(C)]`; the returned pointer is valid only when the cdylib
    /// shares the host's rustc version AND the engine's dep graph
    /// (workspace plugin cdylibs do; subprocess cdylibs don't dep on
    /// `streamlib-engine` so they can't reach `HostVulkanTexture` in
    /// the first place).
    ///
    /// Used by the dlopen smoke fixtures for the OpenGL / Skia /
    /// Vulkan surface adapters, which need to call
    /// `XxxSurfaceAdapter::register_host_surface` with a real
    /// `Arc<HostVulkanTexture>` to exercise `acquire_*` paths.
    pub host_vulkan_texture_arc:
        unsafe extern "C" fn(texture_handle: *const c_void) -> *const c_void,

    // =========================================================================
    // v11 (M32 #1253) — one-shot slot reservation for the five engine-free
    // surfaces. Slot order + offsets assigned here at aggregation; every
    // per-surface block's provisional @280 tail collapses under this single
    // bump. Each body ships a typed NotYetProvided-style stub until its
    // surface fill-in issue (#1258–#1262) lands the real host body.
    // =========================================================================

    // ---- Present target (#1258) ----
    /// Create a swapchain-backed present target from a native window
    /// handle. Re-materializes `raw_window_handle::{RawWindowHandle,
    /// RawDisplayHandle}` from the repr, constructs the host
    /// `VulkanPresentTarget`, and writes the Box-shaped `PresentTarget`
    /// PluginAbiObject into `*out_present_target`. `color` null = legacy
    /// SDR pick. Linux-only; the reserved Win32/AppKit discriminants and
    /// non-Linux hosts return the typed not-yet-provided error.
    pub create_present_target: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        window: *const RawWindowHandleRepr,
        width: u32,
        height: u32,
        vsync: u32,
        color: *const ColorTraitsRepr,
        out_present_target: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Release an owned `PresentTarget` handle (`Box::from_raw` + drop of
    /// `Box<VulkanPresentTarget>`, no-op on null). Single drop-only slot
    /// — `VulkanPresentTarget` is single-owner `!Clone`, no clone slot.
    pub drop_present_target: unsafe extern "C" fn(owned_handle: *const c_void),

    // ---- Hardware video encode/decode (#1259) ----
    /// Mint a `SimpleEncoder` on the host device and write the Box-shaped
    /// handle into `*out_session` plus the two cached aligned-extent POD
    /// out-params (RGBA input to `submit_texture` must be >= these).
    /// Linux-only. `disable_gpu_input_prealloc` in the descriptor gates
    /// the eager `prepare_gpu_encode_resources` call.
    pub create_encoder_session: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        desc: *const VideoEncoderSessionDescriptorRepr,
        out_session: *mut *const c_void,
        out_aligned_width: *mut u32,
        out_aligned_height: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Release an owned encoder session: `Box::from_raw(Box<SimpleEncoder>)`
    /// then drop (`wait_idle` plus spec-ordered teardown). No-op on null.
    /// No clone slot (`!Clone` single-owner GPU pipeline). May be called
    /// outside an escalate scope; the session Box owns its own device
    /// Arc.
    pub drop_encoder_session: unsafe extern "C" fn(owned_handle: *const c_void),

    /// Mint a `SimpleDecoder` on the host device and write the Box-shaped
    /// handle into `*out_session`. Dimensions are auto-detected from the
    /// SPS (query via the `dimensions` methods slot after feed).
    /// Linux-only.
    pub create_decoder_session: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        desc: *const VideoDecoderSessionDescriptorRepr,
        out_session: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Release an owned decoder session
    /// (`Box::from_raw(Box<SimpleDecoder>)` + drop). No-op on null. No
    /// clone slot.
    pub drop_decoder_session: unsafe extern "C" fn(owned_handle: *const c_void),

    // ---- Exportable timeline semaphore (#1260) ----
    /// Construct an OPAQUE_FD-exportable timeline semaphore with the
    /// given `initial_value` and write a fully-initialized
    /// `HostTimelineSemaphore` PluginAbiObject (16 bytes; `handle` +
    /// host-static `methods` vtable pointer) into `*out_timeline`.
    /// Distinct from v6 `create_timeline_semaphore` (non-exportable,
    /// Arc-raw transit, in-process only). Linux-only.
    pub create_exportable_timeline_semaphore: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        initial_value: u64,
        out_timeline: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // ---- Texture readback (#1261) ----
    /// Mint a single-in-flight `VulkanTextureReadback` and write the
    /// Box-shaped opaque handle into `*out_readback_handle` plus the
    /// cached-POD out-params (`out_handle_id` — process-wide handle id;
    /// `out_staging_size` — the primitive's own `staging_size()`, never
    /// recomputed ABI-side). Planar formats (Nv12) are rejected with a
    /// typed error. Linux-only; drop-only (`drop_texture_readback`).
    pub create_texture_readback: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        label_ptr: *const u8,
        label_len: usize,
        width: u32,
        height: u32,
        format_raw: u32,
        out_readback_handle: *mut *const c_void,
        out_handle_id: *mut u64,
        out_staging_size: *mut u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Release an owned texture-readback handle
    /// (`Box::from_raw(Box<Arc<VulkanTextureReadback>>)` + drop; the
    /// primitive's Drop can block on the pending timeline). No-op on
    /// null. No clone slot.
    pub drop_texture_readback: unsafe extern "C" fn(handle: *const c_void),

    // ---- OPAQUE_FD / CUDA buffer surface (#1262) ----
    /// Allocate an OPAQUE_FD-exportable `VkBuffer` on the host device
    /// (`device_local = 1` → `new_opaque_fd_export_device_local`,
    /// CUDA-visible; `0` → `new_opaque_fd_export`) and write the existing
    /// Arc-shaped `StorageBuffer` PluginAbiObject (32 bytes) in-place into
    /// `*out_buffer`. Clone/drop reuse the existing LimitedAccess
    /// `clone_storage_buffer` / `drop_storage_buffer`. Linux-only.
    pub create_opaque_fd_export_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        byte_size: u64,
        device_local: u8,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Export a fresh dup'd OPAQUE_FD (`vkGetMemoryFdKHR`) plus size +
    /// exporting-device UUID from a borrowed `StorageBuffer`, writing the
    /// [`OpaqueFdExportDescriptorRepr`]. FD ownership transfers to the
    /// caller on success; `fd` is written `-1` on any non-zero return.
    /// Call-once-per-import. Linux-only.
    pub export_storage_buffer_opaque_fd: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        buffer: *const c_void,
        out_descriptor: *mut OpaqueFdExportDescriptorRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Wrap an existing `StorageBuffer` (flat `VkBuffer`) as a
    /// `PixelBuffer` PluginAbiObject (`PixelBuffer::from_host_vulkan_buffer`;
    /// both wrap the same Arc) so the flat CUDA buffer registers via the
    /// existing `register_pixel_buffer_with_timeline` SurfaceStore slot.
    /// Writes the `PixelBuffer` in-place into `*out_pixel_buffer`.
    /// Linux-only.
    pub wrap_storage_buffer_as_pixel_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        storage_buffer: *const c_void,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format_raw: u32,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Per-frame CUDA producer copy: in one host-device submission,
    /// GPU-wait on `consume_done` at `consume_done_wait_value` (null =
    /// none), `vkCmdCopyImageToBuffer` from the borrowed source
    /// `texture_handle` (at `source_layout_raw`) into `storage_buffer`,
    /// then signal `produce_done` at `produce_done_signal_value` (null =
    /// none). Timeline params carry `HostTimelineSemaphore.handle` (the
    /// inner Arc pointer). Linux-only.
    ///
    /// (#1262 Decision 4 directed the full frozen signature to be
    /// produced at aggregation; this is the aggregator-assigned shape
    /// carrying both the wait and signal values per the binding
    /// amendment.)
    pub copy_texture_to_storage_buffer_and_signal: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        texture_handle: *const c_void,
        source_layout_raw: i32,
        storage_buffer: *const c_void,
        consume_done_handle: *const c_void,
        consume_done_wait_value: u64,
        produce_done_handle: *const c_void,
        produce_done_signal_value: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for GpuContextFullAccessVTable {}
unsafe impl Sync for GpuContextFullAccessVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn gpu_context_full_access_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 47 fn
        // pointers (8 bytes each) = 4 + 4 + 376 = 384 bytes, align = 8.
        //
        // 34 pre-v11 entries = 1 drop_handle + 7 clone/drop pairs (14 fn
        // pointers for the 7 PluginAbiObject return types: compute / graphics /
        // ray-tracing kernels, texture ring, color converter,
        // acceleration structure, command recorder) + 4 create_* method
        // callbacks (compute / graphics / ray-tracing / texture_ring)
        // + 1 acquire_render_target_dma_buf_image + 9 privileged methods
        // (wait_device_idle, acquire_output_texture,
        // upload_pixel_buffer_as_texture, color_converter,
        // create_command_recorder, build_triangles_blas, build_tlas,
        // supports_ray_tracing_pipeline, check_in_surface)
        // + 1 gpu_capabilities + 1 create_timeline_semaphore
        // + 1 import_dma_buf_storage_buffer + 1 host_vulkan_device_arc
        // + 1 host_vulkan_texture_arc.
        //
        // v11 (#1253) appends 13 slots @280..376: create/drop
        // present_target (2), create/drop encoder + decoder session (4),
        // create_exportable_timeline_semaphore (1), create/drop
        // texture_readback (2), create_opaque_fd_export_buffer +
        // export_storage_buffer_opaque_fd +
        // wrap_storage_buffer_as_pixel_buffer +
        // copy_texture_to_storage_buffer_and_signal (4).
        assert_eq!(size_of::<GpuContextFullAccessVTable>(), 384);
        assert_eq!(align_of::<GpuContextFullAccessVTable>(), 8);
        assert_eq!(offset_of!(GpuContextFullAccessVTable, layout_version), 0);
        assert_eq!(offset_of!(GpuContextFullAccessVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(GpuContextFullAccessVTable, drop_handle), 8);
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, clone_compute_kernel),
            16
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_compute_kernel),
            24
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, clone_graphics_kernel),
            32
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_graphics_kernel),
            40
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, clone_ray_tracing_kernel),
            48
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_ray_tracing_kernel),
            56
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, clone_texture_ring),
            64
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_texture_ring),
            72
        );
        // v4-added PluginAbiObject clone/drop pairs (#917).
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, clone_color_converter),
            80
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_color_converter),
            88
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, clone_acceleration_structure),
            96
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_acceleration_structure),
            104
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, clone_command_recorder),
            112
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_command_recorder),
            120
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_compute_kernel),
            128
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_graphics_kernel),
            136
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_ray_tracing_kernel),
            144
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_texture_ring),
            152
        );
        // C3-added entry (Phase C3, #903).
        assert_eq!(
            offset_of!(
                GpuContextFullAccessVTable,
                acquire_render_target_dma_buf_image
            ),
            160
        );
        // Phase D entries (#906).
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, wait_device_idle),
            168
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, acquire_output_texture),
            176
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, upload_pixel_buffer_as_texture),
            184
        );
        assert_eq!(offset_of!(GpuContextFullAccessVTable, color_converter), 192);
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_command_recorder),
            200
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, build_triangles_blas),
            208
        );
        assert_eq!(offset_of!(GpuContextFullAccessVTable, build_tlas), 216);
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, supports_ray_tracing_pipeline),
            224
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, check_in_surface),
            232
        );
        // v5 (#914): gpu_capabilities.
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, gpu_capabilities),
            240
        );
        // v6 (#914 / #920): create_timeline_semaphore.
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_timeline_semaphore),
            248
        );
        // v7 (#914 / #921): import_dma_buf_storage_buffer.
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, import_dma_buf_storage_buffer),
            256
        );
        // v9 (#1004): host_vulkan_device_arc.
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, host_vulkan_device_arc),
            264
        );
        // v10: host_vulkan_texture_arc.
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, host_vulkan_texture_arc),
            272
        );
        // v11 (#1253) — final slot order assigned at aggregation.
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_present_target),
            280
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_present_target),
            288
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_encoder_session),
            296
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_encoder_session),
            304
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_decoder_session),
            312
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_decoder_session),
            320
        );
        assert_eq!(
            offset_of!(
                GpuContextFullAccessVTable,
                create_exportable_timeline_semaphore
            ),
            328
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_texture_readback),
            336
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_texture_readback),
            344
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, create_opaque_fd_export_buffer),
            352
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, export_storage_buffer_opaque_fd),
            360
        );
        assert_eq!(
            offset_of!(
                GpuContextFullAccessVTable,
                wrap_storage_buffer_as_pixel_buffer
            ),
            368
        );
        assert_eq!(
            offset_of!(
                GpuContextFullAccessVTable,
                copy_texture_to_storage_buffer_and_signal
            ),
            376
        );
    }
}
