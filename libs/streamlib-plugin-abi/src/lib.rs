// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pure ABI contract for StreamLib's dynamic plugin system.
//!
//! Loosely analogous to Unreal's `IModuleInterface` or VST3's audio-
//! plugin spec: a `#[repr(C)]` wire-protocol header that lets a host
//! binary and a dlopen'd Rust cdylib communicate **without sharing
//! any Rust types beyond primitives and `extern "C" fn` pointers**.
//!
//! The deployment model this enables: computer A builds the host
//! binary, computer B builds packages via CI, computer C ships their
//! own packages — all using different rustc minor versions and
//! different dep resolutions, all interoperating, as long as they
//! target the same triple and pin the same [`STREAMLIB_ABI_VERSION`].
//! No commit-level coupling, no shared Cargo.lock.
//!
//! # What crosses the wire
//!
//! The host fills out a [`HostServices`] struct with `extern "C" fn`
//! pointers that bridge every process-wide service the plugin's
//! statically-linked engine copy would otherwise see in isolation:
//! tracing emit, PUBSUB publish, schema-registry register / lookup,
//! iceoryx2-log emit. Cdylib registration of processor types crosses
//! via [`HostServices::processor_register`], which carries a msgpack-
//! encoded `ProcessorDescriptor` plus a [`ProcessorVTable`] of
//! extern "C" fn pointers covering the full host-called
//! `DynGeneratedProcessor` surface — constructor + lifecycle plus
//! iceoryx2 wiring, execution-config, and config-json IO.
//!
//! # Example plugin
//!
//! ```ignore
//! use streamlib::prelude::*;
//! use streamlib_plugin_abi::export_plugin;
//!
//! #[streamlib::sdk::processor(execution = Continuous)]
//! pub struct MyProcessor {
//!     #[streamlib::sdk::processors::input(description = "Video input")]
//!     video_in: LinkInput<VideoFrame>,
//! }
//!
//! impl ContinuousProcessor for MyProcessor::Processor {
//!     fn process(&mut self) -> Result<()> {
//!         if let Some(frame) = self.video_in.read() { /* ... */ }
//!         Ok(())
//!     }
//! }
//!
//! export_plugin!(MyProcessor::Processor);
//! ```
//!
//! # Plugin Cargo.toml
//!
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [dependencies]
//! streamlib = "0.2"
//! streamlib-plugin-abi = "0.2"
//! ```

use core::ffi::c_void;

// =============================================================================
// Wire ABI version
// =============================================================================

/// Current ABI version. Plugins must match this exactly at load time.
/// Bumped when the wire shape of [`PluginDeclaration`], the register
/// callback's signature, or [`HostServices`]'s layout changes
/// incompatibly. Same-major-version layout additions append to the
/// end of [`HostServices`] and read the new fields only when
/// `abi_layout_version` advertises them.
pub const STREAMLIB_ABI_VERSION: u32 = 4;

/// Layout version of the [`HostServices`] payload. Read first by the
/// cdylib's `install_host_services` before any other field is
/// touched. Bumped whenever fields are added, removed, or reordered.
/// Distinct from [`STREAMLIB_ABI_VERSION`] because layout-only
/// additions can ship without bumping the wire ABI.
///
/// - v1: tracing / PUBSUB / schema / iceoryx2-log callbacks +
///   `processor_registry_typed` typed pointer.
/// - v2: `processor_registry_typed` removed; replaced with
///   [`HostServices::processor_register`] callback + [`ProcessorVTable`].
///   Async-lifecycle wrappers grab the tokio handle from
///   `ctx.tokio_handle()` rather than via a separate callback.
/// - v3: [`RuntimeContextVTable`] + [`AudioClockVTable`] +
///   [`RuntimeOpsVTable`] references appended. The
///   shared-type `tokio::runtime::Handle` crossing is eliminated:
///   plugins own their own tokio runtimes; the host's runtime is
///   not exposed to plugins. Lifecycle methods are synchronous at
///   the trait surface; the host's lifecycle wrappers no longer
///   `block_on`.
/// - v4: [`GpuContextLimitedAccessVTable`] reference appended.
///   The cdylib-side `GpuContextLimitedAccess` shim's
///   `(handle, vtable)` pair sources its vtable pointer from this
///   field. Non-null for hosts that ship a GpuContext; null
///   otherwise (cdylib code must check before dispatching).
/// - v5: [`SurfaceStoreVTable`] reference appended. The cdylib-side
///   `SurfaceStore` shim's `(handle, vtable)` pair sources its
///   vtable pointer from this field. Non-null for hosts that ship
///   a `SurfaceStore`; null otherwise (cdylib code must check
///   before dispatching).
/// - v6: [`GpuContextFullAccessVTable`] reference appended. The
///   cdylib-side `GpuContextFullAccess` shim's `(handle, vtable)`
///   pair sources its vtable pointer from this field. Non-null for
///   hosts that ship a GpuContext; null otherwise (cdylib code must
///   check before dispatching). Reachable from cdylib code only
///   inside an `escalate(|full| ...)` scope (the scope-token
///   machinery lands in C3 — Phase C2 ships the vtable layout +
///   host wiring + cdylib β-shape, locking the wire format before
///   the scope machinery turns it on).
/// - v12: [`RhiColorConverterMethodsVTable`] reference appended.
///   The cdylib-side `RhiColorConverter` β-shape's `methods_vtable`
///   field sources its pointer from this field. Non-null for hosts
///   that ship a GpuContext; null otherwise (cdylib code must check
///   before dispatching). Phase E sub-lift slice A wires the
///   `prepare_buffer_to_image_storage` method through it so cdylib
///   camera processors can prepare a color-conversion kernel without
///   tripping the host-mode-only `host_inner()` panic.
/// - v13: [`RhiCommandRecorderMethodsVTable`] reference appended.
///   The cdylib-side `RhiCommandRecorder` β-shape's `methods_vtable`
///   field sources its pointer from this field. Non-null for hosts
///   that ship a GpuContext; null otherwise (cdylib code must check
///   before dispatching). Phase E sub-lift slice B wires the six
///   camera-hot-path methods (`begin`, `record_image_barrier`,
///   `record_buffer_barrier`, `record_dispatch`,
///   `record_copy_image_to_buffer`, `submit_signaling_timeline`)
///   through it so cdylib camera processors can drive the
///   host-owned recorder per frame without tripping the
///   host-mode-only `host_inner_mut()` panic.
/// - v14: [`OutputWriterVTable`] + [`InputMailboxesVTable`]
///   references appended (issue #894 — LAST shared-Rust-type
///   crossings in the plugin ABI). The cdylib's β-shape
///   `OutputWriter` / `InputMailboxes` field types source their
///   vtable pointers from these slots; per-frame `write_raw` /
///   `read_raw` dispatch through them. Paired with the
///   `set_iceoryx2_resources` slot on `ProcessorVTable` v2 which
///   delivers the per-instance opaque handles. Non-null for every
///   host that wires processors through iceoryx2.
pub const HOST_SERVICES_LAYOUT_VERSION: u32 = 14;

/// Layout version of the [`ProcessorVTable`] struct. Read by the
/// host's `processor_register` impl before dereferencing any vtable
/// entry; mismatching versions abort the registration cleanly.
///
/// - v1: 17 fn pointer slots including `get_iceoryx2_output_writer_arc`
///   and `get_iceoryx2_input_mailboxes_mut` returning shared Rust types
///   (the host coupled to the cdylib's `streamlib-engine` source
///   version through `Arc<OutputWriter>` / `&mut InputMailboxes`
///   layout).
/// - v2: issue #894 retires both shared-type crossings. The two
///   `get_iceoryx2_*` slots are removed and a single
///   `set_iceoryx2_resources` slot is added. The host now allocates
///   `OutputWriterInner` and `InputMailboxesInner` and hands the
///   cdylib `(handle, vtable)` β-shapes via the new slot; the
///   per-frame `write_raw` / `read_raw` calls dispatch through
///   [`OutputWriterVTable`] / [`InputMailboxesVTable`]. **ABI-
///   breaking** — plugins built against v1 are not load-compatible
///   with a v2 host (the slot count and offsets differ).
pub const PROCESSOR_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Layout version of [`RuntimeContextVTable`]. Pinned at offset 0;
/// newer fields append to the end and bump this constant.
pub const RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Layout version of [`AudioClockVTable`].
pub const AUDIO_CLOCK_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Layout version of [`RuntimeOpsVTable`].
///
/// - v1: 5 submit-with-completion ops (`add_processor` /
///   `remove_processor` / `connect` / `disconnect` / `to_json`). Handle
///   lifetime was a borrow into RuntimeContext-owned storage; a shim
///   stashed past `Runner::stop()` would dangle (sound today because
///   nothing stashes; type signature didn't encode it).
/// - v2: added `clone_handle` / `drop_handle` for owning-Arc semantics.
///   The cdylib-side `RuntimeOpsShim` now holds an Arc-bumped owned
///   handle and releases it via `drop_handle` in its Drop impl,
///   keeping the host's `Arc<dyn RuntimeOperations>` alive for the
///   shim's lifetime independently of `RuntimeContext`'s lifetime.
pub const RUNTIME_OPS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Layout version of [`SurfaceStoreVTable`].
pub const SURFACE_STORE_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Layout version of [`GpuContextLimitedAccessVTable`].
///
/// Every Arc-holding return type on the cdylib-facing surface
/// (`PixelBuffer`, `Texture`, `PooledTextureHandle`, 4 Linux-only
/// buffer types, `TextureRegistration`, `RhiCommandQueue`,
/// `CommandBuffer`, `SurfaceStore`) carries its own clone/drop
/// callback pair so refcount accounting runs in host-compiled
/// code regardless of caller DSO. Method-dispatch callbacks
/// cover every cdylib-callable inherent method on
/// `GpuContextLimitedAccess`.
///
/// `CommandBuffer` and `PooledTextureHandle` are intentionally
/// NOT `Clone` — `CommandBuffer` has consume-semantics
/// `commit(self)` / `commit_and_wait(self)` (the cdylib nulls
/// the local handle/vtable fields after dispatch so Drop becomes
/// a no-op); `PooledTextureHandle::Drop` releases the underlying
/// pool slot. Linux-only callbacks ship platform stubs on other
/// triples so the vtable layout stays unconditional.
///
/// - v10: Phase C3 adds `escalate_begin` / `escalate_end` so the
///   cdylib-side `GpuContextLimitedAccess::escalate(|full| ...)` can
///   acquire the host's escalate gate, mint an opaque scope token,
///   and pair it with the FullAccess vtable for the
///   vtable-dispatched transition into `GpuContextFullAccess`.
///   Validation of the
///   scope token on every FullAccess vtable call lives on the
///   FullAccess vtable side (each callback short-circuits to
///   `Error::InvalidEscalateScope` when the token is stale).
/// - v11: Phase F (#908 / #957) adds `texture_native_dma_buf_fd`
///   for the cdylib-facing
///   [`crate::core::rhi::Texture::native_handle`] DMA-BUF FD export
///   path. Real cdylib use case: subprocess adapters that need to
///   hand a `Texture`'s DMA-BUF FD to a different GPU API (CUDA,
///   OpenGL, downstream IPC) without falling through `host_inner()`
///   and panicking. Returns the FD widened to `i64`; `-1` encodes
///   `Option::None`. Non-Linux hosts return `-1` unconditionally;
///   the macOS / Windows native-handle variants are deferred per
///   #908's AI Agent Notes.
/// - v12: #958 follow-up to #914 — adds
///   `set_video_source_timeline_semaphore` /
///   `clear_video_source_timeline_semaphore` slots. The camera
///   processor (loaded as a cdylib via `runtime.load_project`)
///   publishes its `Arc<HostVulkanTimelineSemaphore>` for in-process
///   display consumers to wait on; #971 originally panic-guarded
///   these methods on the premise no cdylib reaches them, but the
///   camera-as-cdylib lifecycle established by #914 does in fact
///   call them. Wire format mirrors the LimitedAccess Arc-borrow
///   pattern from `register_texture`: the cdylib passes
///   `Arc::as_ptr(&timeline) as *const c_void`; the host
///   `Arc::increment_strong_count` + `Arc::from_raw`s a temporary
///   borrow, calls `set_video_source_timeline_semaphore(&arc)`
///   (which itself clones into the slot), then lets the temporary
///   drop. The clear variant is a void no-arg callback. Linux-only
///   on the host side; non-Linux stubs are no-ops.
/// - v13: #958 Phase E sub — adds `wait_timeline_semaphore` slot.
///   Lets the cdylib-side `HostVulkanTimelineSemaphore::wait` —
///   used per-frame by the camera processor on its capture
///   timeline — dispatch through the host instead of touching the
///   host's `vulkanalia::Device` from cdylib code directly.
///   `timeline_handle` is `Arc::as_ptr(timeline) as *const c_void`
///   (borrowed, same shape as `set_video_source_timeline_semaphore`).
///   Returns 0 on success, non-zero (`err_buf` populated) on driver
///   failure / timeout. Linux-only on the host side.
pub const GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION: u32 = 13;

/// Layout version of [`GpuContextFullAccessVTable`].
///
/// The FullAccess vtable carries the privileged kernel-construction
/// surface (compute / graphics / ray-tracing) plus `create_texture_ring`
/// and `acquire_render_target_dma_buf_image`. Each kernel-construction
/// callback consumes a `#[repr(C)]` descriptor mirror
/// ([`ComputeKernelDescriptorRepr`], [`GraphicsKernelDescriptorRepr`],
/// [`RayTracingKernelDescriptorRepr`]) and returns an Arc-handle β-shape
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
/// - v4: β-shape Phase D return types (`RhiColorConverter`,
///   `VulkanAccelerationStructure`, `RhiCommandRecorder`) gain
///   `clone_*` / `drop_*` slot pairs alongside the existing kernel +
///   texture-ring pairs. The slots activate the layout-stable
///   `(handle, vtable)` β-shape pattern so cdylibs can hold +
///   refcount + drop these handles without rustc-version coupling.
/// - v5: `gpu_capabilities` slot returning a `#[repr(C)]`
///   `GpuCapabilitiesRepr` struct (vendor name + capability bools
///   the camera processor needs to drop `full.device().vulkan_device()`
///   reach-through). Per the AI agent notes on #914: capability
///   queries are read-once-at-setup, so one struct-returning slot
///   amortizes better than per-method slots.
/// - v6: `create_timeline_semaphore(initial_value)` slot returning
///   an `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)` opaque
///   handle (Arc-raw-pointer transit, NOT β-shape — the timeline
///   semaphore's β-shape is its own future lift). Camera processor
///   needs this to drop the `full.device().vulkan_device().device()`
///   reach-through for `HostVulkanTimelineSemaphore::new(...)`.
/// - v7: `import_dma_buf_storage_buffer(fd, byte_size)` slot writing
///   a `StorageBuffer` β-shape (already layout-stable from C1)
///   into `*out_buffer`. Camera processor's V4L2 zero-copy path
///   uses this; the host consumes the fd on success.
/// - v8: `build_triangles_blas` and `build_tlas` signatures extended
///   with three new out-params each (`out_device_address: *mut u64`,
///   `out_storage_size: *mut u64`, `out_kind: *mut u32`). Prior to
///   v8 cdylib mint paths populated the `VulkanAccelerationStructure`
///   β-shape's cached POD fields with placeholder zeros; the β-shape
///   getters (`device_address()`, `storage_size()`, `kind()`) then
///   fell back to `host_inner()` which panics in cdylib mode. v8
///   surfaces the real values across the FFI so the cached fields
///   are populated correctly at mint time. **ABI-breaking** — plugins
///   built against v7 are not load-compatible with a v8 host (the fn
///   pointer signatures differ).
pub const GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION: u32 = 8;

