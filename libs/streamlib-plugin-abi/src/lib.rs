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
///   field. Phase C1 (#901) populates the static; for hosts that
///   ship a GpuContext, the pointer is non-null. Hosts without GPU
///   support set it to `null` and cdylib code must check before
///   dispatching.
/// - v5: [`SurfaceStoreVTable`] reference appended (Phase C1 Phase
///   2E). The cdylib-side `SurfaceStore` shim's `(handle, vtable)`
///   pair sources its vtable pointer from this field. Hosts that
///   ship a `SurfaceStore` set it non-null; hosts that don't (or
///   where `gpu.surface_store()` returns `None`) leave it null and
///   cdylib code must check before dispatching.
pub const HOST_SERVICES_LAYOUT_VERSION: u32 = 5;

/// Layout version of the [`ProcessorVTable`] struct. Read by the
/// host's `processor_register` impl before dereferencing any vtable
/// entry; mismatching versions abort the registration cleanly.
pub const PROCESSOR_VTABLE_LAYOUT_VERSION: u32 = 1;

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
///
/// - v1: scaffold for Phase C1 Phase 2E. `clone_handle` / `drop_handle`
///   for owning-Arc lifecycle on `Arc<SurfaceStoreInner>`, plus the 11
///   method-dispatch callbacks for the cross-platform and Linux-only
///   surface-share operations: `connect`, `disconnect`, `check_in`,
///   `check_out`, `register_buffer`, `lookup_buffer`, `release`,
///   `register_texture`, `register_pixel_buffer_with_timeline`,
///   `lookup_texture`, `update_image_layout`.
pub const SURFACE_STORE_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Layout version of [`GpuContextLimitedAccessVTable`].
///
/// - v1: scaffold — layout-version + `clone_handle` / `drop_handle`.
/// - v2: per-type PixelBuffer clone/drop callbacks
///   (`clone_pixel_buffer` / `drop_pixel_buffer`). The cdylib's
///   `PixelBuffer` is `(handle, vtable, cached POD)`; Clone/Drop
///   dispatch through these callbacks so the host's `Arc<PixelBufferRef>`
///   refcount is managed by host-compiled code regardless of which
///   DSO holds the `PixelBuffer`. Same rationale as `RuntimeOpsVTable`
///   v2's `clone_handle`/`drop_handle` but specific to the
///   `PixelBuffer` return type.
/// - v3: PixelBuffer method-dispatch callbacks (`strong_count_pixel_buffer`,
///   `plane_base_address_pixel_buffer`, `plane_size_pixel_buffer`).
///   The remaining non-cached `PixelBuffer` methods now dispatch
///   through host-compiled code instead of casting the handle to
///   `*const PixelBufferRef` cdylib-side — eliminates the cross-DSO
///   `Arc::from_raw` / direct deref UB landmine the v2 scaffold left
///   in place.
/// - v4: per-type `Texture` clone/drop pair (`clone_texture` /
///   `drop_texture`) for the new β-reshape that lifted `Texture`'s
///   `Arc<HostVulkanTexture>` field behind a `(handle, vtable, POD)`
///   wrapper; per-type `PooledTextureHandle` drop callback
///   (`drop_pooled_texture_handle`) — the type is intentionally NOT
///   `Clone` because Drop releases a pool slot, so no clone callback;
///   and six `Texture`-related method-dispatch callbacks
///   (`register_texture`, `register_texture_with_layout`,
///   `update_texture_registration_layout`, `acquire_texture`,
///   `resolve_texture_by_surface_id`, `unregister_texture`). Same
///   rationale as v2 / v3 — keep Arc accounting and the methods that
///   touch host-internal RHI types in host-compiled code regardless
///   of caller DSO.
/// - v5: per-type Linux-only buffer clone/drop pairs for each of
///   `StorageBuffer` / `UniformBuffer` / `VertexBuffer` /
///   `IndexBuffer` (8 callbacks), plus the 4 `acquire_*_buffer`
///   method-dispatch callbacks. Each buffer type wraps the same
///   `Arc<HostVulkanBuffer>` under the hood but keeps a distinct
///   Rust-level type for binding-shape enforcement; the vtable
///   mirrors that by giving each its own pair so future divergence
///   (e.g. a buffer type growing per-type state) doesn't require
///   re-versioning the shared callback. Callbacks are stubs on
///   non-Linux hosts (the buffer types only exist on Linux); the
///   vtable layout is unconditional so the cdylib-side ABI stays
///   stable across triples.
/// - v6: per-type `TextureRegistration` clone/drop pair, three
///   method-dispatch callbacks (`texture_registration_texture`,
///   `texture_registration_current_layout`,
///   `texture_registration_update_layout`), and the
///   `resolve_texture_registration_by_surface_id` method-dispatch
///   callback. `TextureRegistration` was previously returned as
///   `Arc<TextureRegistration>` (Arc layout is rustc-version-
///   dependent — unsafe to cross the cdylib boundary); the
///   β-reshape collapses the return type to a `(handle, vtable)`
///   wrapper that's Arc-semantics-equivalent (cheap Clone via
///   vtable refcount bump) with host-compiled refcount accounting.
/// - v7: per-type `RhiCommandQueue` clone/drop pair + 1 method
///   (`create_command_buffer_from_queue`); per-type `CommandBuffer`
///   drop + 2 consume-semantics commit callbacks
///   (`commit_command_buffer`, `commit_and_wait_command_buffer`) +
///   1 mutator (`copy_texture_command_buffer`) — total 5 lifecycle
///   + per-type-method callbacks. Plus 5 `GpuContextLimitedAccess`
///   method-dispatch callbacks (`command_queue`,
///   `create_command_buffer`, `copy_pixel_buffer_to_texture`,
///   `blit_copy`, `blit_copy_iosurface`). 12 callbacks total.
///   `CommandBuffer` is deliberately NOT `Clone` (single-use
///   commit-semantics); the cdylib's `commit(self)` /
///   `commit_and_wait(self)` impls null the local handle/vtable
///   fields after the callback so Drop becomes a no-op (the host
///   already dropped the inner during commit).
/// - v8: two `SurfaceStore`-related method-dispatch callbacks on
///   the parent vtable: `surface_store` (returns an owned
///   `SurfaceStore` β-shape into an out-param; null handle ↔ None)
///   and `check_out_surface` (convenience method that delegates to
///   the engine's `SurfaceStore::check_out` while keeping the
///   surface-share lookup hidden behind the GpuContext capability
///   surface). The bulk of the `SurfaceStore` ABI lives on its own
///   [`SurfaceStoreVTable`], reached via
///   [`HostServices::surface_store_vtable`].
/// - v9: three remaining PixelBuffer method-dispatch callbacks
///   (`acquire_pixel_buffer` returning a `(PixelBufferPoolId,
///   PixelBuffer)` tuple via paired out-params; `get_pixel_buffer`
///   keyed by `PixelBufferPoolId`-as-bytes;
///   `resolve_pixel_buffer_by_surface_id`). Closes the cross-DSO
///   loop for the PixelBuffer surface — every public method on
///   `GpuContextLimitedAccess` that touches a PixelBuffer is now
///   layout-stable.
pub const GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION: u32 = 9;

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
/// Methods bodies still receive `&RuntimeContext*Access` references
/// crossing via Rust trait-object dispatch; those are Phase B + C
/// (see `streamlib-plugin-abi`'s parent issue).
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
    // Iceoryx2 wiring (returns Rust types via raw pointer — known
    // source-coupling for OutputWriter / InputMailboxes; see Phase A
    // AI Agent Notes for the deferred-flip rationale)
    // -------------------------------------------------------------------------

    pub has_iceoryx2_outputs: unsafe extern "C" fn(instance: *const c_void) -> bool,
    pub has_iceoryx2_inputs: unsafe extern "C" fn(instance: *const c_void) -> bool,

    /// Returns `Arc::into_raw(arc).cast()` of an `OutputWriter` arc
    /// the processor exposes, or null for "no output writer". The
    /// host casts back via `Arc::from_raw`, taking ownership of one
    /// strong reference. The cdylib wrapper consumes the `Arc<...>`
    /// returned by `<P as GeneratedProcessor>::get_iceoryx2_output_writer`
    /// directly — that trait method is responsible for returning a
    /// clone (the macro emits `Some(self.outputs.clone())`).
    pub get_iceoryx2_output_writer_arc:
        unsafe extern "C" fn(instance: *const c_void) -> *const c_void,

    /// Returns `&mut self.inputs as *mut InputMailboxes` cast to
    /// `*mut c_void`, or null for "no input mailboxes". The pointer
    /// is valid for the lifetime of `instance` — caller must not
    /// hold across other vtable calls (the cdylib could mutate
    /// during a `process()` call from another thread). In practice
    /// the compiler op holds the lock on `instance` and so the
    /// borrow is sound.
    pub get_iceoryx2_input_mailboxes_mut:
        unsafe extern "C" fn(instance: *mut c_void) -> *mut c_void,

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
/// shim uses to read host-owned runtime context state. Replaces the
/// Rust trait-object / struct-layout-shared crossings Phase A left in
/// place at the cdylib's `ctx.<accessor>()` boundary.
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
/// opaque handles. Their callable surface is defined by the GpuContext
/// callback tables (Phase C — see #886). For Phase B the handles
/// suffice as identity tokens that the cdylib stashes and Phase C
/// fills in.
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
    // GPU context handles (Phase C wires their methods)
    // -------------------------------------------------------------------------

    /// Returns an opaque handle to the privileged [`GpuContextFullAccess`].
    /// Pointer is valid for the lifetime of the surrounding
    /// `RuntimeContextFullAccess` shim. Phase C (#886) defines the
    /// callback table the cdylib uses to invoke methods on the handle.
    pub gpu_full_access: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,

    /// Returns an opaque handle to the restricted [`GpuContextLimitedAccess`].
    /// Same lifetime and Phase C contract as
    /// [`Self::gpu_full_access`].
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
    // PixelBuffer return-type lifetime (v2 — Phase C1)
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
    // PixelBuffer method-dispatch (v3 — eliminate cross-DSO Arc::from_raw)
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
    // Texture return-type lifetime (v4 — Phase C1 Phase 2A)
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
    // Method dispatch — Texture-related (v4)
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
    // Linux-only buffer Arc-handle lifecycle (v5 — Phase C1 Phase 2B)
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
    // Linux-only buffer acquire methods (v5)
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
    // TextureRegistration Arc-handle lifecycle (v6 — Phase C1 Phase 2C)
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
    // TextureRegistration method dispatch (v6)
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
    // RhiCommandQueue Arc-handle lifecycle (v7 — Phase C1 Phase 2D)
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
    // CommandBuffer lifecycle — drop + consume-semantics commits (v7)
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
    /// `*const Texture` pointers — the layout is locked by Phase
    /// 2A's `texture_layout` test so the host's read agrees with the
    /// cdylib's write.
    pub copy_texture_command_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        src: *const c_void,
        dst: *const c_void,
    ),

    // -------------------------------------------------------------------------
    // GpuContextLimitedAccess method dispatch — 5 methods (v7)
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
    // SurfaceStore accessors (v8 — Phase C1 Phase 2E)
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
    // Remaining PixelBuffer methods (v9 — Phase C1 Phase 2F)
    // -------------------------------------------------------------------------
    //
    // Last three methods on `GpuContextLimitedAccess` that touch
    // `PixelBuffer`. Closes the cross-DSO loop for the PixelBuffer
    // surface (Phase 0 already β-reshaped the type; Phase 2F wires
    // the remaining acquire / lookup methods through the vtable).

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
}

