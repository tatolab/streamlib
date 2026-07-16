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

// ==============================================================================
// Module declarations + crate-root re-exports
// ==============================================================================

mod primitives;
pub mod repr;
pub mod vtables;

pub use primitives::*;
pub use repr::*;
pub use vtables::*;

// =============================================================================
// Wire ABI version
// =============================================================================

/// Current ABI version. Plugins must match this exactly at load time.
/// Bumped when the wire shape of [`PluginDeclaration`], the register
/// callback's signature, or [`HostServices`]'s layout changes
/// incompatibly. Same-major-version layout additions append to the
/// end of [`HostServices`] and read the new fields only when
/// `abi_layout_version` advertises them.
///
/// - v5: [`PluginDeclaration`] grows the build-fingerprint handshake
///   (`abi_layout_fingerprint`, `engine_transit_fingerprint`,
///   `build_identity_ptr` / `build_identity_len`). The host refuses,
///   with a typed error, any plugin whose measured ABI layout or
///   engine-internal transit layout could skew from its own. The
///   `abi_version` field stays pinned at offset 0 and is read first,
///   so the appended fields are dereferenced only from a v5
///   declaration.
pub const STREAMLIB_ABI_VERSION: u32 = 5;

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
///   host wiring + cdylib PluginAbiObject, locking the wire format before
///   the scope machinery turns it on).
/// - v12: [`RhiColorConverterMethodsVTable`] reference appended.
///   The cdylib-side `RhiColorConverter` PluginAbiObject's `methods_vtable`
///   field sources its pointer from this field. Non-null for hosts
///   that ship a GpuContext; null otherwise (cdylib code must check
///   before dispatching). Phase E sub-lift slice A wires the
///   `prepare_buffer_to_image_storage` method through it so cdylib
///   camera processors can prepare a color-conversion kernel without
///   tripping the host-mode-only `host_inner()` panic.
/// - v13: [`RhiCommandRecorderMethodsVTable`] reference appended.
///   The cdylib-side `RhiCommandRecorder` PluginAbiObject's `methods_vtable`
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
///   crossings in the plugin ABI). The cdylib's PluginAbiObject
///   `OutputWriter` / `InputMailboxes` field types source their
///   vtable pointers from these slots; per-frame `write_raw` /
///   `read_raw` dispatch through them. Paired with the
///   `set_iceoryx2_resources` slot on `ProcessorVTable` v2 which
///   delivers the per-instance opaque handles. Non-null for every
///   host that wires processors through iceoryx2.
/// - v15: M32 one-shot slot reservation (#1253) appends five per-type
///   methods-vtable pointers under this single bump —
///   `present_target_methods_vtable` (#1258),
///   `video_encoder_session_methods_vtable` +
///   `video_decoder_session_methods_vtable` (#1259),
///   `host_timeline_semaphore_methods_vtable` (#1260), and
///   `vulkan_texture_readback_methods_vtable` (#1261). Each sources the
///   host's static per-type dispatch table for the matching mintable
///   PluginAbiObject; non-null for hosts that ship a GpuContext, null
///   otherwise (cdylib must check before dispatching). Paired with the
///   `GpuContextFullAccessVTable` v10→v11 growth.
pub const HOST_SERVICES_LAYOUT_VERSION: u32 = 15;

// =============================================================================
// Build-fingerprint folding — shared FNV-1a helpers
// =============================================================================

/// FNV-1a 64-bit offset basis.
const FNV1A_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV1A_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Begin an FNV-1a fold accumulator.
pub const fn fingerprint_init() -> u64 {
    FNV1A_OFFSET_BASIS
}

/// Fold one `u64` (little-endian bytes) into an FNV-1a accumulator.
pub const fn fingerprint_fold_u64(mut hash: u64, value: u64) -> u64 {
    let bytes = value.to_le_bytes();
    let mut i = 0;
    while i < 8 {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV1A_PRIME);
        i += 1;
    }
    hash
}

/// Fold a byte slice into an FNV-1a accumulator.
pub const fn fingerprint_fold_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV1A_PRIME);
        i += 1;
    }
    hash
}