/// Layout version of [`TextureRingMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only.
/// - v2: `TextureRingSlot` β-shape lands (fixed-size POD
///   `surface_id_bytes: [u8; 64]` + `surface_id_len: u32` +
///   `slot_index: u32`, replacing the heap `String` surface_id) and
///   the method slots `acquire_next` / `copy_pixel_buffer_to_slot` /
///   `slot` get wired through. Each cross-DSO call uses caller-
///   provided out-parameter buffers for the slot's typed POD bytes;
///   the slot's `texture` β-shape is itself a `(handle, vtable,
///   POD)` triple cloned through its own per-type Clone vtable.
pub const TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Layout version of [`VulkanComputeKernelMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only.
/// - v2: appended `set_push_constants` / `dispatch` slots (primitive
///   arguments only).
/// - v3: appended typed binding-method slots
///   `set_storage_buffer_pixel` / `set_storage_buffer_storage` /
///   `set_uniform_buffer` / `set_sampled_texture` /
///   `set_storage_image`. Each carries the matching plugin-handle's
///   raw `Arc::into_raw` pointer; the host wrapper reconstructs the
///   borrow and forwards to the inner kernel. Buffer slots are typed
///   by Rust wrapper to mirror streamlib's typed-wrapper binding-site
///   contract.
pub const VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION: u32 = 3;

/// Layout version of [`VulkanGraphicsKernelMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only.
/// - v2: appended typed binding-method slots
///   `set_storage_buffer_pixel` / `set_storage_buffer_storage` /
///   `set_uniform_buffer` / `set_sampled_texture` /
///   `set_storage_image` / `set_vertex_buffer` / `set_index_buffer`
///   plus the primitive-argument slots `set_push_constants` /
///   `offscreen_render`. Each binding slot carries the matching
///   plugin-handle's raw `Arc::into_raw` pointer; the host wrapper
///   reconstructs the borrow and forwards to the inner kernel.
///   Buffer slots are typed by Rust wrapper to mirror streamlib's
///   typed-wrapper binding-site contract (same shape as the
///   compute-kernel methods vtable v3).
pub const VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Layout version of [`VulkanRayTracingKernelMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only.
/// - v2: appended typed binding-method slots
///   `set_acceleration_structure` / `set_storage_buffer_pixel` /
///   `set_storage_buffer_storage` / `set_uniform_buffer` /
///   `set_sampled_texture` / `set_storage_image` plus the
///   primitive-argument slots `set_push_constants` / `trace_rays`.
///   Each binding slot carries the matching plugin-handle's raw
///   `Arc::into_raw` pointer; the host wrapper reconstructs the
///   borrow and forwards to the inner kernel. Buffer slots are
///   typed by Rust wrapper to mirror streamlib's typed-wrapper
///   binding-site contract (same shape as the compute-kernel
///   methods vtable v3). Ray-tracing kernels are serial — like
///   compute, they own a single command buffer + fence and have no
///   `frame_index` argument on any slot.
pub const VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Layout version of [`VulkanAccelerationStructureMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only (issue #907 Phase E
///   PR 5/5).
/// - v2: appended `label` slot returning the AS's human-readable
///   label via a caller-provided byte buffer (same shape as
///   `TextureRingSlot.surface_id` from #947 — `String` layout
///   isn't cdylib-safe). `vk_handle` stays host-only — the
///   `vk::AccelerationStructureKHR` is a vulkanalia handle that
///   can't safely cross a DSO boundary without vulkanalia
///   version coupling, and no in-tree cdylib consumer reads it.
///   The POD getters (`device_address`, `storage_size`, `kind`)
///   are populated at mint time via the v8
///   [`GpuContextFullAccessVTable::build_triangles_blas`] /
///   `build_tlas` out-params and don't need vtable slots.
pub const VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Layout version of [`RhiColorConverterMethodsVTable`].
///
/// - v1: ships the `prepare_buffer_to_image_storage` slot — the
///   minimum surface a cdylib camera processor needs to dispatch
///   YCbCr→RGBA conversion through the host's cached buffer→image
///   kernel without panicking at the β-shape's host-mode-only
///   `host_inner()` access. Out-params return an opaque
///   `Arc<VulkanComputeKernelInner>`-shaped handle plus the kernel's
///   `push_constant_size` POD so the cdylib can reconstruct a
///   `VulkanComputeKernel` β-shape via the parent FullAccess vtable's
///   per-type methods vtable lookup.
pub const RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Layout version of [`RhiCommandRecorderMethodsVTable`].
///
/// - v1: ships six method slots a cdylib camera processor needs
///   to drive the host-owned `RhiCommandRecorder` per frame —
///   `begin`, `record_image_barrier`, `record_buffer_barrier`,
///   `record_dispatch`, `record_copy_image_to_buffer`,
///   `submit_signaling_timeline`. Without these the β-shape's
///   `host_inner()` / `host_inner_mut()` panic-guards fire from
///   cdylib code on every per-frame call.
/// - v2: appends two PixelBuffer-flavored sibling slots —
///   `record_pixel_buffer_barrier` and
///   `record_copy_image_to_pixel_buffer` — so cdylibs can barrier
///   and copy-image-to into a `PixelBuffer` destination. The v1
///   StorageBuffer-flavored slots are unchanged; the new slots
///   are appended at the end of the struct. This is the
///   "sibling-slot per buffer flavor" pattern documented on
///   `RhiCommandRecorderMethodsVTable` and already used by
///   `VulkanGraphicsKernelMethodsVTable`'s
///   `set_storage_buffer_pixel` / `set_storage_buffer_storage`
///   pair.
pub const RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Layout version of [`OutputWriterVTable`].
///
/// - v1: ships the four slots a cdylib processor's `OutputWriter`
///   β-shape needs to dispatch every public-API call through the
///   host: `write_raw` (the per-frame hot-path emit), `has_port`
///   (configuration query), `clone_arc` / `drop_arc`
///   (refcount-managed handle lifetime so the cdylib-side β-shape
///   can implement `Clone` + `Drop` without crossing the inner
///   `Arc<OutputWriterInner>` source layout).
pub const OUTPUT_WRITER_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Layout version of [`InputMailboxesVTable`].
///
/// - v1: ships the two slots a cdylib processor's `InputMailboxes`
///   β-shape needs from inside `process()`: `read_raw` (returns
///   the next raw frame for a port, with `has_data` out-param
///   distinguishing "no data" from "deserialization-style errors")
///   and `has_data` (query without consuming). All other
///   `InputMailboxes` methods (`add_port`, `set_subscriber`,
///   `set_listener`, `listener_fd`, `drain_listener`,
///   `receive_pending`, `route`, `any_port_has_data`) are
///   host-side only and don't need vtable slots.
pub const INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION: u32 = 1;

// =============================================================================
// Primitive enums
// =============================================================================

/// Log level for tracing + iceoryx2-log emits. Matches
/// `tracing::Level` and `iceoryx2_log_types::LogLevel` orderings;
/// `Fatal` from iceoryx2 collapses to `Error`.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostLogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

/// Filter interest returned by the host's `tracing_register_callsite`
/// callback. Matches `tracing-core`'s `Interest` semantics: `Never`
/// permanently disables a callsite; `Always` permanently enables;
/// `Sometimes` defers to per-event `tracing_enabled`.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostInterest {
    Never = 0,
    Sometimes = 1,
    Always = 2,
}

/// Opaque host-owned state pointer. Threaded through every callback
/// as the first argument; the host derefs to its concrete service
/// table, the cdylib treats it as opaque.
pub type HostHandle = *const c_void;

// =============================================================================
// ProcessorVTable — extern "C" dispatch table for processor instances
// =============================================================================

/// `extern "C" fn` dispatch table the host uses to call methods on a
/// dlopen'd processor instance. Replaces the `Box<dyn
/// DynGeneratedProcessor>` dyn-trait crossing the host used to
/// dispatch through.
///
/// The vtable covers the full host-called surface — constructor +
/// lifecycle (setup / teardown / on_pause / on_resume / process /
/// start / stop / destroy) plus the static-info, iceoryx2-wiring,
/// and config-IO methods compiler ops invoke on every processor.
/// Methods bodies receive `&RuntimeContext*Access` shims whose
/// public method surface is implemented entirely in terms of the
/// callback tables on [`HostServices`] — no Rust trait-object or
/// shared-struct-layout crossing at the host/cdylib boundary.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. The host's
/// `processor_register` impl reads it before dereferencing any other
/// field; older vtables loaded into newer hosts are rejected
/// cleanly. New fields go at the **end** and bump
/// [`PROCESSOR_VTABLE_LAYOUT_VERSION`].
///
/// # Error convention
///
/// Sync lifecycle methods (`process`, `start`, `stop`) and async
/// lifecycle methods (`setup`, `teardown`, `on_pause`, `on_resume`)
/// share the error convention: return `0` on success, non-zero on
/// failure. `err_buf` / `err_buf_cap` is a caller-provided UTF-8
/// scratch buffer the callee writes a message into; `*err_len`
/// receives the actual byte count written. Truncation is benign
/// (caller's buffer was too small).
///
/// `construct` follows the same convention but returns a `*mut
/// c_void` instance handle (null on failure).
///
/// `to_runtime_json`, `config_json`, `execution_config` return a
/// byte count: 0 = "no payload"; a value larger than `out_cap` = the
/// required buffer size (caller should resize and retry). On
/// success, `*out_len` receives the bytes written.
#[repr(C)]
pub struct ProcessorVTable {
    /// Vtable layout version. Must equal
    /// [`PROCESSOR_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Constructor + lifetime
    // -------------------------------------------------------------------------

    /// Build a processor instance from msgpack-encoded `Config`
    /// bytes. Returns a thin opaque pointer the cdylib's wrappers
    /// cast back to `*mut P::Processor`. Null = failure (message in
    /// `err_buf`).
    pub construct: unsafe extern "C" fn(
        config_msgpack_ptr: *const u8,
        config_msgpack_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> *mut c_void,

    /// Free the heap allocation `construct` returned. Equivalent to
    /// `Box::from_raw(instance as *mut P::Processor)` + drop on the
    /// cdylib side.
    pub destroy: unsafe extern "C" fn(instance: *mut c_void),

    // -------------------------------------------------------------------------
    // Async lifecycle (block_on'd inside cdylib using host's tokio handle)
    // -------------------------------------------------------------------------

    pub setup: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    pub teardown: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    pub on_pause: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    pub on_resume: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Sync lifecycle
    // -------------------------------------------------------------------------

    pub process: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Manual-mode start. Returns non-zero with an error message for
    /// non-Manual processors.
    pub start: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Manual-mode stop. Returns non-zero with an error message for
    /// non-Manual processors.
    pub stop: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Static info
    // -------------------------------------------------------------------------

    /// Serialize the processor's [`ExecutionConfig`] to msgpack bytes.
    /// Return value follows the byte-count convention documented on
    /// the struct.
    pub execution_config_msgpack: unsafe extern "C" fn(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize,

    // -------------------------------------------------------------------------
    // Iceoryx2 wiring (host-allocates ownership flip — issue #894)
    //
    // The shared-Rust-type crossings (`Arc<OutputWriter>` /
    // `&mut InputMailboxes`) are retired. The host allocates the
    // `OutputWriterInner` and `InputMailboxesInner` and hands the
    // cdylib opaque `(handle, vtable)` β-shapes via
    // `set_iceoryx2_resources`. Per-frame `write_raw` / `read_raw`
    // dispatch through the new
    // [`OutputWriterVTable`] / [`InputMailboxesVTable`] slots.
    // -------------------------------------------------------------------------

    pub has_iceoryx2_outputs: unsafe extern "C" fn(instance: *const c_void) -> bool,
    pub has_iceoryx2_inputs: unsafe extern "C" fn(instance: *const c_void) -> bool,

    /// Install host-allocated `OutputWriter` and `InputMailboxes`
    /// β-shape handles into the cdylib's processor instance.
    ///
    /// Called by the host once per processor instance after
    /// `construct` returns and before any port connections are
    /// wired. The cdylib's `from_config` initializes its `outputs`
    /// / `inputs` β-shape fields with null `handle` + null `vtable`;
    /// this callback patches in the host-allocated handles so the
    /// per-frame `write_raw` / `read_raw` calls in `process()` see
    /// non-null handles.
    ///
    /// `output_writer_handle` is an `Arc::into_raw(Arc<OutputWriterInner>)`
    /// opaque pointer; the cdylib owns one strong reference and
    /// balances it via `OutputWriterVTable::drop_arc` on Drop. Null
    /// when the processor has no outputs (the cdylib then keeps
    /// the field's null β-shape and never dispatches through it).
    ///
    /// `input_mailboxes_handle` is an `Arc::into_raw(Arc<InputMailboxesInner>)`
    /// opaque pointer with the same lifetime contract. Null when
    /// the processor has no inputs.
    ///
    /// `output_writer_vtable` / `input_mailboxes_vtable` are
    /// `&'static` pointers to the host's vtables (sourced from
    /// [`HostServices::output_writer_vtable`] /
    /// [`HostServices::input_mailboxes_vtable`]). Layout-versions on
    /// both vtables are validated at install time so the cdylib can
    /// dereference without re-checking.
    ///
    /// Returns `0` on success, non-zero on failure (e.g., processor
    /// has no `outputs` / `inputs` field for a non-null handle —
    /// shape mismatch between host and cdylib).
    pub set_iceoryx2_resources: unsafe extern "C" fn(
        instance: *mut c_void,
        output_writer_handle: *const c_void,
        output_writer_vtable: *const OutputWriterVTable,
        input_mailboxes_handle: *const c_void,
        input_mailboxes_vtable: *const InputMailboxesVTable,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Config / state IO (msgpack bytes on the wire)
    // -------------------------------------------------------------------------

    /// Apply a runtime-reconfigure update. The bytes are
    /// msgpack-encoded `P::Config` (matches `construct`'s payload
    /// shape).
    pub apply_config_msgpack: unsafe extern "C" fn(
        instance: *mut c_void,
        config_msgpack_ptr: *const u8,
        config_msgpack_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Serialize the processor's runtime state to msgpack. Return
    /// value follows the byte-count convention; 0 = no state.
    pub to_runtime_msgpack: unsafe extern "C" fn(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize,

    /// Serialize the processor's current config to msgpack. Return
    /// value follows the byte-count convention; 0 = no config.
    pub config_msgpack: unsafe extern "C" fn(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize,
}

// Safety: every field is a primitive or a fn pointer. The vtable's
// `&'static` storage on the cdylib side outlives the cdylib's
// process lifetime via `LOADED_PLUGIN_LIBRARIES` pinning.
unsafe impl Send for ProcessorVTable {}
unsafe impl Sync for ProcessorVTable {}

// =============================================================================
// RuntimeContextVTable — per-instance accessors for the RuntimeContext shim
// =============================================================================

/// Dispatch table the cdylib's `RuntimeContext{Full,Limited}Access`
/// shim uses to read host-owned runtime context state. Every accessor
/// on the shim's public API routes through this table — no Rust
/// trait-object / shared-struct-layout crossing at the cdylib
/// boundary.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0. Older vtables loaded into
/// newer hosts are rejected cleanly. New fields go at the **end** and
/// bump [`RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION`].
///
/// # Opaque-handle returns
///
/// `gpu_full_access` / `gpu_limited_access` return `*const c_void`
/// opaque handles paired with [`GpuContextLimitedAccessVTable`] for
/// method dispatch.
///
/// `audio_clock_handle` and `runtime_ops_handle` return opaque per-
/// instance handles paired with the static vtables on [`HostServices`]
/// ([`HostServices::audio_clock_vtable`],
/// [`HostServices::runtime_ops_vtable`]).
#[repr(C)]
pub struct RuntimeContextVTable {
    /// Vtable layout version. Must equal
    /// [`RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Identifier accessors (owned-return; cdylib does not retain a borrow)
    // -------------------------------------------------------------------------

    /// Copy the runtime id as UTF-8 bytes into `out_buf`. Returns the
    /// required length; `*out_len` receives the actually-written
    /// count (`min(required, out_buf_cap)`). Truncation is benign;
    /// the caller resizes and retries when `required > out_buf_cap`.
    pub runtime_id_copy: unsafe extern "C" fn(
        ctx: *const c_void,
        out_buf: *mut u8,
        out_buf_cap: usize,
        out_len: *mut usize,
    ) -> usize,

    /// Copy the processor id as UTF-8 bytes into `out_buf`. Returns
    /// `-1` when the processor id is `None` (shared/global ctx); for
    /// `Some`, returns the required length and writes `*out_len` like
    /// [`Self::runtime_id_copy`].
    pub processor_id_copy: unsafe extern "C" fn(
        ctx: *const c_void,
        out_buf: *mut u8,
        out_buf_cap: usize,
        out_len: *mut usize,
    ) -> isize,

    // -------------------------------------------------------------------------
    // Lifecycle flags
    // -------------------------------------------------------------------------

    pub is_paused: unsafe extern "C" fn(ctx: *const c_void) -> bool,
    pub should_process: unsafe extern "C" fn(ctx: *const c_void) -> bool,

    // -------------------------------------------------------------------------
    // GPU context handles
    // -------------------------------------------------------------------------

    /// Returns an opaque handle to the privileged [`GpuContextFullAccess`].
    /// Pointer is valid for the lifetime of the surrounding
    /// `RuntimeContextFullAccess` shim. Paired with the methods
    /// reached via [`HostServices::gpu_context_limited_access_vtable`]
    /// for the limited-access surface (FullAccess is engine-only
    /// today; cross-DSO FullAccess wiring is future-phase work).
    pub gpu_full_access: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,

    /// Returns an opaque handle to the restricted [`GpuContextLimitedAccess`].
    /// Paired with [`HostServices::gpu_context_limited_access_vtable`]
    /// for method dispatch.
    pub gpu_limited_access: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,

    // -------------------------------------------------------------------------
    // Host-owned services (handles; static vtables live on HostServices)
    // -------------------------------------------------------------------------

    /// Opaque handle to the runtime's audio clock. Pair with
    /// [`HostServices::audio_clock_vtable`] to call methods on it.
    /// The handle remains valid for the lifetime of the runtime.
    pub audio_clock_handle: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,

    /// Opaque handle to the runtime's graph-mutation operations.
    /// Pair with [`HostServices::runtime_ops_vtable`] to invoke
    /// methods. The handle remains valid for the lifetime of the
    /// runtime.
    pub runtime_ops_handle: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,
}

// Safety: every field is a primitive or a fn pointer. The vtable's
// `&'static` storage on the host side outlives the cdylib's process
// lifetime via the `LOADED_PLUGIN_LIBRARIES` pinning shape.
unsafe impl Send for RuntimeContextVTable {}
unsafe impl Sync for RuntimeContextVTable {}

// =============================================================================
// AudioClockVTable — extern "C" dispatch for SharedAudioClock
// =============================================================================

/// FFI-compatible mirror of `AudioTickContext` carried into
/// extern "C" tick callbacks. Field order matches the host-side
/// `AudioTickContext` and is locked by layout-regression tests.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct AudioTickContextRepr {
    pub timestamp_ns: i64,
    pub samples_needed: u64,
    pub sample_rate: u32,
    pub _reserved_padding: u32,
    pub tick_number: u64,
}

/// Dispatch table for the host's audio clock. The cdylib obtains a
/// handle via [`RuntimeContextVTable::audio_clock_handle`] and reads
/// the static vtable from [`HostServices::audio_clock_vtable`].
#[repr(C)]
pub struct AudioClockVTable {
    pub layout_version: u32,
    pub _reserved_padding: u32,

    /// Returns the clock's sample rate in Hz.
    pub sample_rate: unsafe extern "C" fn(handle: *const c_void) -> u32,

    /// Returns the clock's buffer size (samples per tick).
    pub buffer_size: unsafe extern "C" fn(handle: *const c_void) -> usize,

    /// Register a tick callback. The host owns the callback registration
    /// and invokes `callback(user_data, AudioTickContextRepr)` on every
    /// tick. The `drop_user_data` fn is invoked when the registration
    /// is released (host shutdown or clock teardown). Multiple
    /// registrations are permitted; they fire in registration order.
    pub on_tick: unsafe extern "C" fn(
        handle: *const c_void,
        callback: unsafe extern "C" fn(*mut c_void, AudioTickContextRepr),
        user_data: *mut c_void,
        drop_user_data: unsafe extern "C" fn(*mut c_void),
    ),
}

unsafe impl Send for AudioClockVTable {}
unsafe impl Sync for AudioClockVTable {}

// =============================================================================
// RuntimeOpsVTable — extern "C" dispatch for RuntimeOperations
// =============================================================================

/// Completion callback signature for async runtime ops.
///
/// `status` is `0` on success, non-zero on error. On success,
/// `result_ptr` points at a msgpack-encoded result payload of length
/// `result_len`. On error, `result_ptr` points at a UTF-8 error
/// message of length `result_len`.
///
/// The pointed-at bytes are valid only for the duration of the
/// callback invocation; the cdylib must copy any data it needs to
/// retain.
pub type RuntimeOpCompletionCallback = unsafe extern "C" fn(
    user_data: *mut c_void,
    status: i32,
    result_ptr: *const u8,
    result_len: usize,
);

/// Dispatch table for the host's graph-mutation operations
/// (`add_processor`, `connect`, etc.). The cdylib obtains a handle
/// via [`RuntimeContextVTable::runtime_ops_handle`] and reads the
/// static vtable from [`HostServices::runtime_ops_vtable`].
///
/// All methods are submit-with-completion: the host fires
/// `completion(user_data, status, result_ptr, result_len)` once
/// when the operation finishes. The completion may fire synchronously
/// (op was instantly ready) or asynchronously (on a host thread).
/// The cdylib's wrapper bridges back to its own runtime via a
/// `tokio::sync::oneshot` or equivalent.
///
/// Request payloads are msgpack-encoded; the host decodes against
/// the same types the in-process trait surface accepts
/// (`ProcessorSpec`, `OutputLinkPortRef`, `InputLinkPortRef`,
/// `ProcessorUniqueId`, `LinkUniqueId`).
#[repr(C)]
pub struct RuntimeOpsVTable {
    pub layout_version: u32,
    pub _reserved_padding: u32,

    /// Submit an `add_processor` operation. `spec_msgpack` carries a
    /// msgpack-encoded `ProcessorSpec`. On success the result payload
    /// is the msgpack-encoded `ProcessorUniqueId`.
    pub add_processor: unsafe extern "C" fn(
        handle: *const c_void,
        spec_msgpack_ptr: *const u8,
        spec_msgpack_len: usize,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    /// Submit a `remove_processor` operation. `processor_id_msgpack`
    /// carries a msgpack-encoded `ProcessorUniqueId`. Empty success
    /// payload.
    pub remove_processor: unsafe extern "C" fn(
        handle: *const c_void,
        processor_id_msgpack_ptr: *const u8,
        processor_id_msgpack_len: usize,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    /// Submit a `connect` operation. `from_msgpack` and `to_msgpack`
    /// carry msgpack-encoded `OutputLinkPortRef` / `InputLinkPortRef`.
    /// Success payload is the msgpack-encoded `LinkUniqueId`.
    pub connect: unsafe extern "C" fn(
        handle: *const c_void,
        from_msgpack_ptr: *const u8,
        from_msgpack_len: usize,
        to_msgpack_ptr: *const u8,
        to_msgpack_len: usize,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    /// Submit a `disconnect` operation. `link_id_msgpack` carries a
    /// msgpack-encoded `LinkUniqueId`. Empty success payload.
    pub disconnect: unsafe extern "C" fn(
        handle: *const c_void,
        link_id_msgpack_ptr: *const u8,
        link_id_msgpack_len: usize,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    /// Submit a `to_json` operation. Success payload is the msgpack-
    /// encoded `serde_json::Value`.
    pub to_json: unsafe extern "C" fn(
        handle: *const c_void,
        completion: RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ),

    // v2 additions: owning-Arc handle lifetime management.

    /// Take a (borrowed) handle returned from
    /// [`RuntimeContextVTable::runtime_ops_handle`] and return a new
    /// owned handle with an Arc refcount bump on the underlying
    /// `Arc<dyn RuntimeOperations>`. The owned handle remains valid
    /// even after the originating `RuntimeContext` is dropped, and
    /// MUST be released exactly once via [`Self::drop_handle`].
    pub clone_handle: unsafe extern "C" fn(borrowed_handle: *const c_void) -> *const c_void,

    /// Release an owned handle previously obtained from
    /// [`Self::clone_handle`]. Calling on a null pointer is a no-op.
    /// Calling on the same owned handle twice is undefined behaviour
    /// (it would double-free the Arc refcount).
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),
}

unsafe impl Send for RuntimeOpsVTable {}
unsafe impl Sync for RuntimeOpsVTable {}

// =============================================================================
// GpuContextLimitedAccessVTable — extern "C" dispatch for GpuContextLimitedAccess
// =============================================================================

/// Dispatch table for the host's `GpuContextLimitedAccess`. The
/// cdylib obtains a handle via
/// [`RuntimeContextVTable::gpu_limited_access`] and reads the static
/// vtable from [`HostServices::gpu_context_limited_access_vtable`].
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror [`RuntimeOpsVTable`] v2:
/// `clone_handle(borrowed) -> owned` bumps the host's
/// `Arc<GpuContext>` refcount; `drop_handle(owned)` releases. The
/// owned handle remains valid even after the originating
/// `RuntimeContext` is dropped, which matches the existing
/// `GpuContextLimitedAccess: Clone` contract that lets plugins
/// stash a clone in `setup()` and hand it to a worker thread that
/// outlives the lifecycle call.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods append
/// to the end and bump
/// [`GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct GpuContextLimitedAccessVTable {
    /// Vtable layout version. Must equal
    /// [`GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Handle lifetime (mirrors RuntimeOpsVTable v2)
    // -------------------------------------------------------------------------

    /// Take a borrowed handle returned from
    /// [`RuntimeContextVTable::gpu_limited_access`] and return a new
    /// owned handle with an Arc refcount bump on the underlying
    /// `Arc<GpuContext>`. The owned handle remains valid even after
    /// the originating `RuntimeContext` is dropped, and MUST be
    /// released exactly once via [`Self::drop_handle`].
    pub clone_handle: unsafe extern "C" fn(borrowed_handle: *const c_void) -> *const c_void,

    /// Release an owned handle previously obtained from
    /// [`Self::clone_handle`]. Calling on a null pointer is a no-op.
    /// Calling on the same owned handle twice is undefined behaviour
    /// (it would double-free the Arc refcount).
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),

    // -------------------------------------------------------------------------
    // PixelBuffer return-type lifetime
    // -------------------------------------------------------------------------
    //
    // The cdylib's `PixelBuffer` is `(handle, vtable, cached POD)` where
    // `handle` is `Arc::into_raw(Arc<PixelBufferRef>)` produced by the
    // host. The cdylib never touches Arc internals directly; both
    // refcount bumps (Clone) and decrements (Drop) dispatch through
    // these host-resident callbacks so the Arc accounting is done by
    // host-compiled code under any rustc-minor / dep-graph drift.

    /// Bump the refcount on a `PixelBuffer` handle. Called by the
    /// cdylib's `Clone for PixelBuffer`. The handle pointer is
    /// `Arc::into_raw(Arc<PixelBufferRef>)`-shaped; host
    /// implementation calls `Arc::increment_strong_count(handle)`.
    /// Calling on a null pointer is a no-op.
    pub clone_pixel_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `PixelBuffer` handle. Called by
    /// the cdylib's `Drop for PixelBuffer`. Host implementation
    /// calls `Arc::decrement_strong_count(handle)`; when the
    /// refcount reaches zero the underlying `PixelBufferRef` (and
    /// its platform buffer) is dropped. Calling on a null pointer
    /// is a no-op.
    pub drop_pixel_buffer: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // PixelBuffer method-dispatch (eliminates cross-DSO Arc::from_raw)
    // -------------------------------------------------------------------------
    //
    // The remaining non-cached `PixelBuffer` methods dispatch through
    // host-compiled code so the cdylib never casts the opaque handle
    // to a concrete `*const PixelBufferRef`. Casting the handle
    // cdylib-side would require both sides to agree on
    // `PixelBufferRef`'s in-memory layout — which the cdylib has no
    // way to guarantee under rustc-minor / dep-graph drift.

    /// Number of `PixelBuffer` references to the same underlying
    /// `PixelBufferRef`. Engine-internal probe used by the host's
    /// pool manager to detect "buffer no longer in use" without
    /// locking. Cdylib callers technically can call it through the
    /// vtable today, but the engine restricts the cdylib-facing
    /// `PixelBuffer::strong_count` API to `pub(crate)` so the
    /// cross-DSO path is host-only by visibility. Calling on a null
    /// pointer returns `0`.
    pub strong_count_pixel_buffer: unsafe extern "C" fn(handle: *const c_void) -> usize,

    /// Mapped base address for the given plane, or null if out of
    /// range. Plane 0 on a VMA-allocated or single-plane-imported
    /// buffer points at the same bytes as
    /// `slpn_gpu_surface_plane_base_address` / equivalent. Calling
    /// on a null handle returns `null`.
    pub plane_base_address_pixel_buffer:
        unsafe extern "C" fn(handle: *const c_void, plane_index: u32) -> *mut u8,

    /// Byte size of the given plane, or `0` if out of range. Calling
    /// on a null handle returns `0`.
    pub plane_size_pixel_buffer:
        unsafe extern "C" fn(handle: *const c_void, plane_index: u32) -> u64,

    // -------------------------------------------------------------------------
    // Texture return-type lifetime
    // -------------------------------------------------------------------------
    //
    // The cdylib's `Texture` is `(handle, vtable, cached POD)` where
    // `handle` is `Arc::into_raw(Arc<TextureInner>)` produced by the
    // host. Identical Arc-lifetime contract as `PixelBuffer` —
    // refcount accounting runs in host-compiled code so the cdylib
    // never has to know `TextureInner`'s layout.

    /// Bump the refcount on a `Texture` handle. Called by the
    /// cdylib's `Clone for Texture`. The handle pointer is
    /// `Arc::into_raw(Arc<TextureInner>)`-shaped; host implementation
    /// calls `Arc::increment_strong_count(handle)`. Calling on a null
    /// pointer is a no-op.
    pub clone_texture: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `Texture` handle. Called by the
    /// cdylib's `Drop for Texture`. Host implementation calls
    /// `Arc::decrement_strong_count(handle)`; when the refcount
    /// reaches zero the underlying `TextureInner` (and its platform
    /// texture) is dropped. Calling on a null pointer is a no-op.
    pub drop_texture: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // PooledTextureHandle return-type lifetime (v4 — drop-only)
    // -------------------------------------------------------------------------
    //
    // `PooledTextureHandle` is deliberately NOT `Clone`: Drop must
    // release the pool slot exactly once via the underlying
    // `TexturePoolInner::release(slot_id)` path. The cdylib carries a
    // `Box::into_raw(Box::new(PooledTextureHandleInner))`-shaped
    // handle and fires `drop_pooled_texture_handle` from its `Drop`
    // impl. There is no `clone_pooled_texture_handle` — cloning would
    // duplicate the raw pointer and double-release the slot.

    /// Release the host-side `PooledTextureHandleInner` backing a
    /// `PooledTextureHandle`. The host runs `Box::from_raw + drop`,
    /// which fires the inner's `Drop` impl and releases the pool
    /// slot. Calling on a null pointer is a no-op; calling twice on
    /// the same owned handle is undefined behaviour (double-free of
    /// the Box plus a double-release of the pool slot).
    pub drop_pooled_texture_handle: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // Method dispatch — Texture-related
    // -------------------------------------------------------------------------
    //
    // The six methods on the cdylib's `GpuContextLimitedAccess` that
    // touch `Texture` / `PooledTextureHandle` / `TextureRegistration`
    // now dispatch through these callbacks instead of through the
    // cdylib's view of `GpuContext`'s layout. Each callback's first
    // argument is the `*const Arc<GpuContext>`-shaped handle from
    // `RuntimeContextVTable::gpu_limited_access` (or a clone via
    // `Self::clone_handle`).

    /// Register a texture in the host's same-process texture cache.
    /// `texture_handle` is the `*const Arc<TextureInner>` from a
    /// cdylib-side `Texture`'s `handle` field; the host bumps the Arc
    /// refcount (`Arc::increment_strong_count`) and inserts a clone
    /// into the cache. The cdylib's caller still owns its `Texture`
    /// value and continues to be responsible for its eventual Drop.
    /// Calling with a null handle or null texture_handle is a no-op.
    ///
    /// `initial_layout_raw` is i32-encoded `VulkanLayout` on Linux;
    /// non-Linux hosts ignore the layout. The "without layout" form
    /// of the call passes `VulkanLayout::UNDEFINED` (i32 0).
    pub register_texture: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        texture_handle: *const c_void,
        initial_layout_raw: i32,
    ),

    /// Update a registered texture's tracked layout after a
    /// transition. Linux-only contract on the host side; non-Linux
    /// hosts treat this as a no-op. Calling on a missing id is a
    /// no-op.
    pub update_texture_registration_layout: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        layout_raw: i32,
    ),

    /// Acquire a pooled texture for the given descriptor. On success
    /// writes a new `PooledTextureHandle` into `*out_pooled_handle`
    /// and returns `0`. On failure writes a UTF-8 error message into
    /// `err_buf` (clamped to `err_buf_cap`), sets `*err_len` to the
    /// bytes written, and returns non-zero.
    ///
    /// The `format_raw` is the `#[repr(u32)]` discriminant of
    /// [`streamlib_consumer_rhi::TextureFormat`]; `usage_bits` is
    /// [`streamlib_consumer_rhi::TextureUsages::bits`].
    pub acquire_texture: unsafe extern "C" fn(
        handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
        usage_bits: u32,
        out_pooled_handle: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Resolve a VideoFrame's texture from its surface_id. On
    /// success writes a new `Texture` into `*out_texture` and
    /// returns `0`. On failure writes a UTF-8 error message into
    /// `err_buf` and returns non-zero.
    ///
    /// `has_layout` is `1` when `layout_raw` carries a per-frame
    /// `texture_layout` override, `0` for the default-resolution
    /// path. `width` / `height` are required for the Path 3 fallback.
    pub resolve_texture_by_surface_id: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        has_layout: i32,
        layout_raw: i32,
        width: u32,
        height: u32,
        out_texture: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Remove an id from the host's same-process texture cache.
    /// Idempotent — missing entries are a no-op.
    pub unregister_texture: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
    ),

    // -------------------------------------------------------------------------
    // Linux-only buffer Arc-handle lifecycle
    // -------------------------------------------------------------------------
    //
    // The cdylib's `StorageBuffer` / `UniformBuffer` / `VertexBuffer` /
    // `IndexBuffer` are each `(handle, vtable, byte_size, mapped_ptr)`
    // where `handle` is `Arc::into_raw(Arc<HostVulkanBuffer>)`. All
    // four wrap the same Arc type under the hood but keep separate
    // Rust newtypes for binding-shape enforcement. Each gets its own
    // clone/drop pair so the vtable structure mirrors the type
    // structure — future-proofs against per-type divergence (a
    // buffer growing extra state) without re-versioning a shared
    // callback. Stub on non-Linux hosts; callable only from cdylib
    // code that links the Linux-only buffer types.

    /// Bump the refcount on a `StorageBuffer` handle.
    /// `Arc::increment_strong_count(handle as *const HostVulkanBuffer)`.
    pub clone_storage_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `StorageBuffer` handle.
    pub drop_storage_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Bump the refcount on a `UniformBuffer` handle.
    pub clone_uniform_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `UniformBuffer` handle.
    pub drop_uniform_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Bump the refcount on a `VertexBuffer` handle.
    pub clone_vertex_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VertexBuffer` handle.
    pub drop_vertex_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Bump the refcount on an `IndexBuffer` handle.
    pub clone_index_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on an `IndexBuffer` handle.
    pub drop_index_buffer: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // Linux-only buffer acquire methods
    // -------------------------------------------------------------------------
    //
    // Each acquire callback writes a fresh `{Storage,Uniform,Vertex,
    // Index}Buffer` into `*out_buffer` on success and returns 0; on
    // failure writes a UTF-8 message into `err_buf` and returns
    // non-zero. Non-Linux stubs return non-zero with a
    // "buffer-type-not-available-on-this-platform" message.

    /// Acquire a `StorageBuffer` of the given byte size.
    pub acquire_storage_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Acquire a `UniformBuffer` of the given byte size.
    pub acquire_uniform_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Acquire a `VertexBuffer` of the given byte size.
    pub acquire_vertex_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Acquire an `IndexBuffer` of the given byte size.
    pub acquire_index_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // TextureRegistration Arc-handle lifecycle
    // -------------------------------------------------------------------------
    //
    // The cdylib's `TextureRegistration` is `(handle, vtable)` where
    // `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`. Same
    // shape as `PixelBuffer` / `Texture`'s Arc-handle pattern. Cdylibs
    // get Arc semantics (cheap Clone via refcount bump) without ever
    // touching the host's `Arc<T>` implementation.

    /// Bump the refcount on a `TextureRegistration` handle. Host
    /// implementation runs
    /// `Arc::increment_strong_count(handle as *const TextureRegistrationInner)`.
    pub clone_texture_registration: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `TextureRegistration` handle. When
    /// the strong count reaches zero the underlying
    /// `TextureRegistrationInner` (and its `Texture` plus
    /// `current_layout` atomic) is dropped.
    pub drop_texture_registration: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // TextureRegistration method dispatch
    // -------------------------------------------------------------------------

    /// Borrow the registration's underlying `Texture`. Returns a
    /// pointer into the host's heap allocation that is alive for as
    /// long as the caller holds the originating
    /// `TextureRegistration` (the Arc's strong count keeps the
    /// inner alive). The returned `Texture` is itself a layout-stable
    /// `#[repr(C)]` value (see `core/rhi/texture.rs::Texture::tests::texture_layout`),
    /// so the cdylib can deref the pointer directly.
    pub texture_registration_texture:
        unsafe extern "C" fn(handle: *const c_void) -> *const c_void,

    /// Last-known `VkImageLayout` (raw `i32` enumerant). Atomic
    /// `Acquire` load on the host side. Linux-only behaviour; non-
    /// Linux hosts return `0` (VK_IMAGE_LAYOUT_UNDEFINED).
    pub texture_registration_current_layout:
        unsafe extern "C" fn(handle: *const c_void) -> i32,

    /// Record a new last-known layout. Atomic `Release` store on the
    /// host side. Linux-only behaviour; non-Linux hosts treat this
    /// as a no-op.
    pub texture_registration_update_layout:
        unsafe extern "C" fn(handle: *const c_void, layout_raw: i32),

    /// Resolve a VideoFrame's full registration record (texture +
    /// layout) from its `surface_id`. On success writes a new
    /// `TextureRegistration` into `*out_registration` and returns
    /// `0`. On failure writes a UTF-8 error message into `err_buf`
    /// and returns non-zero.
    ///
    /// `has_layout` is `1` when `layout_raw` carries a per-frame
    /// `texture_layout` override, `0` for the default-resolution
    /// path. `width` / `height` are required for the Path 3 fallback.
    pub resolve_texture_registration_by_surface_id: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        has_layout: i32,
        layout_raw: i32,
        width: u32,
        height: u32,
        out_registration: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // RhiCommandQueue Arc-handle lifecycle + create_command_buffer
    // -------------------------------------------------------------------------
    //
    // The cdylib's `RhiCommandQueue` is `(handle, vtable)` where
    // `handle` is `Arc::into_raw(Arc<RhiCommandQueueInner>)`. Same
    // shape as every other Arc-handle β-reshape on this vtable.

    /// Bump the refcount on an `RhiCommandQueue` handle.
    pub clone_rhi_command_queue: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on an `RhiCommandQueue` handle.
    pub drop_rhi_command_queue: unsafe extern "C" fn(handle: *const c_void),

    /// Create a new `CommandBuffer` from a queue. On success writes a
    /// fresh `CommandBuffer` (Box-handle β-shape) into `*out_cb` and
    /// returns 0; on failure writes a UTF-8 error message into
    /// `err_buf` and returns non-zero.
    pub create_command_buffer_from_queue: unsafe extern "C" fn(
        queue_handle: *const c_void,
        out_cb: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // CommandBuffer lifecycle — drop + consume-semantics commits
    // -------------------------------------------------------------------------
    //
    // `CommandBuffer` is single-use. Box-handle (not Arc) — no Clone.
    // `commit` and `commit_and_wait` are consume-semantics: the host
    // runs `Box::from_raw + commit + drop` and the cdylib nulls its
    // local handle/vtable fields so Drop becomes a no-op afterward.

    /// Release the host-side `Box<CommandBufferInner>` backing a
    /// `CommandBuffer`. Calling on a null pointer is a no-op.
    /// Calling twice on the same handle is undefined behaviour
    /// (double-free of the Box).
    pub drop_command_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Commit the command buffer for execution (consume-semantics).
    /// Host runs `Box::from_raw + commit + drop`; the cdylib's Drop
    /// is then a no-op (handle/vtable are nulled by the cdylib-side
    /// `commit(self)` wrapper).
    pub commit_command_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Commit and wait for completion (consume-semantics). Same
    /// lifetime contract as [`Self::commit_command_buffer`].
    pub commit_and_wait_command_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Copy one texture to another. `src` / `dst` are
    /// `*const Texture` pointers — the layout is locked by the
    /// per-type `texture_layout` regression test so the host's read
    /// agrees with the cdylib's write.
    pub copy_texture_command_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        src: *const c_void,
        dst: *const c_void,
    ),

    // -------------------------------------------------------------------------
    // GpuContextLimitedAccess command-queue / command-buffer / blit methods
    // -------------------------------------------------------------------------

    /// Return an owned `RhiCommandQueue` view of the host's shared
    /// command queue (refcount bumped on the underlying
    /// `Arc<RhiCommandQueueInner>`). Cdylib's caller releases via
    /// `drop_rhi_command_queue`. Writes the β-shape into
    /// `*out_queue`; returns 0 on success, non-zero on internal
    /// failure (e.g. null gpu handle).
    pub command_queue: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_queue: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Create a CPU-side command buffer from the shared queue. Same
    /// shape as [`Self::create_command_buffer_from_queue`] but takes
    /// a `GpuContext` handle rather than a queue handle —
    /// `GpuContextLimitedAccess::create_command_buffer` is a
    /// convenience that delegates to the engine's shared queue.
    pub create_command_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_cb: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Copy a host-visible pixel buffer's contents into a
    /// pre-allocated device-local texture. Linux-only on the host
    /// side; non-Linux stubs return non-zero. `pixel_buffer` and
    /// `texture` are `*const PixelBuffer` / `*const Texture` β-shape
    /// pointers.
    pub copy_pixel_buffer_to_texture: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        pixel_buffer: *const c_void,
        texture: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        width: u32,
        height: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Copy pixels between same-format, same-size pixel buffers.
    pub blit_copy: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        src: *const c_void,
        dst: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Copy from raw IOSurface to a pixel buffer (macOS-only). The
    /// `src_iosurface_ref` is an `IOSurfaceRef` (raw `*const c_void`).
    /// Non-macOS hosts return non-zero with a "not available on this
    /// platform" message.
    pub blit_copy_iosurface: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        src_iosurface_ref: *const c_void,
        dst_pixel_buffer: *const c_void,
        width: u32,
        height: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // SurfaceStore accessors
    // -------------------------------------------------------------------------
    //
    // The bulk of the SurfaceStore ABI lives on its own
    // SurfaceStoreVTable; these two callbacks bridge from
    // GpuContextLimitedAccess to that subsystem.

    /// Return an owned [`SurfaceStore`] β-shape if the host has one,
    /// or a null-handle β-shape ("None") otherwise. Always returns 0;
    /// callers branch on whether the written `SurfaceStore`'s handle
    /// is null. Writes a fresh β-shape (Arc refcount bumped) into
    /// `*out_store`.
    pub surface_store: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_store: *mut c_void,
    ),

    /// Convenience method: check out a surface from the engine's
    /// `SurfaceStore` by `surface_id` (assumes the store exists).
    /// Writes a fresh `PixelBuffer` β-shape into `*out_pixel_buffer`
    /// on success.
    pub check_out_surface: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // PixelBuffer acquire / get / resolve method-dispatch
    // -------------------------------------------------------------------------

    /// Acquire a pixel buffer from a pre-reserved pool. The tuple
    /// return `(PixelBufferPoolId, PixelBuffer)` is encoded via
    /// paired out-params: `out_pool_id_buf` receives the
    /// `PixelBufferPoolId`'s string bytes (capped at
    /// `out_pool_id_cap`; `*out_pool_id_len` receives the actual
    /// length, truncated to fit). `*out_pixel_buffer` receives a
    /// fresh `PixelBuffer` β-shape on success.
    ///
    /// `format_raw` is the `#[repr(u32)]` discriminant of
    /// [`streamlib_consumer_rhi::PixelFormat`].
    pub acquire_pixel_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
        out_pool_id_buf: *mut u8,
        out_pool_id_cap: usize,
        out_pool_id_len: *mut usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Get a pixel buffer by its pool id (local-cache fast path).
    /// `pool_id_ptr` / `pool_id_len` is the UTF-8 byte
    /// representation of the `PixelBufferPoolId`'s inner string.
    pub get_pixel_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        pool_id_ptr: *const u8,
        pool_id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Resolve a VideoFrame's buffer from its `surface_id`.
    pub resolve_pixel_buffer_by_surface_id: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Escalate scope transition (Phase C3)
    // -------------------------------------------------------------------------

    /// Begin an escalate scope. Acquires the host's escalate gate on
    /// the supplied `gpu_handle`, mints an opaque scope token, and
    /// writes it into `*out_scope_token` on success. Returns 0 on
    /// success, non-zero on failure (message in `err_buf`).
    ///
    /// The token is opaque to the caller; the cdylib's
    /// [`GpuContextLimitedAccess::escalate`] wrapper passes it as the
    /// `gpu_handle` slot when constructing a cdylib-side
    /// [`GpuContextFullAccess`] and back to `escalate_end` when the
    /// scope completes. Every FullAccess vtable callback validates
    /// the token against the host's
    /// `escalate_scope_registry` before dispatch; calls after
    /// `escalate_end` (or against a never-issued token) return a
    /// `Error::InvalidEscalateScope`-flavored error in the callback's
    /// `err_buf`.
    ///
    /// Blocking: the gate's `enter` serializes against any other
    /// escalate scope on the same `GpuContext` (host-mode or
    /// cdylib-mode), so `escalate_begin` may block for the duration
    /// of a prior scope.
    ///
    /// [`GpuContextLimitedAccess::escalate`]: streamlib_plugin_abi::GpuContextLimitedAccessVTable
    /// [`GpuContextFullAccess`]: streamlib_plugin_abi::GpuContextFullAccessVTable
    pub escalate_begin: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_scope_token: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// End an escalate scope. Releases the host's escalate gate and
    /// invalidates the token, then waits for the GPU device to go
    /// idle (matching the host-mode escalate path's
    /// `wait_device_idle` at scope end). Returns 0 on success,
    /// non-zero on failure (message in `err_buf`); a non-zero return
    /// indicates `wait_device_idle` failed — the scope is still
    /// invalidated and the gate is still released.
    ///
    /// Idempotent against a never-issued or already-ended token —
    /// the call returns 0 cleanly without releasing another scope's
    /// gate.
    pub escalate_end: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        scope_token: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Texture method dispatch — DMA-BUF FD export (Phase F)
    // -------------------------------------------------------------------------

    /// Export the texture's GPU memory as a Linux DMA-BUF file
    /// descriptor for cross-framework / cross-process sharing.
    ///
    /// **The returned FD is borrowed — owned by the host-side
    /// texture for as long as the cdylib-side [`Texture`] keeps its
    /// `Arc<TextureInner>` strong count > 0.** The host caches the
    /// FD on the first call and closes it in `HostVulkanTexture`'s
    /// `Drop`; callers that need to hand the FD to a different
    /// process or to an API that consumes it (e.g.
    /// `vkImportMemoryFdKHR` takes ownership on success) MUST
    /// `dup(2)` the returned FD first.
    ///
    /// `texture_handle` is the `*const Arc<TextureInner>`-shaped
    /// `handle` field on a cdylib-side [`Texture`] (the same handle
    /// the cdylib already passes to `clone_texture` / `drop_texture`
    /// — see [`crate::core::rhi::Texture`]). The host derefs it as a
    /// borrow without touching the refcount, calls the platform-
    /// specific Vulkan export path, and returns the FD via the i64
    /// return value.
    ///
    /// Encoding:
    ///   - `>= 0` — valid DMA-BUF FD (always fits in `i32` on Linux,
    ///     widened to `i64` for forward-compat with any future
    ///     platform that exposes wider FD-like identifiers via the
    ///     same slot).
    ///   - `-1` — texture has no DMA-BUF FD (not Linux, or no Vulkan
    ///     backing, or export failed). Equivalent to `Option::None`
    ///     on the cdylib-facing
    ///     [`crate::core::rhi::Texture::native_handle`].
    ///
    /// Non-Linux hosts return `-1` unconditionally — DMA-BUF is a
    /// Linux concept; macOS IOSurface and Windows DXGI shared handles
    /// are deferred to future slots when their respective cdylib
    /// adapter work resumes (see #908's deferred macOS list).
    ///
    /// Calling with a null `texture_handle` returns `-1` (no panic).
    pub texture_native_dma_buf_fd:
        unsafe extern "C" fn(texture_handle: *const c_void) -> i64,

    // -------------------------------------------------------------------------
    // Video-source timeline semaphore publish/clear (v12 — #958)
    // -------------------------------------------------------------------------

    /// Publish a producer's `Arc<HostVulkanTimelineSemaphore>` for
    /// in-process GPU-GPU sync (the in-tree consumer is
    /// `LinuxDisplayProcessor::render_frame`, which waits on the
    /// camera's published timeline before binding the captured
    /// texture).
    ///
    /// `timeline_handle` is `Arc::as_ptr(timeline) as *const c_void`
    /// — a **borrowed** pointer; the cdylib retains its own Arc and
    /// the host does NOT consume the caller's reference. The host
    /// callback `Arc::increment_strong_count`s the pointer,
    /// reconstitutes a temporary owned Arc via `Arc::from_raw`,
    /// calls `gpu.set_video_source_timeline_semaphore(&arc)` (which
    /// itself clones into the slot), and lets the temporary Arc
    /// drop — net effect: one fresh strong count moves into the
    /// slot; the cdylib's Arc is unchanged.
    ///
    /// Mirrors the Arc-borrow-+-strong-count-bump pattern
    /// [`Self::register_texture`] uses for `Arc<TextureInner>`.
    ///
    /// **Arc-raw-pointer transit** — not a layout-stable β-shape.
    /// In-tree consumers (camera) ride this freely because they're
    /// built in the same workspace as the engine. Cross-repo plugin
    /// distribution will need a β-shape lift for
    /// `HostVulkanTimelineSemaphore`; tracked as a future follow-up
    /// alongside `create_timeline_semaphore`'s identical caveat.
    ///
    /// Linux-only on the host side; non-Linux stubs are no-ops.
    /// Calling with a null `gpu_handle` or null `timeline_handle` is
    /// a no-op.
    pub set_video_source_timeline_semaphore: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        timeline_handle: *const c_void,
    ),

    /// Drop the published producer timeline so consumers observe the
    /// absence and skip the wait. Idempotent against a never-set or
    /// already-cleared slot. Pairs with
    /// [`Self::set_video_source_timeline_semaphore`].
    ///
    /// Linux-only on the host side; non-Linux stubs are no-ops.
    /// Calling with a null `gpu_handle` is a no-op.
    pub clear_video_source_timeline_semaphore: unsafe extern "C" fn(
        gpu_handle: *const c_void,
    ),

    // -------------------------------------------------------------------------
    // HostVulkanTimelineSemaphore::wait (v13 — #958 Phase E sub)
    // -------------------------------------------------------------------------

    /// Block until the host's `HostVulkanTimelineSemaphore` counter
    /// has reached or surpassed `value`. Called per-frame from
    /// `HostVulkanTimelineSemaphore::wait` on the cdylib side; the
    /// host calls `vkWaitSemaphores` against its own loaded
    /// `vulkanalia::Device` to avoid running Vulkan dispatch from a
    /// statically-linked cdylib copy of the loader.
    ///
    /// `timeline_handle` is a borrowed `*const HostVulkanTimelineSemaphore`
    /// — the cdylib-side `wait` method takes `&self` and passes
    /// `self as *const Self as *const c_void`; when the caller
    /// instead holds an `Arc<HostVulkanTimelineSemaphore>` directly
    /// (rare for `wait`), `Arc::as_ptr(&arc)` resolves to the same
    /// borrow pointer. The host does NOT bump the refcount on the
    /// borrow.
    ///
    /// `timeout_ns` is the per-call timeout; pass `u64::MAX` for
    /// no timeout. Returns 0 on success, non-zero (`err_buf`
    /// populated) on driver failure / timeout. Null `gpu_handle` or
    /// null `timeline_handle` writes a "null handle" error and
    /// returns non-zero.
    ///
    /// Linux-only on the host side; non-Linux stubs return non-zero.
    pub wait_timeline_semaphore: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        timeline_handle: *const c_void,
        value: u64,
        timeout_ns: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for GpuContextLimitedAccessVTable {}
unsafe impl Sync for GpuContextLimitedAccessVTable {}

// =============================================================================
// GPU kernel descriptor mirrors — Phase C2
// =============================================================================
//
// Wire-protocol `#[repr(C)]` mirrors of the Rust descriptor types in
// `streamlib::core::rhi::{compute,graphics,ray_tracing}_kernel`. Built by
// the cdylib at the FullAccess vtable call site; decoded by the host
// into the canonical Rust `*Descriptor` and dispatched to
// `GpuContextFullAccess::create_*_kernel`.
//
// **Enum discriminant convention.** Variant enums are mirrored as
// `#[repr(u32)]` so the FFI value is the discriminant only. The
// payload-carrying enums (`VertexInputState`, `DepthStencilState`,
// `ColorBlendState`, `RayTracingShaderGroup`) are flattened into
// `(kind: u32, ...flat payload fields)` structs — every payload field
// is always present in the wire format, irrelevant fields are zero or
// `u32::MAX` (the canonical "absent" sentinel for `Option<u32>`
// stage-index references) and ignored on the host side. This matches
// the C1 vtable pattern (`acquire_texture` decodes `format_raw: u32`
// into the appropriate enum on the host) and avoids relying on
// `#[repr(C, u32)]` data-carrying-enum semantics.
//
// **Pointer-shaped slices.** Every `&[T]` in the Rust descriptor is
// mirrored as `(ptr: *const TRepr, len: usize)`. The pointed-at array
// must live for the duration of the vtable call; the host
// `slice::from_raw_parts` lift is bounded by the call. Borrow-shaped
// reprs match the C1 method-dispatch pattern (`id_ptr` / `id_len`
// pairs throughout).
//
// **`Option<u32>` sentinel.** Ray-tracing shader-group references use
// `u32::MAX` to encode `None` (no shader index). `u32::MAX` is
// reserved at the spec level (`VK_SHADER_UNUSED_KHR == ~0u`).

// -----------------------------------------------------------------------------
// Compute kernel mirrors
// -----------------------------------------------------------------------------

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::ComputeBindingKind`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeBindingKindRepr {
    StorageBuffer = 0,
    UniformBuffer = 1,
    SampledTexture = 2,
    StorageImage = 3,
    /// `VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE` — sampled image without a
    /// combined sampler. GLSL `texture2D` / `texelFetch` style.
    SampledImage = 4,
}

/// GPU capability snapshot returned by
/// [`GpuContextFullAccessVTable::gpu_capabilities`]. Layout-stable
/// `#[repr(C)]` so cdylibs can read the fields cross-rustc-version
/// without dep-graph coupling.
///
/// `device_name` is a fixed-size UTF-8 buffer; bytes past `device_name_len`
/// are unspecified. The 256-byte buffer matches Vulkan's
/// `VK_MAX_PHYSICAL_DEVICE_NAME_SIZE` (the source string for vendor
/// names). `_reserved_padding` brings the struct to 8-byte alignment.
#[repr(C)]
pub struct GpuCapabilitiesRepr {
    /// UTF-8 device name; valid for `device_name_len` bytes. Trailing
    /// bytes are unspecified.
    pub device_name: [u8; 256],
    /// Number of valid UTF-8 bytes in `device_name`.
    pub device_name_len: u32,
    /// Whether the GPU exposes `VK_KHR_external_memory_fd` +
    /// `VK_EXT_external_memory_dma_buf` (DMA-BUF FD import path
    /// available).
    pub supports_external_memory: u8,
    /// Whether cross-device DMA-BUF probe is supported. NVIDIA Linux
    /// reports `false` per the engine-layer capability guard
    /// (`docs/learnings/nvidia-opaque-fd-after-swapchain.md`).
    pub supports_cross_device_dma_buf_probe: u8,
    /// Whether the GPU exposes `VK_KHR_ray_tracing_pipeline`.
    pub supports_ray_tracing_pipeline: u8,
    /// Reserved — zero today, brings struct to 264-byte natural
    /// alignment with room for future capability bools.
    pub _reserved_padding: u8,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::ComputeBindingSpec`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ComputeBindingSpecRepr {
    pub binding: u32,
    /// `ComputeBindingKindRepr` discriminant. Held as `u32` to keep the
    /// in-FFI value layout-stable across rustc versions (matches the
    /// pattern used by `acquire_texture`'s `format_raw` parameter).
    pub kind: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::ComputeKernelDescriptor`.
///
/// All pointer fields borrow into caller-owned memory and must
/// remain valid for the duration of the vtable call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ComputeKernelDescriptorRepr {
    pub label_ptr: *const u8,
    pub label_len: usize,
    pub spv_ptr: *const u8,
    pub spv_len: usize,
    pub bindings_ptr: *const ComputeBindingSpecRepr,
    pub bindings_len: usize,
    pub push_constant_size: u32,
    pub _reserved_padding: u32,
}

// -----------------------------------------------------------------------------
// Graphics kernel mirrors
// -----------------------------------------------------------------------------

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::GraphicsShaderStage`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsShaderStageRepr {
    Vertex = 0,
    Fragment = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsStage`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsStageRepr {
    /// `GraphicsShaderStageRepr` discriminant.
    pub stage: u32,
    pub _reserved_padding: u32,
    pub spv_ptr: *const u8,
    pub spv_len: usize,
    pub entry_point_ptr: *const u8,
    pub entry_point_len: usize,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::GraphicsBindingKind`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsBindingKindRepr {
    SampledTexture = 0,
    StorageBuffer = 1,
    UniformBuffer = 2,
    StorageImage = 3,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsBindingSpec`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsBindingSpecRepr {
    pub binding: u32,
    /// `GraphicsBindingKindRepr` discriminant.
    pub kind: u32,
    /// `GraphicsShaderStageFlags::bits()` — already a u32 bitflag set,
    /// crosses as `u32` directly.
    pub stages: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsPushConstants`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsPushConstantsRepr {
    pub size: u32,
    /// `GraphicsShaderStageFlags::bits()`.
    pub stages: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::PrimitiveTopology`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveTopologyRepr {
    PointList = 0,
    LineList = 1,
    LineStrip = 2,
    TriangleList = 3,
    TriangleStrip = 4,
    TriangleFan = 5,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::VertexAttributeFormat`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAttributeFormatRepr {
    R32Float = 0,
    Rg32Float = 1,
    Rgb32Float = 2,
    Rgba32Float = 3,
    R32Uint = 4,
    Rg32Uint = 5,
    Rgb32Uint = 6,
    Rgba32Uint = 7,
    R32Sint = 8,
    Rg32Sint = 9,
    Rgb32Sint = 10,
    Rgba32Sint = 11,
    Rgba8Unorm = 12,
    Rgba8Snorm = 13,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::VertexInputRate`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexInputRateRepr {
    Vertex = 0,
    Instance = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::VertexInputBinding`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VertexInputBindingRepr {
    pub binding: u32,
    pub stride: u32,
    /// `VertexInputRateRepr` discriminant.
    pub input_rate: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::VertexInputAttribute`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VertexInputAttributeRepr {
    pub location: u32,
    pub binding: u32,
    /// `VertexAttributeFormatRepr` discriminant.
    pub format: u32,
    pub offset: u32,
}

/// Discriminant for the [`VertexInputStateRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexInputStateKindRepr {
    None = 0,
    Buffers = 1,
}

/// Tagged-union mirror of `streamlib::core::rhi::VertexInputState`.
///
/// When `kind == None`, the slice pointers are null and lengths are 0.
/// When `kind == Buffers`, the slices borrow into caller-owned arrays.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VertexInputStateRepr {
    /// `VertexInputStateKindRepr` discriminant.
    pub kind: u32,
    pub _reserved_padding: u32,
    pub bindings_ptr: *const VertexInputBindingRepr,
    pub bindings_len: usize,
    pub attributes_ptr: *const VertexInputAttributeRepr,
    pub attributes_len: usize,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::PolygonMode`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolygonModeRepr {
    Fill = 0,
    Line = 1,
    Point = 2,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::CullMode`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CullModeRepr {
    None = 0,
    Front = 1,
    Back = 2,
    FrontAndBack = 3,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::FrontFace`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontFaceRepr {
    CounterClockwise = 0,
    Clockwise = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RasterizationState`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RasterizationStateRepr {
    /// `PolygonModeRepr` discriminant.
    pub polygon_mode: u32,
    /// `CullModeRepr` discriminant.
    pub cull_mode: u32,
    /// `FrontFaceRepr` discriminant.
    pub front_face: u32,
    pub line_width: f32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::MultisampleState`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MultisampleStateRepr {
    pub samples: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::DepthCompareOp`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthCompareOpRepr {
    Never = 0,
    Less = 1,
    Equal = 2,
    LessOrEqual = 3,
    Greater = 4,
    NotEqual = 5,
    GreaterOrEqual = 6,
    Always = 7,
}

/// Discriminant for the [`DepthStencilStateRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthStencilStateKindRepr {
    Disabled = 0,
    Enabled = 1,
}

/// Tagged-union mirror of `streamlib::core::rhi::DepthStencilState`.
///
/// `depth_test` and `depth_write` are ignored when `kind == Disabled`
/// (writer sets them to 0 / 0 by convention).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DepthStencilStateRepr {
    /// `DepthStencilStateKindRepr` discriminant.
    pub kind: u32,
    /// `DepthCompareOpRepr` discriminant. Ignored when `kind == Disabled`.
    pub depth_test: u32,
    /// Boolean as `u32` (0 = false, 1 = true). Ignored when `kind == Disabled`.
    pub depth_write: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::BlendFactor`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendFactorRepr {
    Zero = 0,
    One = 1,
    SrcColor = 2,
    OneMinusSrcColor = 3,
    DstColor = 4,
    OneMinusDstColor = 5,
    SrcAlpha = 6,
    OneMinusSrcAlpha = 7,
    DstAlpha = 8,
    OneMinusDstAlpha = 9,
    ConstantColor = 10,
    OneMinusConstantColor = 11,
    ConstantAlpha = 12,
    OneMinusConstantAlpha = 13,
    SrcAlphaSaturate = 14,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::BlendOp`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendOpRepr {
    Add = 0,
    Subtract = 1,
    ReverseSubtract = 2,
    Min = 3,
    Max = 4,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::ColorBlendAttachment`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ColorBlendAttachmentRepr {
    /// `BlendFactorRepr` discriminant.
    pub src_color_blend_factor: u32,
    /// `BlendFactorRepr` discriminant.
    pub dst_color_blend_factor: u32,
    /// `BlendOpRepr` discriminant.
    pub color_blend_op: u32,
    /// `BlendFactorRepr` discriminant.
    pub src_alpha_blend_factor: u32,
    /// `BlendFactorRepr` discriminant.
    pub dst_alpha_blend_factor: u32,
    /// `BlendOpRepr` discriminant.
    pub alpha_blend_op: u32,
    /// `ColorWriteMask::bits()`.
    pub color_write_mask: u32,
    pub _reserved_padding: u32,
}

/// Discriminant for the [`ColorBlendStateRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorBlendStateKindRepr {
    Disabled = 0,
    Enabled = 1,
}

/// Tagged-union mirror of `streamlib::core::rhi::ColorBlendState`.
///
/// When `kind == Disabled`, `color_write_mask` carries the disabled
/// state's mask and `attachment` fields are zero (ignored on the host
/// side). When `kind == Enabled`, `color_write_mask` is zero and
/// `attachment` carries the full attachment description.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ColorBlendStateRepr {
    /// `ColorBlendStateKindRepr` discriminant.
    pub kind: u32,
    /// `ColorWriteMask::bits()` for the Disabled case; ignored when
    /// `kind == Enabled`.
    pub color_write_mask: u32,
    pub attachment: ColorBlendAttachmentRepr,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::DepthFormat`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthFormatRepr {
    D16Unorm = 0,
    D32Sfloat = 1,
    D24UnormS8Uint = 2,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::AttachmentFormats`.
///
/// `color` is a slice of `TextureFormat::repr(u32)` discriminants
/// (from `streamlib_consumer_rhi::TextureFormat`'s `#[repr(u32)]`).
/// `has_depth` is a boolean encoded as `u32`; when `has_depth == 0`,
/// `depth` is ignored.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AttachmentFormatsRepr {
    /// Pointer to an array of `streamlib_consumer_rhi::TextureFormat`
    /// discriminants.
    pub color_ptr: *const u32,
    pub color_len: usize,
    /// 0 = `None`, 1 = `Some(depth)`.
    pub has_depth: u32,
    /// `DepthFormatRepr` discriminant. Ignored when `has_depth == 0`.
    pub depth: u32,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::GraphicsDynamicState`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsDynamicStateRepr {
    None = 0,
    ViewportScissor = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsPipelineState`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsPipelineStateRepr {
    /// `PrimitiveTopologyRepr` discriminant.
    pub topology: u32,
    pub _reserved_padding1: u32,
    pub vertex_input: VertexInputStateRepr,
    pub rasterization: RasterizationStateRepr,
    pub multisample: MultisampleStateRepr,
    pub depth_stencil: DepthStencilStateRepr,
    pub color_blend: ColorBlendStateRepr,
    pub attachment_formats: AttachmentFormatsRepr,
    /// `GraphicsDynamicStateRepr` discriminant.
    pub dynamic_state: u32,
    pub _reserved_padding2: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::GraphicsKernelDescriptor`.
///
/// All pointer fields borrow into caller-owned memory and must
/// remain valid for the duration of the vtable call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphicsKernelDescriptorRepr {
    pub label_ptr: *const u8,
    pub label_len: usize,
    pub stages_ptr: *const GraphicsStageRepr,
    pub stages_len: usize,
    pub bindings_ptr: *const GraphicsBindingSpecRepr,
    pub bindings_len: usize,
    pub push_constants: GraphicsPushConstantsRepr,
    pub pipeline_state: GraphicsPipelineStateRepr,
    pub descriptor_sets_in_flight: u32,
    pub _reserved_padding: u32,
}

// -----------------------------------------------------------------------------
// Ray-tracing kernel mirrors
// -----------------------------------------------------------------------------

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::RayTracingShaderStage`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingShaderStageRepr {
    RayGen = 0,
    Miss = 1,
    ClosestHit = 2,
    AnyHit = 3,
    Intersection = 4,
    Callable = 5,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RayTracingStage`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingStageRepr {
    /// `RayTracingShaderStageRepr` discriminant.
    pub stage: u32,
    pub _reserved_padding: u32,
    pub spv_ptr: *const u8,
    pub spv_len: usize,
    pub entry_point_ptr: *const u8,
    pub entry_point_len: usize,
}

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::RayTracingBindingKind`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingBindingKindRepr {
    StorageBuffer = 0,
    UniformBuffer = 1,
    SampledTexture = 2,
    StorageImage = 3,
    AccelerationStructure = 4,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RayTracingBindingSpec`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingBindingSpecRepr {
    pub binding: u32,
    /// `RayTracingBindingKindRepr` discriminant.
    pub kind: u32,
    /// `RayTracingShaderStageFlags::bits()`.
    pub stages: u32,
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RayTracingPushConstants`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingPushConstantsRepr {
    pub size: u32,
    /// `RayTracingShaderStageFlags::bits()`.
    pub stages: u32,
}

/// Discriminant for the [`RayTracingShaderGroupRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayTracingShaderGroupKindRepr {
    General = 0,
    TrianglesHit = 1,
    ProceduralHit = 2,
}

/// Sentinel value for `Option<u32>` stage references; matches
/// `VK_SHADER_UNUSED_KHR == ~0u`. Reserved by the Vulkan spec and
/// never a valid in-range stage index.
pub const RAY_TRACING_SHADER_UNUSED: u32 = u32::MAX;

/// Tagged-union mirror of `streamlib::core::rhi::RayTracingShaderGroup`.
///
/// Field interpretation per `kind`:
/// - `General`: `general_or_intersection` carries the general stage
///   index; `closest_hit` / `any_hit` are [`RAY_TRACING_SHADER_UNUSED`].
/// - `TrianglesHit`: `general_or_intersection` is
///   [`RAY_TRACING_SHADER_UNUSED`]; `closest_hit` / `any_hit` carry the
///   shader indices ([`RAY_TRACING_SHADER_UNUSED`] = `None`).
/// - `ProceduralHit`: `general_or_intersection` carries the
///   intersection stage index; `closest_hit` / `any_hit` carry the
///   optional shader indices.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingShaderGroupRepr {
    /// `RayTracingShaderGroupKindRepr` discriminant.
    pub kind: u32,
    /// General stage (General) / intersection stage (ProceduralHit).
    pub general_or_intersection: u32,
    /// Closest-hit stage. [`RAY_TRACING_SHADER_UNUSED`] = absent.
    pub closest_hit: u32,
    /// Any-hit stage. [`RAY_TRACING_SHADER_UNUSED`] = absent.
    pub any_hit: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::RayTracingKernelDescriptor`.
///
/// All pointer fields borrow into caller-owned memory and must
/// remain valid for the duration of the vtable call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RayTracingKernelDescriptorRepr {
    pub label_ptr: *const u8,
    pub label_len: usize,
    pub stages_ptr: *const RayTracingStageRepr,
    pub stages_len: usize,
    pub groups_ptr: *const RayTracingShaderGroupRepr,
    pub groups_len: usize,
    pub bindings_ptr: *const RayTracingBindingSpecRepr,
    pub bindings_len: usize,
    pub push_constants: RayTracingPushConstantsRepr,
    pub max_recursion_depth: u32,
    pub _reserved_padding: u32,
}

// =============================================================================
// GpuContextFullAccessVTable — extern "C" dispatch for privileged GPU work
// =============================================================================

/// Dispatch table for the host's `GpuContextFullAccess`. The cdylib
/// obtains a handle inside an `escalate(|full| ...)` scope (via the
/// `escalate_begin` / `escalate_end` callbacks landing in C3) and
/// reads the static vtable from
/// [`HostServices::gpu_context_full_access_vtable`].
///
/// C2 lands the descriptor wire format + host-side dispatch + cdylib-
/// side β-shape. C3 wires the `escalate_begin` / `escalate_end` scope-
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

    /// Bump the refcount on a `VulkanComputeKernel` β-shape handle.
    /// Called by the cdylib's `Clone for VulkanComputeKernel`. Host
    /// runs `Arc::increment_strong_count(handle as *const VulkanComputeKernelInner)`
    /// against the host-internal Inner type — cdylib never sees the
    /// Inner layout.
    pub clone_compute_kernel: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VulkanComputeKernel` β-shape handle.
    /// Host runs `Arc::decrement_strong_count` against the Inner type.
    pub drop_compute_kernel: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // VulkanGraphicsKernel return-type lifetime
    // -------------------------------------------------------------------------

    /// Bump the refcount on a `VulkanGraphicsKernel` β-shape handle.
    /// Host runs `Arc::increment_strong_count(handle as *const VulkanGraphicsKernelInner)`.
    pub clone_graphics_kernel: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VulkanGraphicsKernel` β-shape handle.
    pub drop_graphics_kernel: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // VulkanRayTracingKernel return-type lifetime
    // -------------------------------------------------------------------------

    /// Bump the refcount on a `VulkanRayTracingKernel` β-shape handle.
    /// Host runs `Arc::increment_strong_count(handle as *const VulkanRayTracingKernelInner)`.
    pub clone_ray_tracing_kernel: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VulkanRayTracingKernel` β-shape handle.
    pub drop_ray_tracing_kernel: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // TextureRing return-type lifetime
    // -------------------------------------------------------------------------

    /// Bump the refcount on a `TextureRing` β-shape handle.
    /// Host runs `Arc::increment_strong_count(handle as *const TextureRingInner)`.
    pub clone_texture_ring: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `TextureRing` β-shape handle.
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
    /// fresh `Texture` β-shape into `*out_texture` on success.
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
    /// `out_texture` receives the [`crate::Texture`] β-shape.
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
    /// the cdylib's `VulkanAccelerationStructure` β-shape carries:
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
    /// cdylib's β-shape carries — same shape as
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

    /// Populate a [`GpuCapabilitiesRepr`] with vendor name + capability
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
    /// **Arc-raw-pointer transit** — not a layout-stable β-shape.
    /// In-tree consumers (camera, display) ride this freely because
    /// they're built in the same workspace as the engine. Cross-repo
    /// plugin distribution will need a β-shape lift for
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
    /// `StorageBuffer` β-shape struct (32 bytes, layout-stable from
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
}

unsafe impl Send for GpuContextFullAccessVTable {}
unsafe impl Sync for GpuContextFullAccessVTable {}

// =============================================================================
// TextureRingMethodsVTable — per-type vtable for TextureRing method dispatch
// =============================================================================

/// Per-type method-dispatch vtable for the `TextureRing` β-shape
/// (issue #907 Phase E + #947 slot β-shape + method dispatch).
///
/// `TextureRing` keeps `clone_*` / `drop_*` dispatch on the parent
/// [`GpuContextFullAccessVTable`] (PR #918's Phase D shape); this
/// vtable adds per-method slots for everything *else* the β-shape
/// exposes — `acquire_next`, `copy_pixel_buffer_to_slot`, `slot`.
/// POD getters (`len`, `is_empty`, `width`, `height`, `format`) are
/// cached on the β-shape struct itself and don't need vtable slots.
///
/// **Slot-return shape (v2):** `acquire_next` and `slot` return the
/// slot's owned typed POD bytes via caller-provided out-parameter
/// buffers (`out_texture_handle` + cached POD slot for the slot's
/// `Texture`, plus `out_surface_id_bytes` + `out_surface_id_len` +
/// `out_slot_index`). The slot's `Texture` β-shape itself manages
/// its Arc lifetime through the parent
/// [`GpuContextLimitedAccessVTable`]'s `clone_texture` /
/// `drop_texture` slots — when the host wrapper hands a cloned
/// `Texture` handle back, the cdylib's `Texture::Drop` will fire
/// `drop_texture` to balance the clone. Surface IDs travel inline
/// as fixed 64-byte buffers (UUIDs are 36 ASCII chars; the 64-byte
/// budget leaves headroom for the bytes + length without a heap
/// allocation crossing the DSO boundary).
///
/// **`copy_pixel_buffer_to_slot` (v2):** the caller passes the
/// slot's `slot_index` (looks up the slot's pre-allocated upload
/// resources host-side) and `surface_id_bytes` + `surface_id_len`
/// (used to refresh the texture-cache registration's layout to
/// `SHADER_READ_ONLY_OPTIMAL` post-upload). No slot deref needed
/// across the boundary — the cdylib's `TextureRingSlot` carries
/// both fields as inline POD.
#[repr(C)]
pub struct TextureRingMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Rotate to the next slot. Writes the slot's `Texture` handle
    /// (cloned through the parent limited-access vtable, so the
    /// returned handle carries its own Arc strong count balanced
    /// by `Texture::Drop`), the cached `Texture` POD descriptors
    /// (width/height/format), the slot's `surface_id` bytes +
    /// length, and the slot index into the caller-provided
    /// out-parameter buffers. Returns 0 on success; non-zero with
    /// UTF-8 message in `err_buf` on failure (e.g. null ring
    /// handle).
    pub acquire_next: unsafe extern "C" fn(
        ring_handle: *const c_void,
        out_texture_handle: *mut *const c_void,
        out_texture_width: *mut u32,
        out_texture_height: *mut u32,
        out_texture_format_raw: *mut u32,
        out_surface_id_bytes: *mut [u8; 64],
        out_surface_id_len: *mut u32,
        out_slot_index: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Write a host-staged pixel buffer's contents into the slot's
    /// pre-allocated texture (the Limited-safe per-frame primitive).
    /// `slot_index` identifies the slot's pre-allocated upload
    /// resources host-side; `surface_id_bytes` + `surface_id_len`
    /// identify the texture-cache registration whose layout is
    /// refreshed to `SHADER_READ_ONLY_OPTIMAL` post-upload. Returns
    /// 0 on success; non-zero with UTF-8 message in `err_buf` on
    /// failure (slot_index out of range, surface_id not valid
    /// UTF-8, GPU submit error, etc.).
    pub copy_pixel_buffer_to_slot: unsafe extern "C" fn(
        ring_handle: *const c_void,
        slot_index: u32,
        surface_id_bytes: *const u8,
        surface_id_len: u32,
        pixel_buffer_handle: *const c_void,
        width: u32,
        height: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Look up a slot by index. Same out-parameter shape as
    /// `acquire_next`. Returns 0 on success; returns -1 (NOT 1) with
    /// no `err_buf` write when `index` is out of range — the
    /// distinction lets the caller `Option::None` cleanly without
    /// allocating an error string. Returns 1 on a hard failure
    /// (null ring handle, etc.) with UTF-8 message in `err_buf`.
    pub slot: unsafe extern "C" fn(
        ring_handle: *const c_void,
        index: usize,
        out_texture_handle: *mut *const c_void,
        out_texture_width: *mut u32,
        out_texture_height: *mut u32,
        out_texture_format_raw: *mut u32,
        out_surface_id_bytes: *mut [u8; 64],
        out_surface_id_len: *mut u32,
        out_slot_index: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for TextureRingMethodsVTable {}
unsafe impl Sync for TextureRingMethodsVTable {}

// =============================================================================
// VulkanComputeKernelMethodsVTable — per-type vtable for compute-kernel method dispatch
// =============================================================================

/// Per-type method-dispatch vtable for the `VulkanComputeKernel`
/// β-shape (issue #907 Phase E + #949 method-dispatch first slice).
///
/// `VulkanComputeKernel` keeps `clone_*` / `drop_*` dispatch on the
/// parent [`GpuContextFullAccessVTable`] (PR #918's Phase D shape);
/// this vtable carries per-method slots for everything the plugin
/// handle exposes that cdylib code needs to dispatch through.
///
/// **Binding-method shape:** typed-by-input-wrapper (one slot per
/// kernel-method × buffer-or-texture wrapper). This mirrors the
/// production cross-DSO pattern used by Dawn / WebGPU (`WGPUBuffer`
/// + per-binding-kind method) and Unreal RHI (typed
/// `SetShaderResourceViewParameter` methods) while honoring
/// streamlib's existing typed-wrapper allocation layer (separate
/// `PixelBuffer` / `StorageBuffer` / `UniformBuffer` Rust types).
/// The longer-term option of collapsing typed wrappers into one
/// `Buffer` + flags primitive is tracked separately in the
/// **RHI Buffer Model Alignment** milestone and would simplify this
/// vtable further; until then, per-type slots are the right shape.
///
/// **Coverage today** (v3): `set_push_constants`, `dispatch`,
/// `set_storage_buffer_pixel(&PixelBuffer)`,
/// `set_storage_buffer_storage(&StorageBuffer)`,
/// `set_uniform_buffer(&UniformBuffer)`,
/// `set_sampled_texture(&Texture)`,
/// `set_storage_image(&Texture)`.
///
/// The engine-only methods (`record(cmd_buf)`,
/// `set_*_image_view(vk::ImageView)`, `bindings() -> Vec<ComputeBindingSpec>`)
/// stay `host_inner`-routed — their parameter / return types are
/// host-internal vulkanalia or allocator-crossing.
#[repr(C)]
pub struct VulkanComputeKernelMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Upload push-constant bytes. `bytes_len` should match the
    /// kernel's declared `push_constant_size` (already cached on
    /// the plugin handle). Returns 0 on success; non-zero with UTF-8
    /// message in `err_buf` on failure.
    pub set_push_constants: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        bytes_ptr: *const u8,
        bytes_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Dispatch the kernel with the given workgroup counts. Returns
    /// 0 on success; non-zero with UTF-8 message in `err_buf` on
    /// failure (GPU submission error, fence wait timeout, etc.).
    pub dispatch: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        group_x: u32,
        group_y: u32,
        group_z: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a [`PixelBuffer`](struct@crate)-shaped storage buffer
    /// (SSBO) at `binding`. `pixel_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<PixelBufferRef>)` pointer the plugin
    /// handle carries; the host wrapper reconstructs the borrow and
    /// forwards. Returns 0 on success; non-zero with UTF-8 message
    /// in `err_buf` on declaration mismatch / unset binding / null
    /// handle.
    pub set_storage_buffer_pixel: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        pixel_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a raw-bytes-shaped storage buffer (SSBO) at `binding`.
    /// `storage_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer the plugin
    /// handle carries.
    pub set_storage_buffer_storage: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        storage_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a uniform buffer (UBO) at `binding`.
    /// `uniform_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer the plugin
    /// handle carries.
    pub set_uniform_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        uniform_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a sampled texture at `binding` using the kernel's
    /// default linear-clamp sampler. `texture_handle` is the raw
    /// `Arc::into_raw(Arc<TextureInner>)` pointer the plugin
    /// handle carries.
    pub set_sampled_texture: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a storage image at `binding`. Caller guarantees the
    /// underlying texture's `STORAGE_BINDING` usage was declared at
    /// creation time.
    pub set_storage_image: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanComputeKernelMethodsVTable {}
unsafe impl Sync for VulkanComputeKernelMethodsVTable {}

// =============================================================================
// VulkanGraphicsKernelMethodsVTable — per-type vtable for graphics-kernel method dispatch
// =============================================================================

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::IndexType`.
///
/// Discriminant carried on [`DrawIndexedCallRepr`]'s sibling index-buffer
/// binding slot ([`VulkanGraphicsKernelMethodsVTable::set_index_buffer`]).
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IndexTypeRepr {
    Uint16 = 0,
    Uint32 = 1,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::Viewport`.
///
/// Field order matches the host-side struct; layout-locked by the
/// regression test in `layout_tests`.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ViewportRepr {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::ScissorRect`.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ScissorRectRepr {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::DrawCall`.
///
/// `Option<Viewport>` / `Option<ScissorRect>` are encoded as
/// `<field>_present: u32` discriminator + a zero-initialized payload
/// when absent — `Option<T>` has no stable `#[repr(C)]` shape.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct DrawCallRepr {
    pub vertex_count: u32,
    pub instance_count: u32,
    pub first_vertex: u32,
    pub first_instance: u32,
    /// `1` when [`Self::viewport`] carries a value; `0` for `None`.
    pub viewport_present: u32,
    /// `1` when [`Self::scissor`] carries a value; `0` for `None`.
    pub scissor_present: u32,
    pub viewport: ViewportRepr,
    pub scissor: ScissorRectRepr,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::DrawIndexedCall`.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct DrawIndexedCallRepr {
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub vertex_offset: i32,
    pub first_instance: u32,
    /// `1` when [`Self::viewport`] carries a value; `0` for `None`.
    pub viewport_present: u32,
    /// `1` when [`Self::scissor`] carries a value; `0` for `None`.
    pub scissor_present: u32,
    pub _reserved_padding: u32,
    pub viewport: ViewportRepr,
    pub scissor: ScissorRectRepr,
}

/// Discriminant for the [`OffscreenDrawRepr::kind`] tagged union.
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OffscreenDrawKindRepr {
    Draw = 0,
    DrawIndexed = 1,
}

/// `#[repr(C)]` tagged-union mirror of
/// `streamlib::vulkan::rhi::OffscreenDraw`.
///
/// Only the slot whose tag matches [`Self::kind`] is read:
/// - `Draw` → [`Self::draw_call`]
/// - `DrawIndexed` → [`Self::draw_indexed_call`]
///
/// The unused slot is zero-initialized; the host wrapper only
/// inspects the kind-matched payload.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct OffscreenDrawRepr {
    /// [`OffscreenDrawKindRepr`] discriminant.
    pub kind: u32,
    pub _reserved_padding: u32,
    pub draw_call: DrawCallRepr,
    pub draw_indexed_call: DrawIndexedCallRepr,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::color_converter::SourceLayoutInfo`.
///
/// Plane strides + UV-plane offset for the buffer→image kernel's
/// SSBO walk. Layout-locked by the regression test in `layout_tests`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceLayoutInfoRepr {
    /// Y plane (NV12) or packed plane (YUYV) row stride in bytes.
    pub plane0_stride_bytes: u32,
    /// UV plane row stride in bytes for NV12; zero for YUYV.
    pub plane1_stride_bytes: u32,
    /// Offset of the UV plane from the start of the source SSBO,
    /// in bytes. Zero for YUYV (single plane).
    pub plane1_offset_bytes: u32,
    /// Reserved padding so the struct stays 4-byte-multiple sized
    /// and naturally aligned; zero today, never read.
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of
/// `streamlib::vulkan::rhi::vulkan_command_recorder::ImageCopyRegion`.
///
/// Buffer↔image copy region — single mip level, single array layer,
/// color aspect, full image. Used by
/// [`RhiCommandRecorderMethodsVTable::record_copy_image_to_buffer`]
/// to cross the cdylib boundary without dragging callers through
/// `vulkanalia` imports. Layout-locked by the regression test in
/// `layout_tests`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageCopyRegionRepr {
    /// Image extent width in pixels.
    pub width: u32,
    /// Image extent height in pixels.
    pub height: u32,
    /// Byte offset within the buffer where the copy begins.
    pub buffer_offset: u64,
    /// Buffer row length in pixels (matches image width for tightly
    /// packed copies).
    pub buffer_row_length: u32,
    /// Buffer image height in pixels (matches image height for
    /// tightly packed copies).
    pub buffer_image_height: u32,
    /// Mip level to copy into / out of.
    pub mip_level: u32,
    /// Array layer to copy into / out of.
    pub array_layer: u32,
    /// Reserved padding so the struct stays 8-byte-aligned and the
    /// trailing bytes of the last 4-byte field are deterministic;
    /// zero today, never read.
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::color::ResolvedColorInfo`.
///
/// Each field is the matching engine-side `#[repr(u32)]` enum's
/// discriminant: `primaries_raw` mirrors `PrimariesId`, `transfer_raw`
/// mirrors `TransferId`, `matrix_raw` mirrors `MatrixId`, `range_raw`
/// mirrors `RangeId`. Layout-locked by the regression test in
/// `layout_tests`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolvedColorInfoRepr {
    /// `PrimariesId` discriminant.
    pub primaries_raw: u32,
    /// `TransferId` discriminant.
    pub transfer_raw: u32,
    /// `MatrixId` discriminant.
    pub matrix_raw: u32,
    /// `RangeId` discriminant.
    pub range_raw: u32,
}

/// Per-type method-dispatch vtable for the `RhiColorConverter`
/// β-shape (Phase E sub-lift slice A).
///
/// `RhiColorConverter` keeps `clone_color_converter` /
/// `drop_color_converter` dispatch on the parent
/// [`GpuContextFullAccessVTable`]; this vtable carries the
/// `prepare_buffer_to_image_storage` slot so cdylib camera processors
/// can prepare the host's cached buffer→image kernel without tripping
/// the β-shape's host-mode-only `host_inner()` access. The slot
/// returns an opaque `Arc<VulkanComputeKernelInner>`-shaped handle
/// plus the kernel's `push_constant_size`; the cdylib reconstructs a
/// `VulkanComputeKernel` β-shape via the host_callbacks() per-type
/// methods vtable lookup.
#[repr(C)]
pub struct RhiColorConverterMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Bind source / destination / push-constants on the converter's
    /// buffer→image kernel and return a fresh
    /// `Arc<VulkanComputeKernelInner>`-shaped opaque handle the cdylib
    /// wraps into its `VulkanComputeKernel` β-shape via the
    /// `host_callbacks().vulkan_compute_kernel_methods_vtable` lookup.
    ///
    /// - `converter_handle` is
    ///   `Arc::as_ptr(Arc<RhiColorConverterInner>)`-shaped (borrowed;
    ///   the host does not bump the converter's refcount).
    /// - `src_buffer_handle` is
    ///   `Arc::into_raw(Arc<HostVulkanBufferInner>)`-shaped from the
    ///   `StorageBuffer` β-shape's `handle` field (borrowed; the
    ///   cdylib retains ownership).
    /// - `dst_texture_handle` is
    ///   `Arc::into_raw(Arc<TextureInner>)`-shaped from the `Texture`
    ///   β-shape's `handle` field (borrowed; the cdylib retains
    ///   ownership).
    /// - `dst_transfer_raw` is the `#[repr(u32)]` discriminant of
    ///   `streamlib::core::color::TransferId`.
    ///
    /// On success writes a bumped
    /// `Arc::into_raw(Arc<VulkanComputeKernelInner>)`-shaped pointer
    /// into `*out_kernel` (the cdylib owns the bumped strong count
    /// and releases it via the parent vtable's
    /// `drop_compute_kernel`) and the kernel's `push_constant_size`
    /// into `*out_cached_push_constant_size`. Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on failure. Linux-only
    /// on the host side; non-Linux stubs return non-zero.
    pub prepare_buffer_to_image_storage: unsafe extern "C" fn(
        converter_handle: *const c_void,
        src_buffer_handle: *const c_void,
        src_layout: *const SourceLayoutInfoRepr,
        dst_texture_handle: *const c_void,
        info: *const ResolvedColorInfoRepr,
        dst_transfer_raw: u32,
        out_kernel: *mut *const c_void,
        out_cached_push_constant_size: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for RhiColorConverterMethodsVTable {}
unsafe impl Sync for RhiColorConverterMethodsVTable {}

/// Per-type method-dispatch vtable for the `RhiCommandRecorder`
/// β-shape (Phase E sub-lift slice B — #984).
///
/// `RhiCommandRecorder` keeps `clone_command_recorder` /
/// `drop_command_recorder` dispatch on the parent
/// [`GpuContextFullAccessVTable`]; this vtable carries per-method
/// slots for the six camera-hot-path methods cdylib code needs to
/// dispatch through (`begin`, `record_image_barrier`,
/// `record_buffer_barrier`, `record_dispatch`,
/// `record_copy_image_to_buffer`, `submit_signaling_timeline`).
/// Without these the β-shape's `host_inner_mut()` / `host_inner()`
/// panic-guards fire from cdylib code on every per-frame call.
///
/// The remaining `RhiCommandRecorder` methods (`record_draw`,
/// `record_draw_indexed`, `record_copy_buffer_to_image`, `submit`,
/// `submit_and_wait`, `submit_with_semaphores`, `command_buffer_raw`,
/// `vulkan_device_ref`) keep their cdylib-mode panic in place — they
/// don't sit on the camera hot path and a follow-up slice lifts
/// them when a consumer arrives.
///
/// **Buffer-flavor coverage today:** the v1 `record_buffer_barrier`
/// and `record_copy_image_to_buffer` slots accept a
/// `StorageBuffer`-shaped handle. v2 added `record_pixel_buffer_barrier`
/// and `record_copy_image_to_pixel_buffer` sibling slots — the
/// camera's per-frame path copies the compute output into a pooled
/// `PixelBuffer` and barriers it through `HOST_READ`. Future
/// consumers needing uniform / vertex / index buffer barriers add
/// further sibling slots rather than discriminating on these (same
/// pattern as `VulkanGraphicsKernelMethodsVTable`'s
/// `set_storage_buffer_pixel` / `set_storage_buffer_storage`).
#[repr(C)]
pub struct RhiCommandRecorderMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Begin a new recording. `recorder_handle` is the
    /// `Box::into_raw(Box<RhiCommandRecorderInner>)` pointer from
    /// the β-shape's `handle` field. Returns 0 on success; non-zero
    /// with UTF-8 message in `err_buf` on failure. Linux-only on the
    /// host side; non-Linux stubs return non-zero.
    pub begin: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record an image layout transition. Layout / stage / access
    /// enumerants travel as their raw integer types: `VulkanLayout`
    /// is `i32` (matches the `VkImageLayout` enumerant);
    /// `VulkanStage` and `VulkanAccess` are `i64` (the
    /// `VK_PIPELINE_STAGE_2_*` and `VK_ACCESS_2_*` bitmasks are
    /// 64-bit).
    ///
    /// - `recorder_handle` is the
    ///   `Box::into_raw(Box<RhiCommandRecorderInner>)` pointer.
    /// - `texture_handle` is
    ///   `Arc::into_raw(Arc<TextureInner>)`-shaped from the
    ///   `Texture` β-shape's `handle` field (borrowed).
    pub record_image_barrier: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        texture_handle: *const c_void,
        from_layout_raw: i32,
        to_layout_raw: i32,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record a buffer memory barrier covering the whole buffer.
    /// Today the camera only uses storage-buffer barriers; this
    /// slot accepts a `StorageBuffer`-shaped handle. The host
    /// reconstructs the typed borrow via
    /// `make_storage_buffer_borrow`. Future consumers needing
    /// uniform / vertex / index buffer barriers add sibling slots,
    /// not a discriminator on this one.
    ///
    /// - `storage_buffer_handle` is
    ///   `Arc::into_raw(Arc<HostVulkanBufferInner>)`-shaped from
    ///   the `StorageBuffer` β-shape's `handle` field (borrowed).
    pub record_buffer_barrier: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        storage_buffer_handle: *const c_void,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record a compute dispatch via
    /// `VulkanComputeKernel::record`. `kernel_handle` is the
    /// `Arc::into_raw(Arc<VulkanComputeKernelInner>)` pointer from
    /// the kernel β-shape's `handle` field (borrowed).
    pub record_dispatch: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        kernel_handle: *const c_void,
        group_x: u32,
        group_y: u32,
        group_z: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record `vkCmdCopyImageToBuffer`. Storage-buffer-shape
    /// destination only today; mirrors the
    /// `record_buffer_barrier` constraint.
    ///
    /// - `src_texture_handle` is
    ///   `Arc::into_raw(Arc<TextureInner>)`-shaped from the source
    ///   `Texture` β-shape's `handle` field (borrowed).
    /// - `dst_storage_buffer_handle` is
    ///   `Arc::into_raw(Arc<HostVulkanBufferInner>)`-shaped from the
    ///   destination `StorageBuffer` β-shape's `handle` field
    ///   (borrowed).
    /// - `region` points at an [`ImageCopyRegionRepr`] the host
    ///   reads once at call time.
    pub record_copy_image_to_buffer: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        src_texture_handle: *const c_void,
        src_layout_raw: i32,
        dst_storage_buffer_handle: *const c_void,
        region: *const ImageCopyRegionRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// End recording and submit, signaling `timeline` at
    /// `signal_value` on completion. `timeline_handle` is a borrow
    /// of `&HostVulkanTimelineSemaphore` (`self as *const Self`
    /// shape — same pattern as the v13 `wait_timeline_semaphore`
    /// slot on `GpuContextLimitedAccessVTable`). The host does not
    /// bump the timeline's refcount.
    pub submit_signaling_timeline: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        timeline_handle: *const c_void,
        signal_value: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// v2 sibling of `record_buffer_barrier` for `PixelBuffer`-shaped
    /// destinations. The camera's per-frame path uses this after the
    /// `vkCmdCopyImageToBuffer` to barrier the pooled pixel buffer
    /// from `TRANSFER_WRITE` to `HOST_READ` so the IPC consumer can
    /// map it.
    ///
    /// - `pixel_buffer_handle` is
    ///   `Arc::into_raw(Arc<PixelBufferRef>)`-shaped from the
    ///   `PixelBuffer` β-shape's `handle` field (borrowed).
    pub record_pixel_buffer_barrier: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        pixel_buffer_handle: *const c_void,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// v2 sibling of `record_copy_image_to_buffer` for
    /// `PixelBuffer`-shaped destinations. The camera's per-frame
    /// path copies the compute output (a host-allocated
    /// `Texture` ring slot) into a pooled `PixelBuffer` for
    /// cross-process IPC.
    ///
    /// - `src_texture_handle` is
    ///   `Arc::into_raw(Arc<TextureInner>)`-shaped from the source
    ///   `Texture` β-shape's `handle` field (borrowed).
    /// - `dst_pixel_buffer_handle` is
    ///   `Arc::into_raw(Arc<PixelBufferRef>)`-shaped from the
    ///   destination `PixelBuffer` β-shape's `handle` field
    ///   (borrowed).
    /// - `region` points at an [`ImageCopyRegionRepr`] the host
    ///   reads once at call time.
    pub record_copy_image_to_pixel_buffer: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        src_texture_handle: *const c_void,
        src_layout_raw: i32,
        dst_pixel_buffer_handle: *const c_void,
        region: *const ImageCopyRegionRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for RhiCommandRecorderMethodsVTable {}
unsafe impl Sync for RhiCommandRecorderMethodsVTable {}

// =============================================================================
// OutputWriterVTable — extern "C" dispatch for the cdylib's OutputWriter
// β-shape
// =============================================================================

/// `extern "C" fn` dispatch table for the cdylib's `OutputWriter`
/// β-shape. Replaces the shared-Rust-type `Arc<OutputWriter>`
/// crossing the cdylib used to expose to the host via
/// `ProcessorVTable::get_iceoryx2_output_writer_arc`.
///
/// Today the host allocates an `Arc<OutputWriterInner>` and hands
/// the cdylib a `(handle, vtable)` β-shape that delegates every
/// public-API call through this vtable. Hot-path emits cross extern
/// "C" once per `write` call; the bytes carry msgpack-encoded
/// frames the cdylib serialized in its own DSO.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0. Older vtables loaded
/// into newer hosts are rejected cleanly. New fields append after
/// `drop_arc` and bump [`OUTPUT_WRITER_VTABLE_LAYOUT_VERSION`].
///
/// # Error convention
///
/// `write_raw` returns `0` on success, non-zero on failure. The
/// caller-provided `err_buf` / `err_buf_cap` is a UTF-8 scratch
/// buffer the callee writes a message into; `*err_len` receives
/// the actual byte count written. Truncation is benign.
///
/// `clone_arc` / `drop_arc` are infallible — they bump / decrement
/// the host-side `Arc<OutputWriterInner>` strong count. `clone_arc`
/// returns the same opaque handle (the underlying inner is the same
/// object); the cdylib pairs each `clone_arc` with exactly one
/// `drop_arc` to keep refcount accounting balanced.
#[repr(C)]
pub struct OutputWriterVTable {
    /// Vtable layout version. Must equal
    /// [`OUTPUT_WRITER_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned; zero today, never read).
    pub _reserved_padding: u32,

    /// Write a raw msgpack-encoded frame to the named output port at
    /// the given timestamp. The cdylib serializes `T` to msgpack in
    /// its own DSO and passes the bytes through; the host then runs
    /// the underlying iceoryx2 publish + notify. Returns `0` on
    /// success, non-zero on failure.
    pub write_raw: unsafe extern "C" fn(
        handle: *const c_void,
        port_ptr: *const u8,
        port_len: usize,
        data_ptr: *const u8,
        data_len: usize,
        timestamp_ns: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check whether a port has been configured. Returns `true` if
    /// the host's `OutputWriterInner` has at least one
    /// `add_connection` entry for the named port.
    pub has_port: unsafe extern "C" fn(
        handle: *const c_void,
        port_ptr: *const u8,
        port_len: usize,
    ) -> bool,

    /// Bump the host-side `Arc<OutputWriterInner>` strong count.
    /// Returns the same opaque handle (the cdylib uses the same
    /// handle in subsequent calls). Pairs 1:1 with `drop_arc`.
    pub clone_arc: unsafe extern "C" fn(handle: *const c_void) -> *const c_void,

    /// Decrement the host-side `Arc<OutputWriterInner>` strong
    /// count. Releases the inner when the count reaches zero.
    pub drop_arc: unsafe extern "C" fn(handle: *const c_void),
}

// Safety: every field is a primitive or an `extern "C" fn` pointer.
// The vtable's `&'static` storage outlives the cdylib's process
// lifetime via the `LOADED_PLUGIN_LIBRARIES` pinning shape.
unsafe impl Send for OutputWriterVTable {}
unsafe impl Sync for OutputWriterVTable {}

// =============================================================================
// InputMailboxesVTable — extern "C" dispatch for the cdylib's
// InputMailboxes β-shape
// =============================================================================

/// `extern "C" fn` dispatch table for the cdylib's `InputMailboxes`
/// β-shape. Replaces the shared-Rust-type `&mut InputMailboxes`
/// crossing the cdylib used to expose to the host via
/// `ProcessorVTable::get_iceoryx2_input_mailboxes_mut`.
///
/// The cdylib's `process()` body reaches input data through this
/// vtable: `read_raw` consumes the next queued frame for a port
/// according to its read mode, `has_data` queries without consuming.
/// All other `InputMailboxes` methods (`add_port`, `set_subscriber`,
/// `set_listener`, `listener_fd`, `drain_listener`,
/// `receive_pending`, `route`, `any_port_has_data`) are host-side
/// only and do not appear on this vtable.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0. Older vtables loaded
/// into newer hosts are rejected cleanly. New fields append after
/// `has_data` and bump [`INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION`].
///
/// # Error convention
///
/// `read_raw` returns `0` on success, non-zero on host-side
/// failure (a malformed inbound frame, allocator failure, etc.).
/// On success, `*has_data` distinguishes "consumed a frame"
/// (`true`) from "no frames queued" (`false`). When `*has_data ==
/// true`, the callee writes the raw msgpack-encoded frame body to
/// `out_buf` and the timestamp to `*out_timestamp`. Truncation is
/// signaled by `*out_len > out_cap` and is treated as an error by
/// the cdylib (it resizes the buffer and retries).
///
/// `has_data` is infallible.
#[repr(C)]
pub struct InputMailboxesVTable {
    /// Vtable layout version. Must equal
    /// [`INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned; zero today, never read).
    pub _reserved_padding: u32,

    /// Consume the next queued raw frame for the named port. The
    /// host runs `receive_pending` first (draining iceoryx2 into
    /// the per-port mailbox) then pops according to the port's
    /// `ReadMode` (skip-to-latest for video, FIFO for audio).
    ///
    /// On entry `*out_len = 0`. On success the callee writes:
    /// - `*has_data = true` if a frame was consumed; the frame's
    ///   msgpack-encoded body is copied to `out_buf[..*out_len]`
    ///   and the frame's monotonic timestamp to `*out_timestamp`.
    /// - `*has_data = false` if the mailbox was empty.
    ///
    /// If the frame body is larger than `out_cap`, `*out_len` is
    /// set to the **required** length and the data is not copied —
    /// the cdylib resizes its buffer and retries.
    pub read_raw: unsafe extern "C" fn(
        handle: *const c_void,
        port_ptr: *const u8,
        port_len: usize,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        out_timestamp: *mut i64,
        has_data: *mut bool,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check whether the named port has at least one queued frame
    /// after draining iceoryx2's per-publisher buffer into the
    /// per-port mailbox. Returns `false` for unknown ports.
    pub has_data: unsafe extern "C" fn(
        handle: *const c_void,
        port_ptr: *const u8,
        port_len: usize,
    ) -> bool,
}

// Safety: every field is a primitive or an `extern "C" fn` pointer.
// The vtable's `&'static` storage outlives the cdylib's process
// lifetime via the `LOADED_PLUGIN_LIBRARIES` pinning shape.
unsafe impl Send for InputMailboxesVTable {}
unsafe impl Sync for InputMailboxesVTable {}

/// Per-type method-dispatch vtable for the `VulkanGraphicsKernel`
/// β-shape (issue #907 Phase E PR 3/5 + #951 method-dispatch slice).
///
/// `VulkanGraphicsKernel` keeps `clone_*` / `drop_*` dispatch on the
/// parent [`GpuContextFullAccessVTable`] (PR #918's Phase D shape);
/// this vtable carries per-method slots for the plugin handle's
/// binding + draw surface that cdylib code needs to dispatch through.
///
/// **Binding-method shape:** typed-by-input-wrapper (one slot per
/// kernel-method × buffer-or-texture wrapper). Mirrors the
/// `VulkanComputeKernelMethodsVTable` v3 shape and the production
/// cross-DSO patterns in Dawn / WebGPU + Unreal RHI.
///
/// **Coverage today** (v2):
/// - Binding slots: `set_storage_buffer_pixel`,
///   `set_storage_buffer_storage`, `set_uniform_buffer`,
///   `set_sampled_texture`, `set_storage_image`,
///   `set_vertex_buffer`, `set_index_buffer`.
/// - Primitive-argument slots: `set_push_constants`,
///   `offscreen_render`.
///
/// **Engine-only methods** (NOT on this vtable): `cmd_bind_and_draw`
/// and `cmd_bind_and_draw_indexed` accept a raw `vk::CommandBuffer`
/// from a caller-managed render-pass scope. They stay
/// `host_inner`-routed; cdylib code cannot mint a `vk::CommandBuffer`
/// without an `RhiCommandRecorder` β-shape (a separate concern). The
/// `bindings()` / `pipeline_state()` accessors return host-internal
/// types and stay `host_inner`-routed for the same reason.
#[repr(C)]
pub struct VulkanGraphicsKernelMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Bind a [`PixelBuffer`](struct@crate)-shaped storage buffer
    /// (SSBO) at `(frame_index, binding)`. `pixel_buffer_handle` is
    /// the raw `Arc::into_raw(Arc<PixelBufferRef>)` pointer the
    /// plugin handle carries. Returns 0 on success; non-zero with
    /// UTF-8 message in `err_buf` on failure.
    pub set_storage_buffer_pixel: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        pixel_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a raw-bytes storage buffer at `(frame_index, binding)`.
    /// `storage_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer.
    pub set_storage_buffer_storage: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        storage_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a uniform buffer (UBO) at `(frame_index, binding)`.
    /// `uniform_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer.
    pub set_uniform_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        uniform_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a sampled texture at `(frame_index, binding)` using the
    /// kernel's default linear-clamp sampler.
    pub set_sampled_texture: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a storage image at `(frame_index, binding)`. Caller
    /// guarantees the underlying texture's `STORAGE_BINDING` usage
    /// was declared at creation time.
    pub set_storage_image: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a vertex buffer at `(frame_index, binding)`. `binding`
    /// must match a `VertexInputBinding` declared in the pipeline's
    /// vertex input state. `vertex_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer.
    pub set_vertex_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        vertex_buffer_handle: *const c_void,
        offset: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind an index buffer at `frame_index`. `index_buffer_handle`
    /// is the raw `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer.
    /// `index_type` is the [`IndexTypeRepr`] discriminant.
    pub set_index_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        index_buffer_handle: *const c_void,
        offset: u64,
        index_type: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Stage push-constant bytes for `frame_index`. `bytes_len`
    /// should match the kernel's declared `push_constants.size`
    /// (already cached on the plugin handle). Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on failure.
    pub set_push_constants: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        bytes_ptr: *const u8,
        bytes_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Render into one or more offscreen color attachments using the
    /// kernel's owned command buffer + fence. Convenience for
    /// one-shot renderers (tests, smoke harnesses).
    ///
    /// Color targets travel as parallel arrays of the same length
    /// `target_count`:
    /// - `color_texture_handles`: `Arc::into_raw(Arc<TextureInner>)`
    ///   pointers, one per attachment.
    /// - `color_clear_present`: `1` per attachment that wants a
    ///   CLEAR load_op; `0` for LOAD.
    /// - `color_clear_values`: RGBA float clear color per
    ///   attachment; read only when the matching present flag is `1`.
    ///
    /// `draw` is the [`OffscreenDrawRepr`] tagged union (only the
    /// `kind`-matched payload is read on the host side).
    pub offscreen_render: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        color_texture_handles: *const *const c_void,
        color_clear_present: *const u32,
        color_clear_values: *const [f32; 4],
        target_count: usize,
        extent_width: u32,
        extent_height: u32,
        draw: *const OffscreenDrawRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanGraphicsKernelMethodsVTable {}
unsafe impl Sync for VulkanGraphicsKernelMethodsVTable {}

// =============================================================================
// VulkanRayTracingKernelMethodsVTable — per-type vtable for RT-kernel method dispatch
// =============================================================================

/// Per-type method-dispatch vtable for the `VulkanRayTracingKernel`
/// β-shape.
///
/// Mirrors the compute-kernel vtable shape (serial dispatch — one
/// command buffer + fence owned by the kernel, no `frame_index`
/// argument on any slot). The `bindings()` getter and
/// `set_push_constants_value::<T>` (generic) stay `host_inner`-routed
/// — `Vec<RayTracingBindingSpec>` isn't `#[repr(C)]` and the generic
/// reduces to `set_push_constants` for cdylib mode.
#[repr(C)]
pub struct VulkanRayTracingKernelMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Bind a top-level acceleration structure at `binding`. The
    /// slot must be declared as `AccelerationStructure` in the
    /// kernel's binding spec. `acceleration_structure_handle` is
    /// the raw `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`
    /// pointer the plugin handle carries. Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on declaration
    /// mismatch / null handle / wrong AS kind.
    pub set_acceleration_structure: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        acceleration_structure_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a [`PixelBuffer`](struct@crate)-shaped storage buffer
    /// (SSBO) at `binding`. `pixel_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<PixelBufferRef>)` pointer the plugin
    /// handle carries; the host wrapper reconstructs the borrow and
    /// forwards. Returns 0 on success; non-zero with UTF-8 message
    /// in `err_buf` on declaration mismatch / unset binding / null
    /// handle.
    pub set_storage_buffer_pixel: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        pixel_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a raw-bytes-shaped storage buffer (SSBO) at `binding`.
    /// `storage_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer the plugin
    /// handle carries.
    pub set_storage_buffer_storage: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        storage_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a uniform buffer (UBO) at `binding`.
    /// `uniform_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer the plugin
    /// handle carries.
    pub set_uniform_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        uniform_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a sampled texture at `binding` using the kernel's
    /// default linear-clamp sampler. `texture_handle` is the raw
    /// `Arc::into_raw(Arc<TextureInner>)` pointer the plugin
    /// handle carries.
    pub set_sampled_texture: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a storage image at `binding`. Caller guarantees the
    /// underlying texture's `STORAGE_BINDING` usage was declared at
    /// creation time.
    pub set_storage_image: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Upload push-constant bytes. `bytes_len` should match the
    /// kernel's declared `push_constants.size` (already cached on
    /// the plugin handle). Returns 0 on success; non-zero with UTF-8
    /// message in `err_buf` on failure.
    pub set_push_constants: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        bytes_ptr: *const u8,
        bytes_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Dispatch the kernel: write all staged descriptors, record
    /// bind + push + `cmd_trace_rays_khr`, submit, wait on the
    /// kernel's fence before returning. Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on failure (missing
    /// binding, unset push-constants, GPU submission error, fence
    /// wait timeout, etc.).
    pub trace_rays: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        width: u32,
        height: u32,
        depth: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanRayTracingKernelMethodsVTable {}
unsafe impl Sync for VulkanRayTracingKernelMethodsVTable {}

// =============================================================================
// VulkanAccelerationStructureMethodsVTable — per-type vtable for AS method dispatch
// =============================================================================

/// Per-type method-dispatch vtable for the
/// `VulkanAccelerationStructure` β-shape (issue #907 Phase E PR 5/5
/// + #955 method dispatch).
///
/// Mirrors the kernel methods-vtable shape. POD getters
/// (`device_address`, `kind`, `storage_size`) are populated at mint
/// time via the v8 [`GpuContextFullAccessVTable::build_triangles_blas`]
/// / `build_tlas` out-params and don't need vtable slots — the
/// cached values on the β-shape struct are always real, never
/// placeholder zeros.
///
/// The single vtable slot is `label`, which uses the same byte-
/// buffer out-param shape as `TextureRingSlot.surface_id` from #947.
/// `vk_handle` stays host-only — the
/// `vk::AccelerationStructureKHR` is a vulkanalia handle whose
/// layout couples to the vulkanalia minor version, and there is no
/// in-tree cdylib consumer that reads it (every binding into a
/// ray-tracing kernel goes through the host-side
/// `set_acceleration_structure` slot, which dereferences the AS on
/// the host side and reads `vk_handle` there).
#[repr(C)]
pub struct VulkanAccelerationStructureMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Read the AS's human-readable label into a caller-provided
    /// byte buffer. `out_buf` / `out_buf_cap` describe the buffer;
    /// `*out_len` receives the number of bytes written (≤ `out_buf_cap`
    /// — labels longer than the buffer are silently truncated, which
    /// is fine for diagnostic strings). Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on failure (null AS
    /// handle, null out-buffer pointer, etc.).
    pub label: unsafe extern "C" fn(
        as_handle: *const c_void,
        out_buf: *mut u8,
        out_buf_cap: usize,
        out_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanAccelerationStructureMethodsVTable {}
unsafe impl Sync for VulkanAccelerationStructureMethodsVTable {}

// =============================================================================
// SurfaceStoreVTable — extern "C" dispatch for cross-process surface sharing
// =============================================================================

/// Dispatch table for the host's `SurfaceStore`. The cdylib obtains a
/// handle via [`GpuContextLimitedAccessVTable::surface_store`] and
/// reads the static vtable from [`HostServices::surface_store_vtable`].
///
/// Lives in its own vtable (not folded into
/// [`GpuContextLimitedAccessVTable`]) for two reasons:
/// 1. **Surface-area discipline** — `SurfaceStore`'s public method
///    surface is large (~10 methods, mixing cross-platform and
///    Linux-only operations) and conceptually distinct from the GPU
///    capability surface. Folding it into the parent vtable would
///    nearly double `GpuContextLimitedAccessVTable`'s size without
///    adding semantic clarity.
/// 2. **Separate-vtable-per-subsystem precedent** — `AudioClockVTable`
///    already lives outside `RuntimeContextVTable` at the
///    `HostServices` level (via
///    [`HostServices::audio_clock_vtable`]); the same shape
///    applies here.
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror every other Arc-handle β-
/// reshape: `clone_handle(borrowed) -> owned` bumps the host's
/// `Arc<SurfaceStoreInner>` refcount; `drop_handle(owned)` releases.
/// The owned handle remains valid even after the originating
/// `RuntimeContext` is dropped — matches the existing
/// `SurfaceStore: Clone` contract.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0. New methods append to the
/// end and bump [`SURFACE_STORE_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct SurfaceStoreVTable {
    /// Vtable layout version. Must equal
    /// [`SURFACE_STORE_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps following pointers naturally aligned;
    /// zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Handle lifetime
    // -------------------------------------------------------------------------

    /// Bump the refcount on a `SurfaceStore` handle.
    /// `Arc::increment_strong_count(handle as *const SurfaceStoreInner)`.
    pub clone_handle: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `SurfaceStore` handle. When the
    /// strong count reaches zero the underlying connection / cache
    /// state is dropped.
    pub drop_handle: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // Cross-platform method dispatch
    // -------------------------------------------------------------------------

    /// Connect to the surface-share service (XPC on macOS, Unix
    /// socket on Linux). On success returns 0; on failure writes a
    /// UTF-8 error into `err_buf` and returns non-zero.
    pub connect: unsafe extern "C" fn(
        handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Disconnect from the surface-share service.
    pub disconnect: unsafe extern "C" fn(
        handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check in a pixel buffer for cross-process sharing. The
    /// returned `surface_id` is written into `out_id_buf` (capped at
    /// `out_id_cap`); the actual length is stored in `*out_id_len`.
    /// Truncation returns the required length without writing.
    pub check_in: unsafe extern "C" fn(
        handle: *const c_void,
        pixel_buffer: *const c_void,
        out_id_buf: *mut u8,
        out_id_cap: usize,
        out_id_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check out a surface by its `surface_id`. On success writes a
    /// `PixelBuffer` β-shape into `*out_pixel_buffer` and returns 0.
    pub check_out: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Register a pre-allocated buffer under the given pool id.
    pub register_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        pool_id_ptr: *const u8,
        pool_id_len: usize,
        pixel_buffer: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Look up a previously-registered buffer by its pool id. Writes
    /// a `PixelBuffer` β-shape into `*out_pixel_buffer` on success.
    pub lookup_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        pool_id_ptr: *const u8,
        pool_id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Release a checked-out surface by its `surface_id`. Idempotent.
    pub release: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Linux-only method dispatch (stub on other platforms)
    // -------------------------------------------------------------------------
    //
    // `register_texture` / `register_pixel_buffer_with_timeline` /
    // `lookup_texture` / `update_image_layout` are Linux-only on the
    // host side (they wrap DMA-BUF / OPAQUE_FD surface-share IPC).
    // Non-Linux hosts ship stubs that return non-zero with a clean
    // error message.

    /// Register a texture for cross-process sharing. `texture` is a
    /// `*const Texture` β-shape pointer; `timeline_handle` is an
    /// opaque `Arc<HostVulkanTimelineSemaphore>` pointer (null for
    /// "no timeline") — engine-only, cdylibs pass null. `layout_raw`
    /// is the i32 `VkImageLayout` enumerant.
    pub register_texture: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        texture: *const c_void,
        timeline_handle: *const c_void,
        layout_raw: i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Register a pixel buffer with an optional timeline-semaphore
    /// sidecar. Same `timeline_handle` shape as
    /// [`Self::register_texture`].
    pub register_pixel_buffer_with_timeline: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        pixel_buffer: *const c_void,
        timeline_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Look up a registered texture by `surface_id`. Writes a
    /// `Texture` β-shape into `*out_texture` and the producer's
    /// last-published `VkImageLayout` (raw i32) into `*out_layout_raw`.
    pub lookup_texture: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        out_texture: *mut c_void,
        out_layout_raw: *mut i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Update the published `VkImageLayout` for an already-registered
    /// texture. Linux-only on the host side.
    pub update_image_layout: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        layout_raw: i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for SurfaceStoreVTable {}
unsafe impl Sync for SurfaceStoreVTable {}

// =============================================================================
// HostServices — the callback table
// =============================================================================

/// Host-services payload the host hands to plugin cdylibs via the
/// `STREAMLIB_PLUGIN.register` callback.
///
/// **Pure ABI.** Every field is either a primitive or an
/// `unsafe extern "C" fn` pointer. No Rust types cross the
/// boundary. Stable under rustc minor-version drift and
/// transitive-dep drift, as long as both sides target the same
/// triple and link the same [`STREAMLIB_ABI_VERSION`].
///
/// # Layout discipline
///
/// `abi_layout_version` and `host` are pinned at offset 0 and offset
/// 8 forever; the cdylib reads `abi_layout_version` before
/// dereferencing any other field, so an older cdylib loaded into a
/// newer host can refuse to load cleanly when fields shift.
///
/// New fields go at the **end** and bump
/// [`HOST_SERVICES_LAYOUT_VERSION`]. Removing or reordering existing
/// fields requires bumping [`STREAMLIB_ABI_VERSION`].
#[repr(C)]
pub struct HostServices {
    /// Layout version. Must equal [`HOST_SERVICES_LAYOUT_VERSION`].
    pub abi_layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Opaque host state. Passed to every callback below.
    pub host: HostHandle,

    // -------------------------------------------------------------------------
    // Tracing — forwarder Subscriber callbacks (tracing-ext-ffi-subscriber shape)
    // -------------------------------------------------------------------------

    /// Register a callsite with the host's tracing pipeline. The
    /// host's `EnvFilter` computes interest from `(target, level)`
    /// and returns it; the cdylib caches the result per-callsite
    /// the same way tracing-core does locally.
    pub tracing_register_callsite: unsafe extern "C" fn(
        host: HostHandle,
        target_ptr: *const u8,
        target_len: usize,
        level: HostLogLevel,
    ) -> HostInterest,

    /// Per-event enable check. Called when [`HostInterest::Sometimes`]
    /// was returned by `tracing_register_callsite`. The host can
    /// short-circuit emit by returning `false`.
    pub tracing_enabled: unsafe extern "C" fn(
        host: HostHandle,
        target_ptr: *const u8,
        target_len: usize,
        level: HostLogLevel,
    ) -> bool,

    /// Emit an event. `message_ptr`/`len` is the formatted message
    /// (the `tracing::info!("{}", x)` body); `fields_msgpack_ptr`/`len`
    /// is a msgpack `map` of structured fields excluding `message`,
    /// empty when there are no fields beyond the message. The host
    /// deserializes the map into its `JsonlSinkLayer::Capture` shape.
    pub tracing_emit: unsafe extern "C" fn(
        host: HostHandle,
        target_ptr: *const u8,
        target_len: usize,
        level: HostLogLevel,
        message_ptr: *const u8,
        message_len: usize,
        fields_msgpack_ptr: *const u8,
        fields_msgpack_len: usize,
    ),

    // -------------------------------------------------------------------------
    // PUBSUB
    // -------------------------------------------------------------------------

    /// Publish a serialized `Event` to a topic. The event is encoded
    /// the same way `PubSub::publish` encodes today (msgpack-named
    /// via `rmp_serde::to_vec_named`), so host-side
    /// deserialization is identical regardless of caller DSO.
    ///
    /// Subscribe is intentionally absent: cdylib code does not
    /// currently subscribe; if a future plugin shape needs it, add a
    /// `pubsub_subscribe` callback paired with a cdylib-provided
    /// listener fn pointer and bump
    /// [`HOST_SERVICES_LAYOUT_VERSION`].
    pub pubsub_publish: unsafe extern "C" fn(
        host: HostHandle,
        topic_ptr: *const u8,
        topic_len: usize,
        event_msgpack_ptr: *const u8,
        event_msgpack_len: usize,
    ),

    // -------------------------------------------------------------------------
    // Schema registry
    // -------------------------------------------------------------------------

    /// Register a schema's YAML body under its canonical id. Last
    /// write wins (matches `register_schema` semantics).
    pub schema_register: unsafe extern "C" fn(
        host: HostHandle,
        canonical_id_ptr: *const u8,
        canonical_id_len: usize,
        yaml_ptr: *const u8,
        yaml_len: usize,
    ),

    /// Lookup a schema by canonical id. The host invokes
    /// `result_callback(result_userdata, yaml_ptr, yaml_len)` exactly
    /// once before returning; `yaml_ptr` is null + `yaml_len` is 0 on
    /// miss. The callback receives a borrow valid only for the
    /// duration of the call; cdylib code must copy if it needs to
    /// outlive the call.
    pub schema_lookup: unsafe extern "C" fn(
        host: HostHandle,
        canonical_id_ptr: *const u8,
        canonical_id_len: usize,
        result_callback: extern "C" fn(
            userdata: *mut c_void,
            yaml_ptr: *const u8,
            yaml_len: usize,
        ),
        result_userdata: *mut c_void,
    ),

    // -------------------------------------------------------------------------
    // iceoryx2-log
    // -------------------------------------------------------------------------

    /// Emit an iceoryx2 log record. Used by the cdylib's
    /// `iceoryx2_log_types::Log` forwarder; the host bridges to its
    /// own tracing pipeline.
    pub iceoryx_log_emit: unsafe extern "C" fn(
        host: HostHandle,
        level: HostLogLevel,
        origin_ptr: *const u8,
        origin_len: usize,
        message_ptr: *const u8,
        message_len: usize,
    ),

    // -------------------------------------------------------------------------
    // Processor registration (v2 — replaces the v1 typed pointer)
    // -------------------------------------------------------------------------

    /// Register a processor type with the host's registry. The
    /// `descriptor_msgpack` bytes encode a `ProcessorDescriptor`
    /// (using `streamlib-processor-schema`'s serde derives) — the
    /// host decodes them and stores the descriptor + vtable +
    /// constructor.
    ///
    /// `vtable` is a `&'static ProcessorVTable` on the cdylib side;
    /// the host pins the loaded library forever via
    /// `LOADED_PLUGIN_LIBRARIES`, so the pointer outlives the host's
    /// usage.
    ///
    /// Returns `0` on success. Non-zero indicates the descriptor
    /// was malformed, the vtable layout version mismatched, or the
    /// processor type was already registered; the cdylib's macro
    /// expansion treats failures as silent (the host's "processor
    /// not registered" check surfaces the error to the user).
    pub processor_register: unsafe extern "C" fn(
        host: HostHandle,
        descriptor_msgpack_ptr: *const u8,
        descriptor_msgpack_len: usize,
        vtable: *const ProcessorVTable,
    ) -> i32,

    // -------------------------------------------------------------------------
    // RuntimeContext vtable surface (v3 — eliminates the tokio shared crossing)
    // -------------------------------------------------------------------------

    /// Static dispatch table the cdylib's
    /// `RuntimeContext{Full,Limited}Access` shim uses to read host-
    /// owned context state. Set once at install time; never null
    /// for v3+ HostServices payloads. See [`RuntimeContextVTable`].
    pub runtime_context_vtable: *const RuntimeContextVTable,

    /// Static dispatch table for the host's `SharedAudioClock`.
    /// Paired with the per-instance handle returned by
    /// [`RuntimeContextVTable::audio_clock_handle`]. Set once at
    /// install time; non-null for hosts that ship an audio clock,
    /// null otherwise (cdylib must check before dispatching).
    pub audio_clock_vtable: *const AudioClockVTable,

    /// Static dispatch table for the host's `RuntimeOperations`.
    /// Paired with the per-instance handle returned by
    /// [`RuntimeContextVTable::runtime_ops_handle`]. Set once at
    /// install time; never null for v3+ HostServices payloads.
    pub runtime_ops_vtable: *const RuntimeOpsVTable,

    // -------------------------------------------------------------------------
    // GpuContext vtable surface
    // -------------------------------------------------------------------------

    /// Static dispatch table for the host's `GpuContextLimitedAccess`.
    /// Paired with the per-instance handle returned by
    /// [`RuntimeContextVTable::gpu_limited_access`]. Set once at
    /// install time; non-null for hosts that ship a GpuContext,
    /// null otherwise (cdylib must check before dispatching).
    pub gpu_context_limited_access_vtable: *const GpuContextLimitedAccessVTable,

    // -------------------------------------------------------------------------
    // SurfaceStore vtable surface
    // -------------------------------------------------------------------------

    /// Static dispatch table for the host's `SurfaceStore`. Paired
    /// with the per-`SurfaceStore` handle returned by
    /// [`GpuContextLimitedAccessVTable::surface_store`]. Set once at
    /// install time; non-null for hosts that ship a `SurfaceStore`,
    /// null otherwise (cdylib must check before dispatching).
    pub surface_store_vtable: *const SurfaceStoreVTable,

    // -------------------------------------------------------------------------
    // GpuContextFullAccess vtable surface (v6 — Phase C2)
    // -------------------------------------------------------------------------

    /// Static dispatch table for the host's `GpuContextFullAccess`.
    /// Paired with the per-scope opaque handle the C3 `escalate_begin`
    /// callback returns. Set once at install time; non-null for hosts
    /// that ship a GpuContext, null otherwise (cdylib must check
    /// before dispatching). Phase C2 lands the layout + host wiring +
    /// cdylib β-shape; Phase C3 wires the scope-token machinery that
    /// makes the methods reachable from `escalate(|full| ...)` call
    /// sites.
    pub gpu_context_full_access_vtable: *const GpuContextFullAccessVTable,

    // -------------------------------------------------------------------------
    // TextureRingMethodsVTable surface (v7 — issue #907 Phase E PR 1/5)
    // -------------------------------------------------------------------------

    /// Static dispatch table for `TextureRing` β-shape method
    /// dispatch. Paired with the per-`TextureRing` handle the
    /// cdylib carries on its β-shape struct (`methods_vtable`
    /// field). Set once at install time; non-null for hosts that
    /// ship a GpuContext, null otherwise (cdylib must check before
    /// dispatching). PR 1 of issue #907 lands the empty-shell
    /// vtable + pointer plumbing; follow-up PRs append the actual
    /// method slots.
    pub texture_ring_methods_vtable: *const TextureRingMethodsVTable,

    // -------------------------------------------------------------------------
    // VulkanComputeKernelMethodsVTable surface (v8 — issue #907 Phase E PR 2/5)
    // -------------------------------------------------------------------------

    /// Static dispatch table for `VulkanComputeKernel` β-shape
    /// method dispatch. Paired with the per-`VulkanComputeKernel`
    /// handle the cdylib carries on its β-shape struct
    /// (`methods_vtable` field). Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise (cdylib
    /// must check before dispatching). PR 2 of issue #907 lands the
    /// empty-shell vtable + pointer plumbing; follow-up PRs append
    /// the actual method slots.
    pub vulkan_compute_kernel_methods_vtable: *const VulkanComputeKernelMethodsVTable,

    // -------------------------------------------------------------------------
    // VulkanGraphicsKernelMethodsVTable surface (v9 — issue #907 Phase E PR 3/5)
    // -------------------------------------------------------------------------

    /// Static dispatch table for `VulkanGraphicsKernel` β-shape
    /// method dispatch. Paired with the per-`VulkanGraphicsKernel`
    /// handle the cdylib carries on its β-shape struct
    /// (`methods_vtable` field). Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise (cdylib
    /// must check before dispatching). PR 3 of issue #907 lands the
    /// empty-shell vtable + pointer plumbing; follow-up PRs append
    /// the actual method slots.
    pub vulkan_graphics_kernel_methods_vtable: *const VulkanGraphicsKernelMethodsVTable,

    // -------------------------------------------------------------------------
    // VulkanRayTracingKernelMethodsVTable surface (v10 — issue #907 Phase E PR 4/5)
    // -------------------------------------------------------------------------

    /// Static dispatch table for `VulkanRayTracingKernel` β-shape
    /// method dispatch. Paired with the per-`VulkanRayTracingKernel`
    /// handle the cdylib carries on its β-shape struct
    /// (`methods_vtable` field). Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise (cdylib
    /// must check before dispatching). PR 4 of issue #907 lands the
    /// empty-shell vtable + pointer plumbing; follow-up PRs append
    /// the actual method slots.
    pub vulkan_ray_tracing_kernel_methods_vtable:
        *const VulkanRayTracingKernelMethodsVTable,

    // -------------------------------------------------------------------------
    // VulkanAccelerationStructureMethodsVTable surface (v11 — issue #907 Phase E PR 5/5)
    // -------------------------------------------------------------------------

    /// Static dispatch table for `VulkanAccelerationStructure`
    /// β-shape method dispatch. Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise. PR 5 of
    /// issue #907 lands the empty-shell vtable + pointer plumbing;
    /// follow-up PRs append the actual method slots.
    pub vulkan_acceleration_structure_methods_vtable:
        *const VulkanAccelerationStructureMethodsVTable,

    // -------------------------------------------------------------------------
    // RhiColorConverterMethodsVTable surface (v12 — Phase E sub-lift slice A)
    // -------------------------------------------------------------------------

    /// Static dispatch table for `RhiColorConverter` β-shape method
    /// dispatch. Paired with the per-`RhiColorConverter` handle the
    /// cdylib carries on its β-shape struct (`methods_vtable` field).
    /// Set once at install time; non-null for hosts that ship a
    /// GpuContext, null otherwise (cdylib must check before
    /// dispatching). Phase E sub-lift slice A lands the
    /// `prepare_buffer_to_image_storage` slot so cdylib camera
    /// processors can prepare color-conversion kernels without
    /// tripping the β-shape's host-mode-only `host_inner()` panic.
    pub rhi_color_converter_methods_vtable:
        *const RhiColorConverterMethodsVTable,

    // -------------------------------------------------------------------------
    // RhiCommandRecorderMethodsVTable surface (v13 — Phase E sub-lift slice B)
    // -------------------------------------------------------------------------

    /// Static dispatch table for `RhiCommandRecorder` β-shape
    /// method dispatch. Paired with the per-`RhiCommandRecorder`
    /// handle the cdylib carries on its β-shape struct
    /// (`methods_vtable` field). Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise (cdylib
    /// must check before dispatching). Phase E sub-lift slice B
    /// lands six camera-hot-path slots (`begin`,
    /// `record_image_barrier`, `record_buffer_barrier`,
    /// `record_dispatch`, `record_copy_image_to_buffer`,
    /// `submit_signaling_timeline`) so cdylib camera processors
    /// can drive the host-owned recorder per frame without
    /// tripping the β-shape's host-mode-only `host_inner_mut()`
    /// panic.
    pub rhi_command_recorder_methods_vtable:
        *const RhiCommandRecorderMethodsVTable,

    // -------------------------------------------------------------------------
    // OutputWriterVTable + InputMailboxesVTable references (v14 — issue #894)
    // -------------------------------------------------------------------------

    /// Static dispatch table for the cdylib's `OutputWriter` β-shape
    /// method dispatch. Paired with the per-instance opaque handle
    /// the cdylib stores on its `outputs` field after the host
    /// invokes `ProcessorVTable::set_iceoryx2_resources`. Non-null
    /// for every host that wires processors with output ports;
    /// hosts that strictly don't ship the iceoryx2 transport can
    /// leave it null and the cdylib will treat
    /// `set_iceoryx2_resources` as a no-op for outputs.
    pub output_writer_vtable: *const OutputWriterVTable,

    /// Static dispatch table for the cdylib's `InputMailboxes`
    /// β-shape method dispatch. Paired with the per-instance opaque
    /// handle the cdylib stores on its `inputs` field after the host
    /// invokes `ProcessorVTable::set_iceoryx2_resources`. Non-null
    /// for every host that wires processors with input ports.
    pub input_mailboxes_vtable: *const InputMailboxesVTable,
}

// Note: under v3 the ABI eliminates the tokio shared-type crossing
// entirely. Plugins own their own tokio runtimes (or whatever async
// runtime they prefer); the host's runtime is not exposed and is
// never required to match the plugin's. Lifecycle methods are
// synchronous at the trait surface; the host's lifecycle wrappers
// no longer wrap user code in `block_on`. Plugins that want async
// in lifecycle methods do their own `block_on` internally.

// Safety: every field is a raw pointer, a fn pointer, or a
// primitive. The host guarantees the pointed-at state outlives the
// cdylib's process lifetime via the `LOADED_PLUGIN_LIBRARIES`
// pinning shape (the engine's loader keeps the `Library` handle
// alive forever).
unsafe impl Send for HostServices {}
unsafe impl Sync for HostServices {}

// =============================================================================
// PluginDeclaration — the wire envelope
// =============================================================================

/// Plugin register function signature.
///
/// The host passes a pointer to its [`HostServices`] payload. The
/// cdylib's macro expansion forwards the pointer into
/// `streamlib::sdk::plugin::install_host_services`, which validates
/// the layout, installs forwarders for every process-wide static,
/// and registers the plugin's processor types with the host's
/// registry.
///
/// # Safety
///
/// `host_services` must point at a valid [`HostServices`] payload
/// owned by the host. The host guarantees the pointer outlives the
/// cdylib's process lifetime.
pub type PluginRegisterFn = unsafe extern "C" fn(host_services: *const c_void);

// =============================================================================
// Layout regression tests
// =============================================================================
//
// These tests pin the byte-level shape of every type that crosses the
// cdylib boundary. A failure here means the layout drifted in a way
// that would silently corrupt cross-DSO dispatch. Bump the matching
// `*_LAYOUT_VERSION` constant when an intentional change lands and
// update the expected sizes/offsets here in the same commit.
//
// The expected sizes are 64-bit-pointer-target-specific. On a 32-bit
// target the pointer/fn-pointer sizes shrink and the tests need
// `#[cfg(target_pointer_width = "64")]` (left out today — every
// supported triple is 64-bit).
#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn audio_tick_context_repr_layout() {
        // 5 fields: i64 + u64 + u32 + u32 + u64 = 8+8+4+4+8 = 32 bytes
        // with 8-byte alignment from the i64/u64.
        assert_eq!(size_of::<AudioTickContextRepr>(), 32);
        assert_eq!(align_of::<AudioTickContextRepr>(), 8);
        assert_eq!(offset_of!(AudioTickContextRepr, timestamp_ns), 0);
        assert_eq!(offset_of!(AudioTickContextRepr, samples_needed), 8);
        assert_eq!(offset_of!(AudioTickContextRepr, sample_rate), 16);
        assert_eq!(offset_of!(AudioTickContextRepr, _reserved_padding), 20);
        assert_eq!(offset_of!(AudioTickContextRepr, tick_number), 24);
    }

    #[test]
    fn processor_vtable_layout() {
        // v2 (issue #894): the two shared-Rust-type slots
        // `get_iceoryx2_output_writer_arc` and
        // `get_iceoryx2_input_mailboxes_mut` are replaced by a single
        // `set_iceoryx2_resources` slot. 17 - 2 + 1 = 16 fn pointers.
        // header (u32 + u32) + 16 fn pointers @ 8 bytes each =
        // 4 + 4 + 16 * 8 = 136 bytes.
        assert_eq!(size_of::<ProcessorVTable>(), 136);
        assert_eq!(align_of::<ProcessorVTable>(), 8);
        assert_eq!(offset_of!(ProcessorVTable, layout_version), 0);
        assert_eq!(offset_of!(ProcessorVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(ProcessorVTable, construct), 8);
        assert_eq!(offset_of!(ProcessorVTable, destroy), 16);
        assert_eq!(offset_of!(ProcessorVTable, setup), 24);
        assert_eq!(offset_of!(ProcessorVTable, teardown), 32);
        assert_eq!(offset_of!(ProcessorVTable, on_pause), 40);
        assert_eq!(offset_of!(ProcessorVTable, on_resume), 48);
        assert_eq!(offset_of!(ProcessorVTable, process), 56);
        assert_eq!(offset_of!(ProcessorVTable, start), 64);
        assert_eq!(offset_of!(ProcessorVTable, stop), 72);
        assert_eq!(offset_of!(ProcessorVTable, execution_config_msgpack), 80);
        assert_eq!(offset_of!(ProcessorVTable, has_iceoryx2_outputs), 88);
        assert_eq!(offset_of!(ProcessorVTable, has_iceoryx2_inputs), 96);
        assert_eq!(offset_of!(ProcessorVTable, set_iceoryx2_resources), 104);
        assert_eq!(offset_of!(ProcessorVTable, apply_config_msgpack), 112);
        assert_eq!(offset_of!(ProcessorVTable, to_runtime_msgpack), 120);
        assert_eq!(offset_of!(ProcessorVTable, config_msgpack), 128);
    }

    #[test]
    fn output_writer_vtable_layout() {
        // header (u32 + u32) + 4 fn pointers @ 8 bytes each =
        // 4 + 4 + 4 * 8 = 40 bytes.
        assert_eq!(size_of::<OutputWriterVTable>(), 40);
        assert_eq!(align_of::<OutputWriterVTable>(), 8);
        assert_eq!(offset_of!(OutputWriterVTable, layout_version), 0);
        assert_eq!(offset_of!(OutputWriterVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(OutputWriterVTable, write_raw), 8);
        assert_eq!(offset_of!(OutputWriterVTable, has_port), 16);
        assert_eq!(offset_of!(OutputWriterVTable, clone_arc), 24);
        assert_eq!(offset_of!(OutputWriterVTable, drop_arc), 32);
    }

    #[test]
    fn input_mailboxes_vtable_layout() {
        // header (u32 + u32) + 2 fn pointers @ 8 bytes each =
        // 4 + 4 + 2 * 8 = 24 bytes.
        assert_eq!(size_of::<InputMailboxesVTable>(), 24);
        assert_eq!(align_of::<InputMailboxesVTable>(), 8);
        assert_eq!(offset_of!(InputMailboxesVTable, layout_version), 0);
        assert_eq!(offset_of!(InputMailboxesVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(InputMailboxesVTable, read_raw), 8);
        assert_eq!(offset_of!(InputMailboxesVTable, has_data), 16);
    }

    #[test]
    fn plugin_declaration_layout() {
        // u32 + 4-byte padding + 8-byte fn pointer = 16 bytes.
        assert_eq!(size_of::<PluginDeclaration>(), 16);
        assert_eq!(align_of::<PluginDeclaration>(), 8);
        assert_eq!(offset_of!(PluginDeclaration, abi_version), 0);
        assert_eq!(offset_of!(PluginDeclaration, register), 8);
    }

    #[test]
    fn runtime_context_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 8 fn pointers (8 bytes each)
        // = 4 + 4 + 8*8 = 72 bytes
        assert_eq!(size_of::<RuntimeContextVTable>(), 72);
        assert_eq!(align_of::<RuntimeContextVTable>(), 8);
        assert_eq!(offset_of!(RuntimeContextVTable, layout_version), 0);
        assert_eq!(offset_of!(RuntimeContextVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(RuntimeContextVTable, runtime_id_copy), 8);
        assert_eq!(offset_of!(RuntimeContextVTable, processor_id_copy), 16);
        assert_eq!(offset_of!(RuntimeContextVTable, is_paused), 24);
        assert_eq!(offset_of!(RuntimeContextVTable, should_process), 32);
        assert_eq!(offset_of!(RuntimeContextVTable, gpu_full_access), 40);
        assert_eq!(offset_of!(RuntimeContextVTable, gpu_limited_access), 48);
        assert_eq!(offset_of!(RuntimeContextVTable, audio_clock_handle), 56);
        assert_eq!(offset_of!(RuntimeContextVTable, runtime_ops_handle), 64);
    }

    #[test]
    fn audio_clock_vtable_layout() {
        // 4 + 4 + 3 fn pointers = 32 bytes
        assert_eq!(size_of::<AudioClockVTable>(), 32);
        assert_eq!(align_of::<AudioClockVTable>(), 8);
        assert_eq!(offset_of!(AudioClockVTable, layout_version), 0);
        assert_eq!(offset_of!(AudioClockVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(AudioClockVTable, sample_rate), 8);
        assert_eq!(offset_of!(AudioClockVTable, buffer_size), 16);
        assert_eq!(offset_of!(AudioClockVTable, on_tick), 24);
    }

    #[test]
    fn runtime_ops_vtable_layout() {
        // 4 + 4 + 7 fn pointers (v2: 5 submit ops + clone_handle + drop_handle) = 64 bytes
        assert_eq!(size_of::<RuntimeOpsVTable>(), 64);
        assert_eq!(align_of::<RuntimeOpsVTable>(), 8);
        assert_eq!(offset_of!(RuntimeOpsVTable, layout_version), 0);
        assert_eq!(offset_of!(RuntimeOpsVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(RuntimeOpsVTable, add_processor), 8);
        assert_eq!(offset_of!(RuntimeOpsVTable, remove_processor), 16);
        assert_eq!(offset_of!(RuntimeOpsVTable, connect), 24);
        assert_eq!(offset_of!(RuntimeOpsVTable, disconnect), 32);
        assert_eq!(offset_of!(RuntimeOpsVTable, to_json), 40);
        assert_eq!(offset_of!(RuntimeOpsVTable, clone_handle), 48);
        assert_eq!(offset_of!(RuntimeOpsVTable, drop_handle), 56);
    }

    #[test]
    fn host_services_layout_versions_pinned() {
        // v14: issue #894 appends OutputWriterVTable +
        // InputMailboxesVTable references and bumps
        // ProcessorVTable to v2 (slot swap).
        assert_eq!(HOST_SERVICES_LAYOUT_VERSION, 14);
        assert_eq!(STREAMLIB_ABI_VERSION, 4);
        // v2: shared-Rust-type iceoryx2 slots replaced by
        // `set_iceoryx2_resources` (issue #894).
        assert_eq!(PROCESSOR_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, 1);
        // v2: added owning-Arc handle lifetime callbacks
        // (`clone_handle` / `drop_handle`).
        assert_eq!(RUNTIME_OPS_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION, 13);
        assert_eq!(SURFACE_STORE_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION, 8);
        assert_eq!(TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION, 3);
        assert_eq!(VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION, 1);
        // v2: appended PixelBuffer-flavored sibling slots
        // (`record_pixel_buffer_barrier`,
        // `record_copy_image_to_pixel_buffer`) for cdylib camera
        // per-frame copy into pooled `PixelBuffer` destinations.
        assert_eq!(RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION, 2);
        // v1 (issue #894): initial shape — `write_raw`, `has_port`,
        // `clone_arc`, `drop_arc`.
        assert_eq!(OUTPUT_WRITER_VTABLE_LAYOUT_VERSION, 1);
        // v1 (issue #894): initial shape — `read_raw`, `has_data`.
        assert_eq!(INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION, 1);
    }

    #[test]
    fn host_services_tail_carries_fifteen_vtable_pointers() {
        // Trailing vtable pointers on HostServices. We don't pin the
        // absolute offsets (earlier fields carry their own layout
        // audit), but we do pin:
        //   1. Each vtable is a single 8-byte pointer.
        //   2. They appear in the order RuntimeContext → AudioClock →
        //      RuntimeOps → GpuContextLimitedAccess → SurfaceStore →
        //      GpuContextFullAccess → TextureRingMethods →
        //      VulkanComputeKernelMethods → VulkanGraphicsKernelMethods →
        //      VulkanRayTracingKernelMethods → VulkanAccelerationStructureMethods →
        //      RhiColorConverterMethods → RhiCommandRecorderMethods →
        //      OutputWriterMethods → InputMailboxesMethods.
        //   3. They are contiguous (no padding inserted between them).
        assert_eq!(size_of::<*const RuntimeContextVTable>(), 8);
        assert_eq!(size_of::<*const AudioClockVTable>(), 8);
        assert_eq!(size_of::<*const RuntimeOpsVTable>(), 8);
        assert_eq!(size_of::<*const GpuContextLimitedAccessVTable>(), 8);
        assert_eq!(size_of::<*const SurfaceStoreVTable>(), 8);
        assert_eq!(size_of::<*const GpuContextFullAccessVTable>(), 8);
        assert_eq!(size_of::<*const TextureRingMethodsVTable>(), 8);
        assert_eq!(size_of::<*const VulkanComputeKernelMethodsVTable>(), 8);
        assert_eq!(size_of::<*const VulkanGraphicsKernelMethodsVTable>(), 8);
        assert_eq!(size_of::<*const VulkanRayTracingKernelMethodsVTable>(), 8);
        assert_eq!(
            size_of::<*const VulkanAccelerationStructureMethodsVTable>(),
            8
        );
        assert_eq!(size_of::<*const RhiColorConverterMethodsVTable>(), 8);
        assert_eq!(size_of::<*const RhiCommandRecorderMethodsVTable>(), 8);
        assert_eq!(size_of::<*const OutputWriterVTable>(), 8);
        assert_eq!(size_of::<*const InputMailboxesVTable>(), 8);

        let runtime_ctx_off = offset_of!(HostServices, runtime_context_vtable);
        let audio_clock_off = offset_of!(HostServices, audio_clock_vtable);
        let runtime_ops_off = offset_of!(HostServices, runtime_ops_vtable);
        let gpu_lim_off = offset_of!(HostServices, gpu_context_limited_access_vtable);
        let surface_store_off = offset_of!(HostServices, surface_store_vtable);
        let gpu_full_off = offset_of!(HostServices, gpu_context_full_access_vtable);
        let texture_ring_off = offset_of!(HostServices, texture_ring_methods_vtable);
        let compute_kernel_off =
            offset_of!(HostServices, vulkan_compute_kernel_methods_vtable);
        let graphics_kernel_off =
            offset_of!(HostServices, vulkan_graphics_kernel_methods_vtable);
        let rt_kernel_off =
            offset_of!(HostServices, vulkan_ray_tracing_kernel_methods_vtable);
        let accel_struct_off =
            offset_of!(HostServices, vulkan_acceleration_structure_methods_vtable);
        let color_converter_off =
            offset_of!(HostServices, rhi_color_converter_methods_vtable);
        let command_recorder_off =
            offset_of!(HostServices, rhi_command_recorder_methods_vtable);
        let output_writer_off = offset_of!(HostServices, output_writer_vtable);
        let input_mailboxes_off = offset_of!(HostServices, input_mailboxes_vtable);
        assert!(runtime_ctx_off < audio_clock_off);
        assert!(audio_clock_off < runtime_ops_off);
        assert!(runtime_ops_off < gpu_lim_off);
        assert!(gpu_lim_off < surface_store_off);
        assert!(surface_store_off < gpu_full_off);
        assert!(gpu_full_off < texture_ring_off);
        assert!(texture_ring_off < compute_kernel_off);
        assert!(compute_kernel_off < graphics_kernel_off);
        assert!(graphics_kernel_off < rt_kernel_off);
        assert!(rt_kernel_off < accel_struct_off);
        assert!(accel_struct_off < color_converter_off);
        assert!(color_converter_off < command_recorder_off);
        assert!(command_recorder_off < output_writer_off);
        assert!(output_writer_off < input_mailboxes_off);
        assert_eq!(audio_clock_off - runtime_ctx_off, 8);
        assert_eq!(runtime_ops_off - audio_clock_off, 8);
        assert_eq!(gpu_lim_off - runtime_ops_off, 8);
        assert_eq!(surface_store_off - gpu_lim_off, 8);
        assert_eq!(gpu_full_off - surface_store_off, 8);
        assert_eq!(texture_ring_off - gpu_full_off, 8);
        assert_eq!(compute_kernel_off - texture_ring_off, 8);
        assert_eq!(graphics_kernel_off - compute_kernel_off, 8);
        assert_eq!(rt_kernel_off - graphics_kernel_off, 8);
        assert_eq!(accel_struct_off - rt_kernel_off, 8);
        assert_eq!(color_converter_off - accel_struct_off, 8);
        assert_eq!(command_recorder_off - color_converter_off, 8);
        assert_eq!(output_writer_off - command_recorder_off, 8);
        assert_eq!(input_mailboxes_off - output_writer_off, 8);

        // The InputMailboxes pointer must end at the end of the
        // struct (it is the last field added in v14, issue #894).
        assert_eq!(input_mailboxes_off + 8, size_of::<HostServices>());
    }

    #[test]
    fn texture_ring_methods_vtable_layout() {
        // v2 (slot β-shape + method-dispatch slots added):
        //   layout_version              @ 0   (4 bytes, u32)
        //   _reserved_padding           @ 4   (4 bytes, u32)
        //   acquire_next                @ 8   (8 bytes, fn pointer)
        //   copy_pixel_buffer_to_slot   @ 16
        //   slot                        @ 24
        // Total = 32 bytes, align = 8.
        assert_eq!(size_of::<TextureRingMethodsVTable>(), 32);
        assert_eq!(align_of::<TextureRingMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(TextureRingMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(TextureRingMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(offset_of!(TextureRingMethodsVTable, acquire_next), 8);
        assert_eq!(
            offset_of!(TextureRingMethodsVTable, copy_pixel_buffer_to_slot),
            16
        );
        assert_eq!(offset_of!(TextureRingMethodsVTable, slot), 24);
    }

    #[test]
    fn vulkan_compute_kernel_methods_vtable_layout() {
        // v3 (typed binding-method slots added):
        //   layout_version              @ 0   (4 bytes, u32)
        //   _reserved_padding           @ 4   (4 bytes, u32)
        //   set_push_constants          @ 8   (8 bytes, fn pointer)
        //   dispatch                    @ 16
        //   set_storage_buffer_pixel    @ 24
        //   set_storage_buffer_storage  @ 32
        //   set_uniform_buffer          @ 40
        //   set_sampled_texture         @ 48
        //   set_storage_image           @ 56
        // Total = 64 bytes, align = 8.
        assert_eq!(size_of::<VulkanComputeKernelMethodsVTable>(), 64);
        assert_eq!(align_of::<VulkanComputeKernelMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_push_constants),
            8
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, dispatch),
            16
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_storage_buffer_pixel),
            24
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_storage_buffer_storage),
            32
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_uniform_buffer),
            40
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_sampled_texture),
            48
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_storage_image),
            56
        );
    }

    #[test]
    fn vulkan_graphics_kernel_methods_vtable_layout() {
        // v2 (typed binding-method slots + offscreen_render added):
        //   layout_version              @ 0   (4 bytes, u32)
        //   _reserved_padding           @ 4   (4 bytes, u32)
        //   set_storage_buffer_pixel    @ 8   (8 bytes, fn pointer)
        //   set_storage_buffer_storage  @ 16
        //   set_uniform_buffer          @ 24
        //   set_sampled_texture         @ 32
        //   set_storage_image           @ 40
        //   set_vertex_buffer           @ 48
        //   set_index_buffer            @ 56
        //   set_push_constants          @ 64
        //   offscreen_render            @ 72
        // Total = 80 bytes, align = 8.
        assert_eq!(size_of::<VulkanGraphicsKernelMethodsVTable>(), 80);
        assert_eq!(align_of::<VulkanGraphicsKernelMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_storage_buffer_pixel),
            8
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_storage_buffer_storage),
            16
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_uniform_buffer),
            24
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_sampled_texture),
            32
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_storage_image),
            40
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_vertex_buffer),
            48
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_index_buffer),
            56
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_push_constants),
            64
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, offscreen_render),
            72
        );
    }

    #[test]
    fn viewport_repr_layout() {
        // 6 f32 fields, tightly packed, align 4.
        assert_eq!(size_of::<ViewportRepr>(), 24);
        assert_eq!(align_of::<ViewportRepr>(), 4);
        assert_eq!(offset_of!(ViewportRepr, x), 0);
        assert_eq!(offset_of!(ViewportRepr, y), 4);
        assert_eq!(offset_of!(ViewportRepr, width), 8);
        assert_eq!(offset_of!(ViewportRepr, height), 12);
        assert_eq!(offset_of!(ViewportRepr, min_depth), 16);
        assert_eq!(offset_of!(ViewportRepr, max_depth), 20);
    }

    #[test]
    fn scissor_rect_repr_layout() {
        // i32 + i32 + u32 + u32 = 16 bytes, align 4.
        assert_eq!(size_of::<ScissorRectRepr>(), 16);
        assert_eq!(align_of::<ScissorRectRepr>(), 4);
        assert_eq!(offset_of!(ScissorRectRepr, x), 0);
        assert_eq!(offset_of!(ScissorRectRepr, y), 4);
        assert_eq!(offset_of!(ScissorRectRepr, width), 8);
        assert_eq!(offset_of!(ScissorRectRepr, height), 12);
    }

    #[test]
    fn draw_call_repr_layout() {
        // 4 u32 + 2 u32 (present flags) = 24 bytes header,
        // ViewportRepr (24) + ScissorRectRepr (16) = 40 bytes payload,
        // total = 64 bytes, align 4 (all sub-fields ≤4-byte aligned).
        assert_eq!(size_of::<DrawCallRepr>(), 64);
        assert_eq!(align_of::<DrawCallRepr>(), 4);
        assert_eq!(offset_of!(DrawCallRepr, vertex_count), 0);
        assert_eq!(offset_of!(DrawCallRepr, instance_count), 4);
        assert_eq!(offset_of!(DrawCallRepr, first_vertex), 8);
        assert_eq!(offset_of!(DrawCallRepr, first_instance), 12);
        assert_eq!(offset_of!(DrawCallRepr, viewport_present), 16);
        assert_eq!(offset_of!(DrawCallRepr, scissor_present), 20);
        assert_eq!(offset_of!(DrawCallRepr, viewport), 24);
        assert_eq!(offset_of!(DrawCallRepr, scissor), 48);
    }

    #[test]
    fn draw_indexed_call_repr_layout() {
        // 5 u32 + 2 u32 (present) + 1 u32 (padding) = 32 bytes header,
        // ViewportRepr (24) + ScissorRectRepr (16) = 40 bytes payload,
        // total = 72 bytes, align 4.
        assert_eq!(size_of::<DrawIndexedCallRepr>(), 72);
        assert_eq!(align_of::<DrawIndexedCallRepr>(), 4);
        assert_eq!(offset_of!(DrawIndexedCallRepr, index_count), 0);
        assert_eq!(offset_of!(DrawIndexedCallRepr, instance_count), 4);
        assert_eq!(offset_of!(DrawIndexedCallRepr, first_index), 8);
        assert_eq!(offset_of!(DrawIndexedCallRepr, vertex_offset), 12);
        assert_eq!(offset_of!(DrawIndexedCallRepr, first_instance), 16);
        assert_eq!(offset_of!(DrawIndexedCallRepr, viewport_present), 20);
        assert_eq!(offset_of!(DrawIndexedCallRepr, scissor_present), 24);
        assert_eq!(offset_of!(DrawIndexedCallRepr, _reserved_padding), 28);
        assert_eq!(offset_of!(DrawIndexedCallRepr, viewport), 32);
        assert_eq!(offset_of!(DrawIndexedCallRepr, scissor), 56);
    }

    #[test]
    fn offscreen_draw_repr_layout() {
        // kind (u32) + _reserved_padding (u32) = 8 bytes header,
        // DrawCallRepr (64) + DrawIndexedCallRepr (72) = 136 bytes
        // payload, total = 144 bytes, align 4.
        assert_eq!(size_of::<OffscreenDrawRepr>(), 144);
        assert_eq!(align_of::<OffscreenDrawRepr>(), 4);
        assert_eq!(offset_of!(OffscreenDrawRepr, kind), 0);
        assert_eq!(offset_of!(OffscreenDrawRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(OffscreenDrawRepr, draw_call), 8);
        assert_eq!(offset_of!(OffscreenDrawRepr, draw_indexed_call), 72);
    }

    #[test]
    fn vulkan_ray_tracing_kernel_methods_vtable_layout() {
        // v2 (typed binding-method slots + set_push_constants +
        // trace_rays added):
        //   layout_version              @ 0   (4 bytes, u32)
        //   _reserved_padding           @ 4   (4 bytes, u32)
        //   set_acceleration_structure  @ 8   (8 bytes, fn pointer)
        //   set_storage_buffer_pixel    @ 16
        //   set_storage_buffer_storage  @ 24
        //   set_uniform_buffer          @ 32
        //   set_sampled_texture         @ 40
        //   set_storage_image           @ 48
        //   set_push_constants          @ 56
        //   trace_rays                  @ 64
        // Total = 72 bytes, align = 8.
        assert_eq!(size_of::<VulkanRayTracingKernelMethodsVTable>(), 72);
        assert_eq!(align_of::<VulkanRayTracingKernelMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_acceleration_structure),
            8
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_storage_buffer_pixel),
            16
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_storage_buffer_storage),
            24
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_uniform_buffer),
            32
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_sampled_texture),
            40
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_storage_image),
            48
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_push_constants),
            56
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, trace_rays),
            64
        );
    }

    #[test]
    fn vulkan_acceleration_structure_methods_vtable_layout() {
        // v2 (`label` slot added — #955):
        //   layout_version       @ 0 (4 bytes, u32)
        //   _reserved_padding    @ 4 (4 bytes, u32)
        //   label                @ 8 (8 bytes, fn pointer)
        // Total = 16 bytes, align = 8.
        assert_eq!(size_of::<VulkanAccelerationStructureMethodsVTable>(), 16);
        assert_eq!(align_of::<VulkanAccelerationStructureMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanAccelerationStructureMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanAccelerationStructureMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(VulkanAccelerationStructureMethodsVTable, label),
            8
        );
    }

    #[test]
    fn source_layout_info_repr_layout() {
        // Four u32 fields = 16 bytes, align 4.
        assert_eq!(size_of::<SourceLayoutInfoRepr>(), 16);
        assert_eq!(align_of::<SourceLayoutInfoRepr>(), 4);
        assert_eq!(offset_of!(SourceLayoutInfoRepr, plane0_stride_bytes), 0);
        assert_eq!(offset_of!(SourceLayoutInfoRepr, plane1_stride_bytes), 4);
        assert_eq!(offset_of!(SourceLayoutInfoRepr, plane1_offset_bytes), 8);
        assert_eq!(offset_of!(SourceLayoutInfoRepr, _reserved_padding), 12);
    }

    #[test]
    fn resolved_color_info_repr_layout() {
        // Four u32 discriminants = 16 bytes, align 4.
        assert_eq!(size_of::<ResolvedColorInfoRepr>(), 16);
        assert_eq!(align_of::<ResolvedColorInfoRepr>(), 4);
        assert_eq!(offset_of!(ResolvedColorInfoRepr, primaries_raw), 0);
        assert_eq!(offset_of!(ResolvedColorInfoRepr, transfer_raw), 4);
        assert_eq!(offset_of!(ResolvedColorInfoRepr, matrix_raw), 8);
        assert_eq!(offset_of!(ResolvedColorInfoRepr, range_raw), 12);
    }

    #[test]
    fn rhi_color_converter_methods_vtable_layout() {
        // v1:
        //   layout_version                    @ 0  (4 bytes, u32)
        //   _reserved_padding                 @ 4  (4 bytes, u32)
        //   prepare_buffer_to_image_storage   @ 8  (8 bytes, fn pointer)
        // Total = 16 bytes, align = 8.
        assert_eq!(size_of::<RhiColorConverterMethodsVTable>(), 16);
        assert_eq!(align_of::<RhiColorConverterMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(RhiColorConverterMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(RhiColorConverterMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(
                RhiColorConverterMethodsVTable,
                prepare_buffer_to_image_storage
            ),
            8
        );
    }

    #[test]
    fn image_copy_region_repr_layout() {
        // Field layout:
        //   width                @ 0   (4 bytes, u32)
        //   height               @ 4   (4 bytes, u32)
        //   buffer_offset        @ 8   (8 bytes, u64)
        //   buffer_row_length    @ 16  (4 bytes, u32)
        //   buffer_image_height  @ 20  (4 bytes, u32)
        //   mip_level            @ 24  (4 bytes, u32)
        //   array_layer          @ 28  (4 bytes, u32)
        //   _reserved_padding    @ 32  (4 bytes, u32)
        // Total = 40 bytes with 4-byte tail padding rounded up to
        // align(8) = 40 bytes. The struct's alignment is 8 because
        // of the `u64` field.
        assert_eq!(size_of::<ImageCopyRegionRepr>(), 40);
        assert_eq!(align_of::<ImageCopyRegionRepr>(), 8);
        assert_eq!(offset_of!(ImageCopyRegionRepr, width), 0);
        assert_eq!(offset_of!(ImageCopyRegionRepr, height), 4);
        assert_eq!(offset_of!(ImageCopyRegionRepr, buffer_offset), 8);
        assert_eq!(offset_of!(ImageCopyRegionRepr, buffer_row_length), 16);
        assert_eq!(offset_of!(ImageCopyRegionRepr, buffer_image_height), 20);
        assert_eq!(offset_of!(ImageCopyRegionRepr, mip_level), 24);
        assert_eq!(offset_of!(ImageCopyRegionRepr, array_layer), 28);
        assert_eq!(offset_of!(ImageCopyRegionRepr, _reserved_padding), 32);
    }

    #[test]
    fn rhi_command_recorder_methods_vtable_layout() {
        // v2 (v1 unchanged through @48, sibling slots appended):
        //   layout_version                       @ 0   (4 bytes, u32)
        //   _reserved_padding                    @ 4   (4 bytes, u32)
        //   begin                                @ 8   (8 bytes, fn pointer)
        //   record_image_barrier                 @ 16  (8 bytes, fn pointer)
        //   record_buffer_barrier                @ 24  (8 bytes, fn pointer)
        //   record_dispatch                      @ 32  (8 bytes, fn pointer)
        //   record_copy_image_to_buffer          @ 40  (8 bytes, fn pointer)
        //   submit_signaling_timeline            @ 48  (8 bytes, fn pointer)
        //   record_pixel_buffer_barrier          @ 56  (8 bytes, fn pointer, v2)
        //   record_copy_image_to_pixel_buffer    @ 64  (8 bytes, fn pointer, v2)
        // Total = 72 bytes, align = 8.
        assert_eq!(size_of::<RhiCommandRecorderMethodsVTable>(), 72);
        assert_eq!(align_of::<RhiCommandRecorderMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, begin),
            8
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, record_image_barrier),
            16
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, record_buffer_barrier),
            24
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, record_dispatch),
            32
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                record_copy_image_to_buffer
            ),
            40
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                submit_signaling_timeline
            ),
            48
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                record_pixel_buffer_barrier
            ),
            56
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                record_copy_image_to_pixel_buffer
            ),
            64
        );
    }

    #[test]
    fn gpu_context_limited_access_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 53 fn
        // pointers (8 bytes each) = 4 + 4 + 424 = 432 bytes.
        // (Phase F #957 appended texture_native_dma_buf_fd, taking the
        // count from 52 → 53. v12 / #958 appended
        // set_video_source_timeline_semaphore +
        // clear_video_source_timeline_semaphore, taking it 53 → 55.
        // v13 / #958 Phase E sub appended wait_timeline_semaphore,
        // taking it 55 → 56.)
        assert_eq!(size_of::<GpuContextLimitedAccessVTable>(), 456);
        assert_eq!(align_of::<GpuContextLimitedAccessVTable>(), 8);
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, layout_version), 0);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, _reserved_padding),
            4
        );
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, clone_handle), 8);
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, drop_handle), 16);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_pixel_buffer),
            24
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_pixel_buffer),
            32
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, strong_count_pixel_buffer),
            40
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, plane_base_address_pixel_buffer),
            48
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, plane_size_pixel_buffer),
            56
        );
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, clone_texture), 64);
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, drop_texture), 72);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_pooled_texture_handle),
            80
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, register_texture),
            88
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                update_texture_registration_layout
            ),
            96
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_texture),
            104
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, resolve_texture_by_surface_id),
            112
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, unregister_texture),
            120
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_storage_buffer),
            128
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_storage_buffer),
            136
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_uniform_buffer),
            144
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_uniform_buffer),
            152
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_vertex_buffer),
            160
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_vertex_buffer),
            168
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_index_buffer),
            176
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_index_buffer),
            184
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_storage_buffer),
            192
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_uniform_buffer),
            200
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_vertex_buffer),
            208
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_index_buffer),
            216
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_texture_registration),
            224
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_texture_registration),
            232
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, texture_registration_texture),
            240
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                texture_registration_current_layout
            ),
            248
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                texture_registration_update_layout
            ),
            256
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                resolve_texture_registration_by_surface_id
            ),
            264
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_rhi_command_queue),
            272
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_rhi_command_queue),
            280
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                create_command_buffer_from_queue
            ),
            288
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_command_buffer),
            296
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, commit_command_buffer),
            304
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, commit_and_wait_command_buffer),
            312
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, copy_texture_command_buffer),
            320
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, command_queue),
            328
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, create_command_buffer),
            336
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, copy_pixel_buffer_to_texture),
            344
        );
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, blit_copy), 352);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, blit_copy_iosurface),
            360
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, surface_store),
            368
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, check_out_surface),
            376
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_pixel_buffer),
            384
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, get_pixel_buffer),
            392
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, resolve_pixel_buffer_by_surface_id),
            400
        );
        // C3-added entries (Phase C3, #903).
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, escalate_begin),
            408
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, escalate_end),
            416
        );
        // Phase F entry (#908 / #957).
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, texture_native_dma_buf_fd),
            424
        );
        // v12 entries (#958).
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                set_video_source_timeline_semaphore
            ),
            432
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                clear_video_source_timeline_semaphore
            ),
            440
        );
        // v13 entry (#958 Phase E sub).
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, wait_timeline_semaphore),
            448
        );
    }

    #[test]
    fn gpu_context_full_access_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 30 fn
        // pointers (8 bytes each) = 4 + 4 + 240 = 248 bytes.
        //
        // 32 entries = 1 drop_handle + 7 clone/drop pairs (14 fn
        // pointers for the 7 β-shape return types: compute / graphics /
        // ray-tracing kernels, texture ring, color converter,
        // acceleration structure, command recorder) + 4 create_* method
        // callbacks (compute / graphics / ray-tracing / texture_ring)
        // + 1 acquire_render_target_dma_buf_image (C3) + 9 Phase D
        // privileged methods (wait_device_idle, acquire_output_texture,
        // upload_pixel_buffer_as_texture, color_converter,
        // create_command_recorder, build_triangles_blas, build_tlas,
        // supports_ray_tracing_pipeline, check_in_surface)
        // + 1 v5-added gpu_capabilities (#914)
        // + 1 v6-added create_timeline_semaphore (#914 / #920)
        // + 1 v7-added import_dma_buf_storage_buffer (#914 / #921).
        assert_eq!(size_of::<GpuContextFullAccessVTable>(), 264);
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
        // v4-added β-shape clone/drop pairs (#917).
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, clone_color_converter),
            80
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, drop_color_converter),
            88
        );
        assert_eq!(
            offset_of!(
                GpuContextFullAccessVTable,
                clone_acceleration_structure
            ),
            96
        );
        assert_eq!(
            offset_of!(
                GpuContextFullAccessVTable,
                drop_acceleration_structure
            ),
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
            offset_of!(
                GpuContextFullAccessVTable,
                upload_pixel_buffer_as_texture
            ),
            184
        );
        assert_eq!(
            offset_of!(GpuContextFullAccessVTable, color_converter),
            192
        );
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
            offset_of!(
                GpuContextFullAccessVTable,
                supports_ray_tracing_pipeline
            ),
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
    }

    #[test]
    fn gpu_capabilities_repr_layout() {
        // 256-byte device_name + u32 len + 4 u8 fields = 264 bytes.
        // 1-byte alignment (the byte array has 1-byte alignment, u32 has
        // 4-byte but follows the byte array directly; the trailing bools
        // are u8). Total stable across rustc.
        assert_eq!(size_of::<GpuCapabilitiesRepr>(), 264);
        assert_eq!(align_of::<GpuCapabilitiesRepr>(), 4);
        assert_eq!(offset_of!(GpuCapabilitiesRepr, device_name), 0);
        assert_eq!(offset_of!(GpuCapabilitiesRepr, device_name_len), 256);
        assert_eq!(
            offset_of!(GpuCapabilitiesRepr, supports_external_memory),
            260
        );
        assert_eq!(
            offset_of!(GpuCapabilitiesRepr, supports_cross_device_dma_buf_probe),
            261
        );
        assert_eq!(
            offset_of!(GpuCapabilitiesRepr, supports_ray_tracing_pipeline),
            262
        );
        assert_eq!(offset_of!(GpuCapabilitiesRepr, _reserved_padding), 263);
    }

    // -------------------------------------------------------------------------
    // Phase C2 descriptor mirror layouts
    // -------------------------------------------------------------------------

    #[test]
    fn compute_binding_spec_repr_layout() {
        assert_eq!(size_of::<ComputeBindingSpecRepr>(), 8);
        assert_eq!(align_of::<ComputeBindingSpecRepr>(), 4);
        assert_eq!(offset_of!(ComputeBindingSpecRepr, binding), 0);
        assert_eq!(offset_of!(ComputeBindingSpecRepr, kind), 4);
    }

    #[test]
    fn compute_kernel_descriptor_repr_layout() {
        // 3 (ptr, len) pairs (3 * 16 = 48) + u32 + u32 = 56 bytes on
        // 64-bit hosts.
        assert_eq!(size_of::<ComputeKernelDescriptorRepr>(), 56);
        assert_eq!(align_of::<ComputeKernelDescriptorRepr>(), 8);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, label_ptr), 0);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, label_len), 8);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, spv_ptr), 16);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, spv_len), 24);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, bindings_ptr), 32);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, bindings_len), 40);
        assert_eq!(
            offset_of!(ComputeKernelDescriptorRepr, push_constant_size),
            48
        );
        assert_eq!(
            offset_of!(ComputeKernelDescriptorRepr, _reserved_padding),
            52
        );
    }

    #[test]
    fn graphics_stage_repr_layout() {
        // u32 + u32 + 2 (ptr,len) pairs = 8 + 32 = 40 bytes.
        assert_eq!(size_of::<GraphicsStageRepr>(), 40);
        assert_eq!(align_of::<GraphicsStageRepr>(), 8);
        assert_eq!(offset_of!(GraphicsStageRepr, stage), 0);
        assert_eq!(offset_of!(GraphicsStageRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(GraphicsStageRepr, spv_ptr), 8);
        assert_eq!(offset_of!(GraphicsStageRepr, spv_len), 16);
        assert_eq!(offset_of!(GraphicsStageRepr, entry_point_ptr), 24);
        assert_eq!(offset_of!(GraphicsStageRepr, entry_point_len), 32);
    }

    #[test]
    fn graphics_binding_spec_repr_layout() {
        assert_eq!(size_of::<GraphicsBindingSpecRepr>(), 16);
        assert_eq!(align_of::<GraphicsBindingSpecRepr>(), 4);
        assert_eq!(offset_of!(GraphicsBindingSpecRepr, binding), 0);
        assert_eq!(offset_of!(GraphicsBindingSpecRepr, kind), 4);
        assert_eq!(offset_of!(GraphicsBindingSpecRepr, stages), 8);
        assert_eq!(offset_of!(GraphicsBindingSpecRepr, _reserved_padding), 12);
    }

    #[test]
    fn graphics_push_constants_repr_layout() {
        assert_eq!(size_of::<GraphicsPushConstantsRepr>(), 8);
        assert_eq!(align_of::<GraphicsPushConstantsRepr>(), 4);
        assert_eq!(offset_of!(GraphicsPushConstantsRepr, size), 0);
        assert_eq!(offset_of!(GraphicsPushConstantsRepr, stages), 4);
    }

    #[test]
    fn vertex_input_binding_repr_layout() {
        assert_eq!(size_of::<VertexInputBindingRepr>(), 16);
        assert_eq!(align_of::<VertexInputBindingRepr>(), 4);
        assert_eq!(offset_of!(VertexInputBindingRepr, binding), 0);
        assert_eq!(offset_of!(VertexInputBindingRepr, stride), 4);
        assert_eq!(offset_of!(VertexInputBindingRepr, input_rate), 8);
        assert_eq!(offset_of!(VertexInputBindingRepr, _reserved_padding), 12);
    }

    #[test]
    fn vertex_input_attribute_repr_layout() {
        assert_eq!(size_of::<VertexInputAttributeRepr>(), 16);
        assert_eq!(align_of::<VertexInputAttributeRepr>(), 4);
        assert_eq!(offset_of!(VertexInputAttributeRepr, location), 0);
        assert_eq!(offset_of!(VertexInputAttributeRepr, binding), 4);
        assert_eq!(offset_of!(VertexInputAttributeRepr, format), 8);
        assert_eq!(offset_of!(VertexInputAttributeRepr, offset), 12);
    }

    #[test]
    fn vertex_input_state_repr_layout() {
        // u32 + u32 + 2 (ptr,len) pairs = 8 + 32 = 40 bytes.
        assert_eq!(size_of::<VertexInputStateRepr>(), 40);
        assert_eq!(align_of::<VertexInputStateRepr>(), 8);
        assert_eq!(offset_of!(VertexInputStateRepr, kind), 0);
        assert_eq!(offset_of!(VertexInputStateRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(VertexInputStateRepr, bindings_ptr), 8);
        assert_eq!(offset_of!(VertexInputStateRepr, bindings_len), 16);
        assert_eq!(offset_of!(VertexInputStateRepr, attributes_ptr), 24);
        assert_eq!(offset_of!(VertexInputStateRepr, attributes_len), 32);
    }

    #[test]
    fn rasterization_state_repr_layout() {
        assert_eq!(size_of::<RasterizationStateRepr>(), 16);
        assert_eq!(align_of::<RasterizationStateRepr>(), 4);
        assert_eq!(offset_of!(RasterizationStateRepr, polygon_mode), 0);
        assert_eq!(offset_of!(RasterizationStateRepr, cull_mode), 4);
        assert_eq!(offset_of!(RasterizationStateRepr, front_face), 8);
        assert_eq!(offset_of!(RasterizationStateRepr, line_width), 12);
    }

    #[test]
    fn multisample_state_repr_layout() {
        assert_eq!(size_of::<MultisampleStateRepr>(), 8);
        assert_eq!(align_of::<MultisampleStateRepr>(), 4);
        assert_eq!(offset_of!(MultisampleStateRepr, samples), 0);
        assert_eq!(offset_of!(MultisampleStateRepr, _reserved_padding), 4);
    }

    #[test]
    fn depth_stencil_state_repr_layout() {
        assert_eq!(size_of::<DepthStencilStateRepr>(), 16);
        assert_eq!(align_of::<DepthStencilStateRepr>(), 4);
        assert_eq!(offset_of!(DepthStencilStateRepr, kind), 0);
        assert_eq!(offset_of!(DepthStencilStateRepr, depth_test), 4);
        assert_eq!(offset_of!(DepthStencilStateRepr, depth_write), 8);
        assert_eq!(offset_of!(DepthStencilStateRepr, _reserved_padding), 12);
    }

    #[test]
    fn color_blend_attachment_repr_layout() {
        assert_eq!(size_of::<ColorBlendAttachmentRepr>(), 32);
        assert_eq!(align_of::<ColorBlendAttachmentRepr>(), 4);
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, src_color_blend_factor),
            0
        );
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, dst_color_blend_factor),
            4
        );
        assert_eq!(offset_of!(ColorBlendAttachmentRepr, color_blend_op), 8);
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, src_alpha_blend_factor),
            12
        );
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, dst_alpha_blend_factor),
            16
        );
        assert_eq!(offset_of!(ColorBlendAttachmentRepr, alpha_blend_op), 20);
        assert_eq!(offset_of!(ColorBlendAttachmentRepr, color_write_mask), 24);
        assert_eq!(
            offset_of!(ColorBlendAttachmentRepr, _reserved_padding),
            28
        );
    }

    #[test]
    fn color_blend_state_repr_layout() {
        // u32 + u32 + ColorBlendAttachmentRepr(32) = 40 bytes.
        assert_eq!(size_of::<ColorBlendStateRepr>(), 40);
        assert_eq!(align_of::<ColorBlendStateRepr>(), 4);
        assert_eq!(offset_of!(ColorBlendStateRepr, kind), 0);
        assert_eq!(offset_of!(ColorBlendStateRepr, color_write_mask), 4);
        assert_eq!(offset_of!(ColorBlendStateRepr, attachment), 8);
    }

    #[test]
    fn attachment_formats_repr_layout() {
        // (ptr,len) pair + u32 + u32 = 16 + 8 = 24 bytes.
        assert_eq!(size_of::<AttachmentFormatsRepr>(), 24);
        assert_eq!(align_of::<AttachmentFormatsRepr>(), 8);
        assert_eq!(offset_of!(AttachmentFormatsRepr, color_ptr), 0);
        assert_eq!(offset_of!(AttachmentFormatsRepr, color_len), 8);
        assert_eq!(offset_of!(AttachmentFormatsRepr, has_depth), 16);
        assert_eq!(offset_of!(AttachmentFormatsRepr, depth), 20);
    }

    #[test]
    fn graphics_pipeline_state_repr_layout() {
        // topology(4) + pad(4) + vertex_input(40) + raster(16) +
        // multisample(8) + depth_stencil(16) + color_blend(40) +
        // attachment_formats(24) + dynamic_state(4) + pad(4) = 160 bytes.
        assert_eq!(size_of::<GraphicsPipelineStateRepr>(), 160);
        assert_eq!(align_of::<GraphicsPipelineStateRepr>(), 8);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, topology), 0);
        assert_eq!(
            offset_of!(GraphicsPipelineStateRepr, _reserved_padding1),
            4
        );
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, vertex_input), 8);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, rasterization), 48);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, multisample), 64);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, depth_stencil), 72);
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, color_blend), 88);
        assert_eq!(
            offset_of!(GraphicsPipelineStateRepr, attachment_formats),
            128
        );
        assert_eq!(offset_of!(GraphicsPipelineStateRepr, dynamic_state), 152);
        assert_eq!(
            offset_of!(GraphicsPipelineStateRepr, _reserved_padding2),
            156
        );
    }

    #[test]
    fn graphics_kernel_descriptor_repr_layout() {
        // label(ptr,len)=16 + stages(ptr,len)=16 + bindings(ptr,len)=16
        // + push_constants(8) + pipeline_state(160) +
        // descriptor_sets_in_flight(4) + pad(4) = 224 bytes.
        assert_eq!(size_of::<GraphicsKernelDescriptorRepr>(), 224);
        assert_eq!(align_of::<GraphicsKernelDescriptorRepr>(), 8);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, label_ptr), 0);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, label_len), 8);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, stages_ptr), 16);
        assert_eq!(offset_of!(GraphicsKernelDescriptorRepr, stages_len), 24);
        assert_eq!(
            offset_of!(GraphicsKernelDescriptorRepr, bindings_ptr),
            32
        );
        assert_eq!(
            offset_of!(GraphicsKernelDescriptorRepr, bindings_len),
            40
        );
        assert_eq!(
            offset_of!(GraphicsKernelDescriptorRepr, push_constants),
            48
        );
        assert_eq!(
            offset_of!(GraphicsKernelDescriptorRepr, pipeline_state),
            56
        );
        assert_eq!(
            offset_of!(
                GraphicsKernelDescriptorRepr,
                descriptor_sets_in_flight
            ),
            216
        );
        assert_eq!(
            offset_of!(GraphicsKernelDescriptorRepr, _reserved_padding),
            220
        );
    }

    #[test]
    fn ray_tracing_stage_repr_layout() {
        assert_eq!(size_of::<RayTracingStageRepr>(), 40);
        assert_eq!(align_of::<RayTracingStageRepr>(), 8);
        assert_eq!(offset_of!(RayTracingStageRepr, stage), 0);
        assert_eq!(offset_of!(RayTracingStageRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(RayTracingStageRepr, spv_ptr), 8);
        assert_eq!(offset_of!(RayTracingStageRepr, spv_len), 16);
        assert_eq!(offset_of!(RayTracingStageRepr, entry_point_ptr), 24);
        assert_eq!(offset_of!(RayTracingStageRepr, entry_point_len), 32);
    }

    #[test]
    fn ray_tracing_binding_spec_repr_layout() {
        assert_eq!(size_of::<RayTracingBindingSpecRepr>(), 16);
        assert_eq!(align_of::<RayTracingBindingSpecRepr>(), 4);
        assert_eq!(offset_of!(RayTracingBindingSpecRepr, binding), 0);
        assert_eq!(offset_of!(RayTracingBindingSpecRepr, kind), 4);
        assert_eq!(offset_of!(RayTracingBindingSpecRepr, stages), 8);
        assert_eq!(
            offset_of!(RayTracingBindingSpecRepr, _reserved_padding),
            12
        );
    }

    #[test]
    fn ray_tracing_push_constants_repr_layout() {
        assert_eq!(size_of::<RayTracingPushConstantsRepr>(), 8);
        assert_eq!(align_of::<RayTracingPushConstantsRepr>(), 4);
        assert_eq!(offset_of!(RayTracingPushConstantsRepr, size), 0);
        assert_eq!(offset_of!(RayTracingPushConstantsRepr, stages), 4);
    }

    #[test]
    fn ray_tracing_shader_group_repr_layout() {
        assert_eq!(size_of::<RayTracingShaderGroupRepr>(), 16);
        assert_eq!(align_of::<RayTracingShaderGroupRepr>(), 4);
        assert_eq!(offset_of!(RayTracingShaderGroupRepr, kind), 0);
        assert_eq!(
            offset_of!(RayTracingShaderGroupRepr, general_or_intersection),
            4
        );
        assert_eq!(offset_of!(RayTracingShaderGroupRepr, closest_hit), 8);
        assert_eq!(offset_of!(RayTracingShaderGroupRepr, any_hit), 12);
    }

    #[test]
    fn ray_tracing_kernel_descriptor_repr_layout() {
        // 4 (ptr,len) pairs (64) + push_constants(8) +
        // max_recursion_depth(4) + pad(4) = 80 bytes.
        assert_eq!(size_of::<RayTracingKernelDescriptorRepr>(), 80);
        assert_eq!(align_of::<RayTracingKernelDescriptorRepr>(), 8);
        assert_eq!(offset_of!(RayTracingKernelDescriptorRepr, label_ptr), 0);
        assert_eq!(offset_of!(RayTracingKernelDescriptorRepr, label_len), 8);
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, stages_ptr),
            16
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, stages_len),
            24
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, groups_ptr),
            32
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, groups_len),
            40
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, bindings_ptr),
            48
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, bindings_len),
            56
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, push_constants),
            64
        );
        assert_eq!(
            offset_of!(
                RayTracingKernelDescriptorRepr,
                max_recursion_depth
            ),
            72
        );
        assert_eq!(
            offset_of!(RayTracingKernelDescriptorRepr, _reserved_padding),
            76
        );
    }

    #[test]
    fn ray_tracing_shader_unused_sentinel() {
        // The "absent stage" sentinel matches VK_SHADER_UNUSED_KHR.
        assert_eq!(RAY_TRACING_SHADER_UNUSED, u32::MAX);
    }

    #[test]
    fn surface_store_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 13 fn
        // pointers (8 bytes each) = 4 + 4 + 104 = 112 bytes.
        assert_eq!(size_of::<SurfaceStoreVTable>(), 112);
        assert_eq!(align_of::<SurfaceStoreVTable>(), 8);
        assert_eq!(offset_of!(SurfaceStoreVTable, layout_version), 0);
        assert_eq!(offset_of!(SurfaceStoreVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(SurfaceStoreVTable, clone_handle), 8);
        assert_eq!(offset_of!(SurfaceStoreVTable, drop_handle), 16);
        assert_eq!(offset_of!(SurfaceStoreVTable, connect), 24);
        assert_eq!(offset_of!(SurfaceStoreVTable, disconnect), 32);
        assert_eq!(offset_of!(SurfaceStoreVTable, check_in), 40);
        assert_eq!(offset_of!(SurfaceStoreVTable, check_out), 48);
        assert_eq!(offset_of!(SurfaceStoreVTable, register_buffer), 56);
        assert_eq!(offset_of!(SurfaceStoreVTable, lookup_buffer), 64);
        assert_eq!(offset_of!(SurfaceStoreVTable, release), 72);
        assert_eq!(offset_of!(SurfaceStoreVTable, register_texture), 80);
        assert_eq!(
            offset_of!(SurfaceStoreVTable, register_pixel_buffer_with_timeline),
            88
        );
        assert_eq!(offset_of!(SurfaceStoreVTable, lookup_texture), 96);
        assert_eq!(offset_of!(SurfaceStoreVTable, update_image_layout), 104);
    }

    /// Compile-time witnesses that the vtable types are Send + Sync.
    /// This catches regressions where a struct field added to the
    /// vtable would break the unsafe impls.
    #[test]
    fn vtables_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RuntimeContextVTable>();
        assert_send_sync::<AudioClockVTable>();
        assert_send_sync::<RuntimeOpsVTable>();
        assert_send_sync::<GpuContextLimitedAccessVTable>();
        assert_send_sync::<SurfaceStoreVTable>();
        assert_send_sync::<GpuContextFullAccessVTable>();
        assert_send_sync::<RhiColorConverterMethodsVTable>();
        assert_send_sync::<RhiCommandRecorderMethodsVTable>();
        assert_send_sync::<HostServices>();
        assert_send_sync::<ProcessorVTable>();
    }
}

