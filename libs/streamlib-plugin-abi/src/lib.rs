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
pub const HOST_SERVICES_LAYOUT_VERSION: u32 = 3;

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
pub const RUNTIME_OPS_VTABLE_LAYOUT_VERSION: u32 = 1;

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
}

unsafe impl Send for RuntimeOpsVTable {}
unsafe impl Sync for RuntimeOpsVTable {}

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
        // 4 + 4 + 5 fn pointers = 48 bytes
        assert_eq!(size_of::<RuntimeOpsVTable>(), 48);
        assert_eq!(align_of::<RuntimeOpsVTable>(), 8);
        assert_eq!(offset_of!(RuntimeOpsVTable, layout_version), 0);
        assert_eq!(offset_of!(RuntimeOpsVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(RuntimeOpsVTable, add_processor), 8);
        assert_eq!(offset_of!(RuntimeOpsVTable, remove_processor), 16);
        assert_eq!(offset_of!(RuntimeOpsVTable, connect), 24);
        assert_eq!(offset_of!(RuntimeOpsVTable, disconnect), 32);
        assert_eq!(offset_of!(RuntimeOpsVTable, to_json), 40);
    }

    #[test]
    fn host_services_v3_layout_version_is_bumped() {
        assert_eq!(HOST_SERVICES_LAYOUT_VERSION, 3);
        assert_eq!(STREAMLIB_ABI_VERSION, 4);
        assert_eq!(RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(RUNTIME_OPS_VTABLE_LAYOUT_VERSION, 1);
    }

    #[test]
    fn host_services_v3_tail_carries_three_vtable_pointers() {
        // The v3 additions live at the tail of HostServices. We don't
        // pin the absolute offsets (earlier fields are subject to
        // their own pre-v3 layout audit), but we do pin:
        //   1. Each vtable is a single 8-byte pointer.
        //   2. They appear in the order RuntimeContext → AudioClock → RuntimeOps.
        //   3. They are contiguous (no padding inserted between them).
        assert_eq!(size_of::<*const RuntimeContextVTable>(), 8);
        assert_eq!(size_of::<*const AudioClockVTable>(), 8);
        assert_eq!(size_of::<*const RuntimeOpsVTable>(), 8);

        let runtime_ctx_off = offset_of!(HostServices, runtime_context_vtable);
        let audio_clock_off = offset_of!(HostServices, audio_clock_vtable);
        let runtime_ops_off = offset_of!(HostServices, runtime_ops_vtable);
        assert!(runtime_ctx_off < audio_clock_off);
        assert!(audio_clock_off < runtime_ops_off);
        assert_eq!(audio_clock_off - runtime_ctx_off, 8);
        assert_eq!(runtime_ops_off - audio_clock_off, 8);

        // The runtime-ops pointer must end at the end of the struct
        // (it is the last field added in v3).
        assert_eq!(runtime_ops_off + 8, size_of::<HostServices>());
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