/// Fold a type's measured `size_of` + `align_of` into an FNV-1a
/// accumulator. Measured from the compiled binary, so a same-declared-
/// version / different-actual-layout republish of any dispatch-surface
/// type changes the fingerprint.
const fn fingerprint_fold_layout(hash: u64, size: usize, align: usize) -> u64 {
    let hash = fingerprint_fold_u64(hash, size as u64);
    fingerprint_fold_u64(hash, align as u64)
}

/// Structural fingerprint of the `#[repr(C)]` plugin-ABI dispatch
/// surface: the declared layout-version constants plus the measured
/// `size_of` / `align_of` of [`HostServices`], [`PluginDeclaration`],
/// and every vtable struct the host and plugin dereference field-by-
/// field.
///
/// The host folds this into its own build fingerprint and refuses,
/// with a typed error, any plugin whose value differs. Because it is
/// *measured* (not a hand-maintained constant), a layout-changing edit
/// to any dispatch-surface struct that forgot to bump the matching
/// layout-version constant still changes this value — catching the
/// same-constant / different-layout republish a version-only check
/// would miss.
///
/// The pure-POD `repr/` structs (`ComputeKernelDescriptorRepr`,
/// `GraphicsPipelineStateRepr`, …) are intentionally *not* folded here:
/// they are `#[repr(C)]` with primitive / explicit-`repr` fields only,
/// so their layout is fully source-determined and identical across any
/// two builds of the same source. Their byte layout is locked by the
/// per-struct `offset_of!` regression tests in each `repr/` module.
pub const PLUGIN_ABI_LAYOUT_FINGERPRINT: u64 = {
    use core::mem::{align_of, size_of};
    let mut h = fingerprint_init();

    // Declared version constants.
    h = fingerprint_fold_u64(h, STREAMLIB_ABI_VERSION as u64);
    h = fingerprint_fold_u64(h, HOST_SERVICES_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, PROCESSOR_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, AUDIO_CLOCK_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, RUNTIME_OPS_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, SURFACE_STORE_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(
        h,
        VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION as u64,
    );
    h = fingerprint_fold_u64(
        h,
        VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION as u64,
    );
    h = fingerprint_fold_u64(
        h,
        VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION as u64,
    );
    h = fingerprint_fold_u64(
        h,
        VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION as u64,
    );
    h = fingerprint_fold_u64(h, RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, OUTPUT_WRITER_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(h, INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION as u64);
    // v11/v15 (M32 #1253) — the five new surface methods vtables.
    h = fingerprint_fold_u64(h, PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION as u64);
    h = fingerprint_fold_u64(
        h,
        VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION as u64,
    );
    h = fingerprint_fold_u64(
        h,
        VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION as u64,
    );
    h = fingerprint_fold_u64(
        h,
        HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION as u64,
    );
    h = fingerprint_fold_u64(
        h,
        VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION as u64,
    );

    // Measured layout of the wire envelope + callback table.
    h = fingerprint_fold_layout(
        h,
        size_of::<PluginDeclaration>(),
        align_of::<PluginDeclaration>(),
    );
    h = fingerprint_fold_layout(h, size_of::<HostServices>(), align_of::<HostServices>());

    // Measured layout of every vtable struct dereferenced field-by-field.
    h = fingerprint_fold_layout(
        h,
        size_of::<ProcessorVTable>(),
        align_of::<ProcessorVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<RuntimeContextVTable>(),
        align_of::<RuntimeContextVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<AudioClockVTable>(),
        align_of::<AudioClockVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<RuntimeOpsVTable>(),
        align_of::<RuntimeOpsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<GpuContextLimitedAccessVTable>(),
        align_of::<GpuContextLimitedAccessVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<SurfaceStoreVTable>(),
        align_of::<SurfaceStoreVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<GpuContextFullAccessVTable>(),
        align_of::<GpuContextFullAccessVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<TextureRingMethodsVTable>(),
        align_of::<TextureRingMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<VulkanComputeKernelMethodsVTable>(),
        align_of::<VulkanComputeKernelMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<VulkanGraphicsKernelMethodsVTable>(),
        align_of::<VulkanGraphicsKernelMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<VulkanRayTracingKernelMethodsVTable>(),
        align_of::<VulkanRayTracingKernelMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<VulkanAccelerationStructureMethodsVTable>(),
        align_of::<VulkanAccelerationStructureMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<RhiColorConverterMethodsVTable>(),
        align_of::<RhiColorConverterMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<RhiCommandRecorderMethodsVTable>(),
        align_of::<RhiCommandRecorderMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<OutputWriterVTable>(),
        align_of::<OutputWriterVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<InputMailboxesVTable>(),
        align_of::<InputMailboxesVTable>(),
    );
    // v11/v15 (M32 #1253) — measured layout of the five new surface
    // methods vtables (each dereferenced field-by-field).
    h = fingerprint_fold_layout(
        h,
        size_of::<PresentTargetMethodsVTable>(),
        align_of::<PresentTargetMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<VideoEncoderSessionMethodsVTable>(),
        align_of::<VideoEncoderSessionMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<VideoDecoderSessionMethodsVTable>(),
        align_of::<VideoDecoderSessionMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<HostTimelineSemaphoreMethodsVTable>(),
        align_of::<HostTimelineSemaphoreMethodsVTable>(),
    );
    h = fingerprint_fold_layout(
        h,
        size_of::<VulkanTextureReadbackMethodsVTable>(),
        align_of::<VulkanTextureReadbackMethodsVTable>(),
    );

    h
};

// =============================================================================
// Reserved PUBSUB control topics
// =============================================================================

/// Reserved control topic for [`HostServices::pubsub_publish`]: an
/// engine-free plugin asks the host runtime to shut down (equivalent
/// to SIGINT/SIGTERM). The payload is a msgpack-encoded UTF-8 string —
/// a human-readable reason for shutdown-attribution in host logs
/// (empty string = unspecified). The host never re-publishes on this
/// topic; it maps the request onto its internal runtime-shutdown
/// event. Requests are idempotent and fire-and-forget.
///
/// The string value is wire contract shared by every host and every
/// plugin — a silent rename is a break, so it is locked by a
/// constant-value test in this crate.
pub const PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST: &str =
    "control:runtime-shutdown-request";

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
    /// deserialization is identical regardless of caller plugin.
    ///
    /// Reserved `control:` topics are the exception: their payload is
    /// the per-topic wire shape defined next to their constant, NOT an
    /// `Event` msgpack. The host matches those topics before the
    /// `Event` decode and maps each onto an internal action rather than
    /// re-publishing the bytes. See
    /// [`PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST`].
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
        result_callback: extern "C" fn(userdata: *mut c_void, yaml_ptr: *const u8, yaml_len: usize),
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
    /// cdylib PluginAbiObject; Phase C3 wires the scope-token machinery that
    /// makes the methods reachable from `escalate(|full| ...)` call
    /// sites.
    pub gpu_context_full_access_vtable: *const GpuContextFullAccessVTable,

    // -------------------------------------------------------------------------
    // TextureRingMethodsVTable surface (v7 — issue #907 Phase E PR 1/5)
    // -------------------------------------------------------------------------
    /// Static dispatch table for `TextureRing` PluginAbiObject method
    /// dispatch. Paired with the per-`TextureRing` handle the
    /// cdylib carries on its PluginAbiObject struct (`methods_vtable`
    /// field). Set once at install time; non-null for hosts that
    /// ship a GpuContext, null otherwise (cdylib must check before
    /// dispatching). PR 1 of issue #907 lands the empty-shell
    /// vtable + pointer plumbing; follow-up PRs append the actual
    /// method slots.
    pub texture_ring_methods_vtable: *const TextureRingMethodsVTable,

    // -------------------------------------------------------------------------
    // VulkanComputeKernelMethodsVTable surface (v8 — issue #907 Phase E PR 2/5)
    // -------------------------------------------------------------------------
    /// Static dispatch table for `VulkanComputeKernel` PluginAbiObject
    /// method dispatch. Paired with the per-`VulkanComputeKernel`
    /// handle the cdylib carries on its PluginAbiObject struct
    /// (`methods_vtable` field). Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise (cdylib
    /// must check before dispatching). PR 2 of issue #907 lands the
    /// empty-shell vtable + pointer plumbing; follow-up PRs append
    /// the actual method slots.
    pub vulkan_compute_kernel_methods_vtable: *const VulkanComputeKernelMethodsVTable,

    // -------------------------------------------------------------------------
    // VulkanGraphicsKernelMethodsVTable surface (v9 — issue #907 Phase E PR 3/5)
    // -------------------------------------------------------------------------
    /// Static dispatch table for `VulkanGraphicsKernel` PluginAbiObject
    /// method dispatch. Paired with the per-`VulkanGraphicsKernel`
    /// handle the cdylib carries on its PluginAbiObject struct
    /// (`methods_vtable` field). Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise (cdylib
    /// must check before dispatching). PR 3 of issue #907 lands the
    /// empty-shell vtable + pointer plumbing; follow-up PRs append
    /// the actual method slots.
    pub vulkan_graphics_kernel_methods_vtable: *const VulkanGraphicsKernelMethodsVTable,

    // -------------------------------------------------------------------------
    // VulkanRayTracingKernelMethodsVTable surface (v10 — issue #907 Phase E PR 4/5)
    // -------------------------------------------------------------------------
    /// Static dispatch table for `VulkanRayTracingKernel` PluginAbiObject
    /// method dispatch. Paired with the per-`VulkanRayTracingKernel`
    /// handle the cdylib carries on its PluginAbiObject struct
    /// (`methods_vtable` field). Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise (cdylib
    /// must check before dispatching). PR 4 of issue #907 lands the
    /// empty-shell vtable + pointer plumbing; follow-up PRs append
    /// the actual method slots.
    pub vulkan_ray_tracing_kernel_methods_vtable: *const VulkanRayTracingKernelMethodsVTable,

    // -------------------------------------------------------------------------
    // VulkanAccelerationStructureMethodsVTable surface (v11 — issue #907 Phase E PR 5/5)
    // -------------------------------------------------------------------------
    /// Static dispatch table for `VulkanAccelerationStructure`
    /// PluginAbiObject method dispatch. Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise. PR 5 of
    /// issue #907 lands the empty-shell vtable + pointer plumbing;
    /// follow-up PRs append the actual method slots.
    pub vulkan_acceleration_structure_methods_vtable:
        *const VulkanAccelerationStructureMethodsVTable,

    // -------------------------------------------------------------------------
    // RhiColorConverterMethodsVTable surface (v12 — Phase E sub-lift slice A)
    // -------------------------------------------------------------------------
    /// Static dispatch table for `RhiColorConverter` PluginAbiObject method
    /// dispatch. Paired with the per-`RhiColorConverter` handle the
    /// cdylib carries on its PluginAbiObject struct (`methods_vtable` field).
    /// Set once at install time; non-null for hosts that ship a
    /// GpuContext, null otherwise (cdylib must check before
    /// dispatching). Phase E sub-lift slice A lands the
    /// `prepare_buffer_to_image_storage` slot so cdylib camera
    /// processors can prepare color-conversion kernels without
    /// tripping the PluginAbiObject's host-mode-only `host_inner()` panic.
    pub rhi_color_converter_methods_vtable: *const RhiColorConverterMethodsVTable,

    // -------------------------------------------------------------------------
    // RhiCommandRecorderMethodsVTable surface (v13 — Phase E sub-lift slice B)
    // -------------------------------------------------------------------------
    /// Static dispatch table for `RhiCommandRecorder` PluginAbiObject
    /// method dispatch. Paired with the per-`RhiCommandRecorder`
    /// handle the cdylib carries on its PluginAbiObject struct
    /// (`methods_vtable` field). Set once at install time; non-null
    /// for hosts that ship a GpuContext, null otherwise (cdylib
    /// must check before dispatching). Phase E sub-lift slice B
    /// lands six camera-hot-path slots (`begin`,
    /// `record_image_barrier`, `record_buffer_barrier`,
    /// `record_dispatch`, `record_copy_image_to_buffer`,
    /// `submit_signaling_timeline`) so cdylib camera processors
    /// can drive the host-owned recorder per frame without
    /// tripping the PluginAbiObject's host-mode-only `host_inner_mut()`
    /// panic.
    pub rhi_command_recorder_methods_vtable: *const RhiCommandRecorderMethodsVTable,

    // -------------------------------------------------------------------------
    // OutputWriterVTable + InputMailboxesVTable references (v14 — issue #894)
    // -------------------------------------------------------------------------
    /// Static dispatch table for the cdylib's `OutputWriter` PluginAbiObject
    /// method dispatch. Paired with the per-instance opaque handle
    /// the cdylib stores on its `outputs` field after the host
    /// invokes `ProcessorVTable::set_iceoryx2_resources`. Non-null
    /// for every host that wires processors with output ports;
    /// hosts that strictly don't ship the iceoryx2 transport can
    /// leave it null and the cdylib will treat
    /// `set_iceoryx2_resources` as a no-op for outputs.
    pub output_writer_vtable: *const OutputWriterVTable,

    /// Static dispatch table for the cdylib's `InputMailboxes`
    /// PluginAbiObject method dispatch. Paired with the per-instance opaque
    /// handle the cdylib stores on its `inputs` field after the host
    /// invokes `ProcessorVTable::set_iceoryx2_resources`. Non-null
    /// for every host that wires processors with input ports.
    pub input_mailboxes_vtable: *const InputMailboxesVTable,

    // -------------------------------------------------------------------------
    // M32 one-shot slot reservation (v15 — #1253)
    // -------------------------------------------------------------------------
    /// Static dispatch table for the `PresentTarget` PluginAbiObject's
    /// method dispatch (#1258). Set once at install time; non-null for
    /// hosts that ship a GpuContext, null otherwise (cdylib must check
    /// before dispatching).
    pub present_target_methods_vtable: *const PresentTargetMethodsVTable,

    /// Static dispatch table for the `VideoEncoderSession`
    /// PluginAbiObject's method dispatch (#1259). Set once at install
    /// time; non-null for hosts that ship a GpuContext, null otherwise.
    pub video_encoder_session_methods_vtable: *const VideoEncoderSessionMethodsVTable,

    /// Static dispatch table for the `VideoDecoderSession`
    /// PluginAbiObject's method dispatch (#1259). Set once at install
    /// time; non-null for hosts that ship a GpuContext, null otherwise.
    pub video_decoder_session_methods_vtable: *const VideoDecoderSessionMethodsVTable,

    /// Static dispatch table for the `HostTimelineSemaphore`
    /// PluginAbiObject's method dispatch (#1260). Set once at install
    /// time; non-null for hosts that ship a GpuContext, null otherwise.
    pub host_timeline_semaphore_methods_vtable: *const HostTimelineSemaphoreMethodsVTable,

    /// Static dispatch table for the `TextureReadback` PluginAbiObject's
    /// method dispatch (#1261). Set once at install time; non-null for
    /// hosts that ship a GpuContext, null otherwise.
    pub vulkan_texture_readback_methods_vtable: *const VulkanTextureReadbackMethodsVTable,
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

/// Plugin declaration exported by dynamic libraries.
///
/// Plugins export a static named `STREAMLIB_PLUGIN` of this type via
/// [`export_plugin!`]. The host's loader looks up the symbol,
/// validates `abi_version` (offset 0, read first), then the build
/// fingerprints, then invokes `register`.
///
/// # Layout discipline
///
/// `abi_version` is pinned at offset 0 and `register` at offset 8
/// forever. The host reads `abi_version` before dereferencing the
/// appended v5 fields, so those fields are only ever read from a v5
/// declaration whose byte shape matches.
#[repr(C)]
pub struct PluginDeclaration {
    /// Wire ABI version — must equal [`STREAMLIB_ABI_VERSION`] at
    /// load time. Pinned at offset 0; read first.
    pub abi_version: u32,

    /// Reserved padding (keeps `register` naturally aligned; zero
    /// today, never read).
    pub _reserved_padding: u32,

    /// Register callback. Receives the host-services pointer; the
    /// cdylib's macro expansion uses it to install every per-plugin
    /// static's forwarder before registering processors. Pinned at
    /// offset 8.
    pub register: PluginRegisterFn,

    /// Structural fingerprint of the plugin's `#[repr(C)]` plugin-ABI
    /// dispatch surface — the plugin's [`PLUGIN_ABI_LAYOUT_FINGERPRINT`]
    /// at build time. The host refuses the load unless it equals its
    /// own.
    pub abi_layout_fingerprint: u64,

    /// Structural fingerprint of the engine-internal (non-`#[repr(C)]`)
    /// transit surface the plugin's statically-linked engine copy
    /// exposes across the FullAccess vtable's raw-`Arc` slots. `0` for
    /// an engine-free plugin (no transit surface); otherwise the
    /// host refuses the load unless it equals its own engine's value.
    pub engine_transit_fingerprint: u64,

    /// Pointer to the plugin's human-readable build-identity string
    /// (engine version / rustc version / target triple / profile).
    /// Read defensively by the host only on the error path. Never
    /// null for an [`export_plugin!`]-emitted declaration.
    pub build_identity_ptr: *const u8,

    /// Byte length of the [`Self::build_identity_ptr`] string.
    pub build_identity_len: usize,
}

// Safety: primitives, a function pointer, and a `*const u8` that
// points at a `'static` string owned by the plugin cdylib (kept
// alive for the process lifetime via `LOADED_PLUGIN_LIBRARIES`).
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
    ($first:ty $(, $rest:ty)* $(,)?) => {
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
            // a panic in install / register is converted to silent return.
            // The host's post-call "processor not registered" check
            // surfaces a clear configuration error in that case.
            let _ = ::std::panic::catch_unwind(|| {
                // SDK-path resolution is centralized in the `#[processor]`
                // macro: it generates `__streamlib_install_host_services` /
                // `__streamlib_register` on each Processor against the
                // consumer's real SDK crate (auto-detected — no `streamlib`
                // aliasing). `export_plugin!` names no SDK path itself, so a
                // plugin built against `streamlib-plugin-sdk` and one built
                // against the `streamlib` facade both work unchanged.
                //
                // SAFETY: forwarded per the [`PluginRegisterFn`] contract.
                // install runs once (on the first processor — it is
                // processor-agnostic); every processor registers via the
                // returned helper.
                let helper = unsafe {
                    <$first>::__streamlib_install_host_services(host_services)
                };
                let ::core::option::Option::Some(helper) = helper else {
                    return;
                };
                <$first>::__streamlib_register(&helper);
                $(
                    <$rest>::__streamlib_register(&helper);
                )*
            });
        }

        #[unsafe(no_mangle)]
        pub static STREAMLIB_PLUGIN: $crate::PluginDeclaration = $crate::PluginDeclaration {
            abi_version: $crate::STREAMLIB_ABI_VERSION,
            _reserved_padding: 0,
            register: __streamlib_plugin_register,
            // The `#[processor]` macro emits these three associated
            // consts against the plugin's detected SDK crate — the
            // facade `streamlib` (statically-linked engine → real
            // transit fingerprint) or the engine-free
            // `streamlib-plugin-sdk` (transit fingerprint 0). The
            // envelope names no SDK path itself, matching how
            // `register` is resolved.
            abi_layout_fingerprint: <$first>::__STREAMLIB_ABI_LAYOUT_FINGERPRINT,
            engine_transit_fingerprint: <$first>::__STREAMLIB_ENGINE_TRANSIT_FINGERPRINT,
            build_identity_ptr: <$first>::__STREAMLIB_BUILD_IDENTITY.as_ptr(),
            build_identity_len: <$first>::__STREAMLIB_BUILD_IDENTITY.len(),
        };
    };
}