/// Plugin declaration exported by dynamic libraries.
///
/// Plugins export a static named `STREAMLIB_PLUGIN` of this type via
/// [`export_plugin!`]. The host's loader looks up the symbol,
/// validates `abi_version`, and invokes `register`.
#[repr(C)]
pub struct PluginDeclaration {
    /// Wire ABI version — must equal [`STREAMLIB_ABI_VERSION`] at
    /// load time.
    pub abi_version: u32,

    /// Register callback. Receives the host-services pointer; the
    /// cdylib's macro expansion uses it to install every per-DSO
    /// static's forwarder before registering processors.
    pub register: PluginRegisterFn,
}

// Safety: contains only a u32 and a function pointer.
unsafe impl Send for PluginDeclaration {}
unsafe impl Sync for PluginDeclaration {}

// =============================================================================
// export_plugin! macro
// =============================================================================

/// Export processors for dynamic loading.
///
/// Emits the `STREAMLIB_PLUGIN` static the host's loader looks for,
/// and generates the register callback that:
///
/// 1. Calls `streamlib::sdk::plugin::install_host_services` with the
///    host-services pointer. The helper validates layout, stores the
///    callback table for the cdylib's PUBSUB / schema-registry
///    forwarders, installs the tracing `ForwardingSubscriber`,
///    installs the iceoryx2-log forwarder, and returns a
///    `RegisterHelper` whose `register::<P>()` method assembles the
///    processor vtable + descriptor and routes through the host's
///    `processor_register` callback.
/// 2. Calls `helper.register::<$processor>()` for each declared
///    processor type, registering it with the host's registry.
///
/// Step 1 must run before step 2: the registry's `register::<P>()`
/// path emits a `RuntimeDidRegisterProcessorType` PUBSUB event and a
/// `tracing::info!` line, both of which only flow back to the host
/// once the forwarders are in place.
///
/// # Example
///
/// ```ignore
/// export_plugin!(MyProcessor::Processor);
/// export_plugin!(ProcessorA::Processor, ProcessorB::Processor);
/// ```
#[macro_export]
macro_rules! export_plugin {
    ($($processor:ty),* $(,)?) => {
        /// Generated by `streamlib_plugin_abi::export_plugin!`.
        ///
        /// # Safety
        ///
        /// `host_services` must point at a layout-compatible
        /// [`HostServices`] payload, per the [`PluginRegisterFn`]
        /// contract.
        #[allow(non_snake_case)]
        unsafe extern "C" fn __streamlib_plugin_register(
            host_services: *const ::core::ffi::c_void,
        ) {
            // Panic across an `extern "C"` boundary is UB.
            // `catch_unwind` contains any unwinding within the cdylib;
            // a panic in `install_host_services` or
            // `helper.register::<_>()` is converted to silent return.
            // The host's post-call "processor not registered" check
            // surfaces a clear configuration error in that case.
            let _ = ::std::panic::catch_unwind(|| {
                // SAFETY: forwarded per the [`PluginRegisterFn`] contract.
                let helper = unsafe {
                    ::streamlib::sdk::plugin::install_host_services(host_services)
                };
                let Some(helper) = helper else {
                    return;
                };
                $(
                    helper.register::<$processor>();
                )*
            });
        }

        #[unsafe(no_mangle)]
        pub static STREAMLIB_PLUGIN: $crate::PluginDeclaration = $crate::PluginDeclaration {
            abi_version: $crate::STREAMLIB_ABI_VERSION,
            register: __streamlib_plugin_register,
        };
    };
}
