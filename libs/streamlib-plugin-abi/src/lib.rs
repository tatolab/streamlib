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
pub const STREAMLIB_ABI_VERSION: u32 = 3;

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
pub const HOST_SERVICES_LAYOUT_VERSION: u32 = 2;

/// Layout version of the [`ProcessorVTable`] struct. Read by the
/// host's `processor_register` impl before dereferencing any vtable
/// entry; mismatching versions abort the registration cleanly.
pub const PROCESSOR_VTABLE_LAYOUT_VERSION: u32 = 1;

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
}

// Note: cdylib-side async-lifecycle wrappers (`setup` / `teardown` /
// `on_pause` / `on_resume`) grab the tokio handle from the
// `RuntimeContext*Access` they receive as a parameter
// (`ctx.tokio_handle()`). Tokio handle layout is host/cdylib-shared
// via the workspace-pinned tokio version. Phase B's RuntimeContext
// callback table will lift that crossing to extern "C" if /
// when multi-builder tokio drift becomes a real concern; until then,
// the tokio handle is one of the few remaining shared-type
// crossings Phase A leaves in place by design.

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