// ==============================================================================
// Layout regression tests
// ==============================================================================
//
// Crate-root tests cover items that live at the crate root — `HostServices`,
// `PluginDeclaration`, the cross-vtable layout-version pin, plus a Send/Sync
// compile-time witness for every vtable type. Per-struct layout regressions
// for each vtable / repr live in their owning submodule's `mod tests`.

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn plugin_declaration_layout() {
        // v5 envelope: u32 abi_version + u32 padding + fn ptr +
        // two u64 fingerprints + (*const u8, usize) identity slice
        // = 48 bytes, align 8.
        assert_eq!(size_of::<PluginDeclaration>(), 48);
        assert_eq!(align_of::<PluginDeclaration>(), 8);
        // `abi_version` @0 and `register` @8 are pinned forever — the
        // host reads `abi_version` before dereferencing any appended
        // field.
        assert_eq!(offset_of!(PluginDeclaration, abi_version), 0);
        assert_eq!(offset_of!(PluginDeclaration, _reserved_padding), 4);
        assert_eq!(offset_of!(PluginDeclaration, register), 8);
        assert_eq!(offset_of!(PluginDeclaration, abi_layout_fingerprint), 16);
        assert_eq!(
            offset_of!(PluginDeclaration, engine_transit_fingerprint),
            24
        );
        assert_eq!(offset_of!(PluginDeclaration, build_identity_ptr), 32);
        assert_eq!(offset_of!(PluginDeclaration, build_identity_len), 40);
    }

    #[test]
    fn plugin_abi_layout_fingerprint_is_nonzero_and_deterministic() {
        // A degenerate all-zero fold would silently accept every
        // mismatched plugin; assert it's a real value.
        assert_ne!(PLUGIN_ABI_LAYOUT_FINGERPRINT, 0);
        // Recomputing the fold over the same inputs is stable within
        // a single build — the const is evaluated once, but the FNV
        // helpers are pure, so folding the abi version twice agrees.
        let a = fingerprint_fold_u64(fingerprint_init(), STREAMLIB_ABI_VERSION as u64);
        let b = fingerprint_fold_u64(fingerprint_init(), STREAMLIB_ABI_VERSION as u64);
        assert_eq!(a, b);
        // Distinct inputs fold to distinct accumulators (the fold is
        // not a constant function).
        assert_ne!(
            fingerprint_fold_u64(fingerprint_init(), 4),
            fingerprint_fold_u64(fingerprint_init(), 5)
        );
    }

    #[test]
    fn host_services_layout_versions_pinned() {
        // v15: M32 one-shot slot reservation (#1253) appends five
        // per-type methods-vtable pointers.
        assert_eq!(HOST_SERVICES_LAYOUT_VERSION, 15);
        // v5: PluginDeclaration grew the build-fingerprint handshake.
        assert_eq!(STREAMLIB_ABI_VERSION, 5);
        // v2: shared-Rust-type iceoryx2 slots replaced by
        // `set_iceoryx2_resources` (issue #894).
        assert_eq!(PROCESSOR_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, 1);
        // v2: added owning-Arc handle lifetime callbacks
        // (`clone_handle` / `drop_handle`).
        assert_eq!(RUNTIME_OPS_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION, 14);
        // SurfaceStore stays at v1 for the entire M32 milestone — #1260
        // and #1262 both re-bless existing slots (no new SurfaceStore
        // slot).
        assert_eq!(SURFACE_STORE_VTABLE_LAYOUT_VERSION, 1);
        // v11: M32 one-shot slot reservation (#1253).
        assert_eq!(GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION, 11);
        assert_eq!(TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION, 2);
        assert_eq!(VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION, 5);
        assert_eq!(VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION, 4);
        assert_eq!(VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION, 3);
        assert_eq!(
            VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION,
            2
        );
        assert_eq!(RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION, 2);
        // v2: appended PixelBuffer-flavored sibling slots
        // (`record_pixel_buffer_barrier`,
        // `record_copy_image_to_pixel_buffer`) for cdylib camera
        // per-frame copy into pooled `PixelBuffer` destinations.
        assert_eq!(RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION, 5);
        // v1 (issue #894): initial shape — `write_raw`, `has_port`,
        // `clone_arc`, `drop_arc`.
        assert_eq!(OUTPUT_WRITER_VTABLE_LAYOUT_VERSION, 1);
        // v2 (#1097 audio-mixer-demo silent-output fix): appends
        // `max_payload_for_port` so cdylib `read_raw` allocates
        // exactly the schema-declared `metadata.max_payload_bytes`
        // and never truncates. Slots: `read_raw`, `has_data`,
        // `clone_arc`, `drop_arc`, `max_payload_for_port`.
        assert_eq!(INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION, 2);
        // v11/v15 (M32 #1253) — the five new surface methods vtables all
        // mint at layout version 1.
        assert_eq!(PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION, 1);
    }

    #[test]
    fn host_services_repr_layout() {
        // 31 fields total: 2 u32 header + 1 host handle + 8 leading
        // extern "C" fn callbacks + 20 trailing vtable pointers (15
        // pre-v15 + 5 M32 #1253).
        // Total = 4 + 4 + 8 + 8*8 + 20*8 = 240 bytes, align = 8.
        assert_eq!(size_of::<HostServices>(), 240);
        assert_eq!(align_of::<HostServices>(), 8);

        // Header.
        assert_eq!(offset_of!(HostServices, abi_layout_version), 0);
        assert_eq!(offset_of!(HostServices, _reserved_padding), 4);
        assert_eq!(offset_of!(HostServices, host), 8);

        // Leading extern "C" fn callbacks.
        assert_eq!(offset_of!(HostServices, tracing_register_callsite), 16);
        assert_eq!(offset_of!(HostServices, tracing_enabled), 24);
        assert_eq!(offset_of!(HostServices, tracing_emit), 32);
        assert_eq!(offset_of!(HostServices, pubsub_publish), 40);
        assert_eq!(offset_of!(HostServices, schema_register), 48);
        assert_eq!(offset_of!(HostServices, schema_lookup), 56);
        assert_eq!(offset_of!(HostServices, iceoryx_log_emit), 64);
        assert_eq!(offset_of!(HostServices, processor_register), 72);

        // Trailing vtable pointers. Each is a single 8-byte pointer,
        // contiguous, terminating exactly at the end of the struct.
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
        assert_eq!(size_of::<*const PresentTargetMethodsVTable>(), 8);
        assert_eq!(size_of::<*const VideoEncoderSessionMethodsVTable>(), 8);
        assert_eq!(size_of::<*const VideoDecoderSessionMethodsVTable>(), 8);
        assert_eq!(size_of::<*const HostTimelineSemaphoreMethodsVTable>(), 8);
        assert_eq!(size_of::<*const VulkanTextureReadbackMethodsVTable>(), 8);

        assert_eq!(offset_of!(HostServices, runtime_context_vtable), 80);
        assert_eq!(offset_of!(HostServices, audio_clock_vtable), 88);
        assert_eq!(offset_of!(HostServices, runtime_ops_vtable), 96);
        assert_eq!(
            offset_of!(HostServices, gpu_context_limited_access_vtable),
            104
        );
        assert_eq!(offset_of!(HostServices, surface_store_vtable), 112);
        assert_eq!(
            offset_of!(HostServices, gpu_context_full_access_vtable),
            120
        );
        assert_eq!(offset_of!(HostServices, texture_ring_methods_vtable), 128);
        assert_eq!(
            offset_of!(HostServices, vulkan_compute_kernel_methods_vtable),
            136
        );
        assert_eq!(
            offset_of!(HostServices, vulkan_graphics_kernel_methods_vtable),
            144
        );
        assert_eq!(
            offset_of!(HostServices, vulkan_ray_tracing_kernel_methods_vtable),
            152
        );
        assert_eq!(
            offset_of!(HostServices, vulkan_acceleration_structure_methods_vtable),
            160
        );
        assert_eq!(
            offset_of!(HostServices, rhi_color_converter_methods_vtable),
            168
        );
        assert_eq!(
            offset_of!(HostServices, rhi_command_recorder_methods_vtable),
            176
        );
        assert_eq!(offset_of!(HostServices, output_writer_vtable), 184);
        assert_eq!(offset_of!(HostServices, input_mailboxes_vtable), 192);
        // v15 (M32 #1253) — five appended per-type methods-vtable
        // pointers.
        assert_eq!(offset_of!(HostServices, present_target_methods_vtable), 200);
        assert_eq!(
            offset_of!(HostServices, video_encoder_session_methods_vtable),
            208
        );
        assert_eq!(
            offset_of!(HostServices, video_decoder_session_methods_vtable),
            216
        );
        assert_eq!(
            offset_of!(HostServices, host_timeline_semaphore_methods_vtable),
            224
        );
        assert_eq!(
            offset_of!(HostServices, vulkan_texture_readback_methods_vtable),
            232
        );
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
        assert_send_sync::<PresentTargetMethodsVTable>();
        assert_send_sync::<VideoEncoderSessionMethodsVTable>();
        assert_send_sync::<VideoDecoderSessionMethodsVTable>();
        assert_send_sync::<HostTimelineSemaphoreMethodsVTable>();
        assert_send_sync::<VulkanTextureReadbackMethodsVTable>();
        assert_send_sync::<HostServices>();
        assert_send_sync::<ProcessorVTable>();
    }
}

#[cfg(test)]
mod control_topic_tests {
    use super::*;

    /// The reserved control-topic string is wire contract shared by
    /// every host and every already-built plugin: the host matches on
    /// this exact literal in `host_pubsub_publish`, and the SDK's
    /// `request_runtime_shutdown` publishes to it. A silent rename
    /// would make the host warn-and-drop the request and the runtime
    /// would hang, so the value is locked here.
    #[test]
    fn runtime_shutdown_request_control_topic_value_is_locked() {
        assert_eq!(
            PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST,
            "control:runtime-shutdown-request"
        );
    }
}