unsafe impl Send for GpuContextLimitedAccessVTable {}
unsafe impl Sync for GpuContextLimitedAccessVTable {}

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
/// 2. **Phase B precedent** — `AudioClockVTable` already established
///    the "separate vtable per significant subsystem" pattern (held
///    at the `RuntimeContext` level via
///    [`HostServices::audio_clock_vtable`]).
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
    // GpuContext vtable surface (v4 — Phase C1, #901)
    // -------------------------------------------------------------------------

    /// Static dispatch table for the host's `GpuContextLimitedAccess`.
    /// Paired with the per-instance handle returned by
    /// [`RuntimeContextVTable::gpu_limited_access`]. Set once at
    /// install time; non-null for hosts that ship a GpuContext,
    /// null otherwise (cdylib must check before dispatching).
    pub gpu_context_limited_access_vtable: *const GpuContextLimitedAccessVTable,

    // -------------------------------------------------------------------------
    // SurfaceStore vtable surface (v5 — Phase C1 Phase 2E, #901)
    // -------------------------------------------------------------------------

    /// Static dispatch table for the host's `SurfaceStore`. Paired
    /// with the per-`SurfaceStore` handle returned by
    /// [`GpuContextLimitedAccessVTable::surface_store`]. Set once at
    /// install time; non-null for hosts that ship a `SurfaceStore`,
    /// null otherwise (cdylib must check before dispatching).
    pub surface_store_vtable: *const SurfaceStoreVTable,
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
        assert_eq!(HOST_SERVICES_LAYOUT_VERSION, 5);
        assert_eq!(STREAMLIB_ABI_VERSION, 4);
        assert_eq!(RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, 1);
        // v2: added owning-Arc handle lifetime callbacks
        // (`clone_handle` / `drop_handle`).
        assert_eq!(RUNTIME_OPS_VTABLE_LAYOUT_VERSION, 2);
        // v9 (Phase C1 Phase 2F): added the 3 remaining PixelBuffer
        // method-dispatch callbacks (acquire_pixel_buffer,
        // get_pixel_buffer, resolve_pixel_buffer_by_surface_id) —
        // closes the cross-DSO loop for the PixelBuffer surface.
        assert_eq!(GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION, 9);
        // v1: scaffold for the SurfaceStore subsystem.
        assert_eq!(SURFACE_STORE_VTABLE_LAYOUT_VERSION, 1);
    }

    #[test]
    fn host_services_tail_carries_five_vtable_pointers() {
        // The v3-v5 additions live at the tail of HostServices. We
        // don't pin the absolute offsets (earlier fields are subject
        // to their own pre-v3 layout audit), but we do pin:
        //   1. Each vtable is a single 8-byte pointer.
        //   2. They appear in the order RuntimeContext → AudioClock →
        //      RuntimeOps → GpuContextLimitedAccess → SurfaceStore.
        //   3. They are contiguous (no padding inserted between them).
        assert_eq!(size_of::<*const RuntimeContextVTable>(), 8);
        assert_eq!(size_of::<*const AudioClockVTable>(), 8);
        assert_eq!(size_of::<*const RuntimeOpsVTable>(), 8);
        assert_eq!(size_of::<*const GpuContextLimitedAccessVTable>(), 8);
        assert_eq!(size_of::<*const SurfaceStoreVTable>(), 8);

        let runtime_ctx_off = offset_of!(HostServices, runtime_context_vtable);
        let audio_clock_off = offset_of!(HostServices, audio_clock_vtable);
        let runtime_ops_off = offset_of!(HostServices, runtime_ops_vtable);
        let gpu_lim_off = offset_of!(HostServices, gpu_context_limited_access_vtable);
        let surface_store_off = offset_of!(HostServices, surface_store_vtable);
        assert!(runtime_ctx_off < audio_clock_off);
        assert!(audio_clock_off < runtime_ops_off);
        assert!(runtime_ops_off < gpu_lim_off);
        assert!(gpu_lim_off < surface_store_off);
        assert_eq!(audio_clock_off - runtime_ctx_off, 8);
        assert_eq!(runtime_ops_off - audio_clock_off, 8);
        assert_eq!(gpu_lim_off - runtime_ops_off, 8);
        assert_eq!(surface_store_off - gpu_lim_off, 8);

        // The surface-store pointer must end at the end of the
        // struct (it is the last field added in v5).
        assert_eq!(surface_store_off + 8, size_of::<HostServices>());
    }

    #[test]
    fn gpu_context_limited_access_vtable_layout() {
        // v7 (Phase C1 Phase 2D): layout_version (u32) +
        // _reserved_padding (u32) + 45 fn pointers (8 bytes each):
        //   v2 (4): clone_handle, drop_handle,
        //           clone_pixel_buffer, drop_pixel_buffer,
        //   v3 (3): strong_count_pixel_buffer, plane_base_address_pixel_buffer,
        //           plane_size_pixel_buffer,
        //   v4 (8): clone_texture, drop_texture, drop_pooled_texture_handle,
        //           register_texture, update_texture_registration_layout,
        //           acquire_texture, resolve_texture_by_surface_id,
        //           unregister_texture,
        //   v5 (12): clone_storage_buffer, drop_storage_buffer,
        //           clone_uniform_buffer, drop_uniform_buffer,
        //           clone_vertex_buffer, drop_vertex_buffer,
        //           clone_index_buffer, drop_index_buffer,
        //           acquire_storage_buffer, acquire_uniform_buffer,
        //           acquire_vertex_buffer, acquire_index_buffer,
        //   v6 (6): clone_texture_registration, drop_texture_registration,
        //           texture_registration_texture,
        //           texture_registration_current_layout,
        //           texture_registration_update_layout,
        //           resolve_texture_registration_by_surface_id,
        //   v7 (12): clone_rhi_command_queue, drop_rhi_command_queue,
        //           create_command_buffer_from_queue,
        //           drop_command_buffer, commit_command_buffer,
        //           commit_and_wait_command_buffer,
        //           copy_texture_command_buffer,
        //           command_queue, create_command_buffer,
        //           copy_pixel_buffer_to_texture, blit_copy,
        //           blit_copy_iosurface,
        //   v8 (2): surface_store, check_out_surface,
        //   v9 (3): acquire_pixel_buffer, get_pixel_buffer,
        //           resolve_pixel_buffer_by_surface_id.
        // = 4 + 4 + 400 = 408 bytes.
        assert_eq!(size_of::<GpuContextLimitedAccessVTable>(), 408);
        assert_eq!(align_of::<GpuContextLimitedAccessVTable>(), 8);
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, layout_version), 0);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, _reserved_padding),
            4
        );
        // v2 entries
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
        // v3 entries
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
        // v4 entries
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
        // v5 entries (Phase 2B): 8 buffer clone/drop pairs + 4
        // acquire_*_buffer = 12 fn pointers appended. Total vtable
        // grows from 128 to 128 + 12*8 = 224 bytes.
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
        // v6 entries (Phase 2C): 2 clone/drop + 3 method-dispatch + 1
        // resolve = 6 fn pointers appended. Vtable grows from 224 to
        // 224 + 6*8 = 272 bytes.
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
        // v7 entries (Phase 2D): 12 fn pointers appended. Vtable
        // grows from 272 to 272 + 12*8 = 368 bytes.
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
        // v8 entries (Phase 2E): 2 fn pointers appended.
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, surface_store),
            368
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, check_out_surface),
            376
        );
        // v9 entries (Phase 2F): 3 fn pointers appended.
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
    }

    #[test]
    fn surface_store_vtable_layout() {
        // SurfaceStoreVTable v1 (Phase C1 Phase 2E):
        //   layout_version (u32) + _reserved_padding (u32) +
        //   13 fn pointers (8 bytes each):
        //     clone_handle, drop_handle (2),
        //     connect, disconnect, check_in, check_out,
        //     register_buffer, lookup_buffer, release (7),
        //     register_texture, register_pixel_buffer_with_timeline,
        //     lookup_texture, update_image_layout (4).
        // = 4 + 4 + 104 = 112 bytes.
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
