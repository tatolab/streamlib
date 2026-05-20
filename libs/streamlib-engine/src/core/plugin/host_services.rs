// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-DSO host-services callback table.
//!
//! Companion to `streamlib-plugin-abi`'s [`HostServices`] ABI
//! contract. This module owns:
//!
//! - **Host-side callback impls** (`host_tracing_emit`,
//!   `host_pubsub_publish`, `host_schema_register`,
//!   `host_schema_lookup`, `host_iceoryx_log_emit`,
//!   `host_processor_register`) that the host's loader writes into a
//!   [`HostServices`] struct before invoking a cdylib's
//!   `STREAMLIB_PLUGIN.register` callback.
//! - **Cdylib-side `install_host_services` helper** that the cdylib's
//!   `export_plugin!` macro calls at register time. The helper
//!   validates layout, stores the callback table in a per-DSO
//!   [`HOST_CALLBACKS`] static, caches the host's tokio handle in
//!   [`HOST_TOKIO_HANDLE`] for cdylib-side async-lifecycle wrappers,
//!   installs the cdylib's tracing `ForwardingSubscriber` and
//!   iceoryx2 `Log` forwarder, and returns a [`RegisterHelper`] for
//!   the macro to register processors with.
//!
//! # Why this shape
//!
//! Rust mangled statics aren't in the dynsym table — every linked
//! copy of streamlib-engine (host binary, every dlopen'd cdylib) has
//! its own [`PUBSUB`], its own schema registry, its own
//! `tracing-core::GLOBAL_DISPATCH`, its own `iceoryx2_log::LOGGER`.
//! Passing `&'static T` references across the FFI would couple
//! every consumer to byte-identical type layouts across DSOs,
//! breaking streamlib's multi-builder deployment model.
//!
//! The callback-table shape removes that coupling: only `extern "C"
//! fn` signatures and primitive payloads cross the wire. The cdylib's
//! statically-linked engine copy keeps its own statics, but the read
//! paths through them (`PUBSUB.publish`, `register_schema`,
//! `get_embedded_schema_definition`, `tracing::*!`,
//! `iceoryx2_log::*`) route through the host's fn pointers instead
//! of through the local DSO's state.
//!
//! Processor registration follows the same shape: cdylib's
//! `RegisterHelper::register::<P>()` monomorphizes a [`ProcessorVTable`]
//! per processor type P and calls the host's `processor_register`
//! callback with the descriptor msgpack + vtable. The host's factory
//! stores `(descriptor, &'static ProcessorVTable)` and dispatches
//! every host-called method through extern "C" — retiring the
//! `Box<dyn DynGeneratedProcessor>` dyn-trait crossing class.
//!
//! # Deployment model this enables
//!
//! Computer A builds the host binary, computer B builds packages via
//! CI, computer C ships their own packages — all using different
//! rustc minor versions and different transitive-dep resolutions —
//! interoperate as long as they target the same triple and link the
//! same [`streamlib_plugin_abi::STREAMLIB_ABI_VERSION`]. No
//! commit-level coupling, no shared Cargo.lock.

use std::ffi::c_void;
use std::sync::OnceLock;

use streamlib_plugin_abi::{
    HostHandle, HostInterest, HostLogLevel, HostServices, ProcessorVTable,
    HOST_SERVICES_LAYOUT_VERSION, PROCESSOR_VTABLE_LAYOUT_VERSION,
};

// Note on tokio: cdylib-side async-lifecycle wrappers grab the tokio
// handle from `ctx.tokio_handle()` rather than going through an
// extern "C" callback. Tokio's `Handle` layout is host/cdylib-shared
// via the workspace-pinned tokio version — one of the known
// shared-type crossings Phase A leaves in place by design (Phase B
// addresses RuntimeContext crossings if multi-builder tokio drift
// becomes a real concern).

use crate::core::pubsub::Event;

// =============================================================================
// HostCallbacks — per-DSO cache of the host's fn pointers
// =============================================================================

/// Cached copy of the host's callback table, stored in
/// [`HOST_CALLBACKS`] by `install_host_services` so the cdylib's
/// PUBSUB / schema-registry / tracing / iceoryx2-log forwarders can
/// reach the host without indirecting through [`HostServices`] on
/// every call.
#[derive(Clone, Copy)]
pub struct HostCallbacks {
    pub host: HostHandle,
    pub tracing_register_callsite: unsafe extern "C" fn(
        host: HostHandle,
        target_ptr: *const u8,
        target_len: usize,
        level: HostLogLevel,
    ) -> HostInterest,
    pub tracing_enabled: unsafe extern "C" fn(
        host: HostHandle,
        target_ptr: *const u8,
        target_len: usize,
        level: HostLogLevel,
    ) -> bool,
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
    pub pubsub_publish: unsafe extern "C" fn(
        host: HostHandle,
        topic_ptr: *const u8,
        topic_len: usize,
        event_msgpack_ptr: *const u8,
        event_msgpack_len: usize,
    ),
    pub schema_register: unsafe extern "C" fn(
        host: HostHandle,
        canonical_id_ptr: *const u8,
        canonical_id_len: usize,
        yaml_ptr: *const u8,
        yaml_len: usize,
    ),
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
    pub iceoryx_log_emit: unsafe extern "C" fn(
        host: HostHandle,
        level: HostLogLevel,
        origin_ptr: *const u8,
        origin_len: usize,
        message_ptr: *const u8,
        message_len: usize,
    ),
    pub processor_register: unsafe extern "C" fn(
        host: HostHandle,
        descriptor_msgpack_ptr: *const u8,
        descriptor_msgpack_len: usize,
        vtable: *const ProcessorVTable,
    ) -> i32,
}

// Safety: every field is a fn pointer or a raw pointer the host
// promises stays valid for the cdylib's process lifetime.
unsafe impl Send for HostCallbacks {}
unsafe impl Sync for HostCallbacks {}

/// Per-DSO cache of the host's callback table. `OnceLock` semantics:
/// the cdylib's `install_host_services` writes once at register
/// time; subsequent reads from `PUBSUB.publish`, `register_schema`,
/// the tracing `ForwardingSubscriber`, and the iceoryx2 forwarder
/// retrieve the same value. **The host's DSO never populates this**
/// — host-side code reads its local statics directly, bypassing the
/// callback table.
static HOST_CALLBACKS: OnceLock<HostCallbacks> = OnceLock::new();

/// Returns this DSO's callback table if a cdylib's
/// `install_host_services` has populated it. `None` in the host
/// binary; `Some(_)` in any cdylib that has registered.
pub fn host_callbacks() -> Option<&'static HostCallbacks> {
    HOST_CALLBACKS.get()
}

// =============================================================================
// install_host_services — cdylib entry point
// =============================================================================

/// Wire the host's services into this DSO. Called by a plugin
/// cdylib's `STREAMLIB_PLUGIN.register` callback via the
/// [`streamlib_plugin_abi::export_plugin!`] macro.
///
/// Validates [`HostServices::abi_layout_version`] against
/// [`HOST_SERVICES_LAYOUT_VERSION`], stores the callback table in
/// [`HOST_CALLBACKS`], installs the cdylib's tracing
/// [`ForwardingSubscriber`] as the per-DSO `GLOBAL_DISPATCH`,
/// installs the cdylib's iceoryx2 `Log` forwarder, and returns a
/// [`RegisterHelper`] the macro uses to register processor types
/// with the host's registry.
///
/// # Returns
///
/// `Some(RegisterHelper)` on success. `None` on layout-version
/// mismatch or null pointer — the macro short-circuits processor
/// registration, and the host's post-call "processor not registered"
/// check surfaces a `Configuration` error.
///
/// # Safety
///
/// `host_services_ptr` must point at a [`HostServices`] value
/// initialized by the host. The host's loader guarantees this.
pub unsafe fn install_host_services(
    host_services_ptr: *const c_void,
) -> Option<RegisterHelper> {
    if host_services_ptr.is_null() {
        return None;
    }

    // SAFETY: per the caller's promise. Read `abi_layout_version`
    // before touching any other field — if the layout doesn't match,
    // the rest of the struct's shape may have drifted.
    let services = unsafe { &*(host_services_ptr as *const HostServices) };

    if services.abi_layout_version != HOST_SERVICES_LAYOUT_VERSION {
        // Logging hasn't been wired yet (the forwarder install is
        // below); the host detects the failure via the post-call
        // "processor not registered" check.
        return None;
    }

    let callbacks = HostCallbacks {
        host: services.host,
        tracing_register_callsite: services.tracing_register_callsite,
        tracing_enabled: services.tracing_enabled,
        tracing_emit: services.tracing_emit,
        pubsub_publish: services.pubsub_publish,
        schema_register: services.schema_register,
        schema_lookup: services.schema_lookup,
        iceoryx_log_emit: services.iceoryx_log_emit,
        processor_register: services.processor_register,
    };

    // Cache the callbacks BEFORE installing tracing — the
    // `ForwardingSubscriber` reads `HOST_CALLBACKS` on every emit.
    let _ = HOST_CALLBACKS.set(callbacks);

    // Install the tracing forwarder as the cdylib's global dispatcher.
    // The cdylib's `tracing::*!()` macros now route every event
    // through the host's `tracing_emit` callback.
    crate::core::plugin::forwarding_subscriber::install_for_self();

    // Install the iceoryx2 log forwarder. The cdylib's iceoryx2-log
    // emits route through the host's `iceoryx_log_emit` callback.
    // Also raise the cdylib's iceoryx2-log level to Trace so the
    // host's filter sees every record; the host then decides via
    // its tracing pipeline what to actually emit.
    crate::core::plugin::iceoryx2_log_forwarder::install_for_self();

    Some(RegisterHelper {})
}

/// Helper handed back to the cdylib's `export_plugin!` macro for
/// registering processors with the host's registry. Source-compatible
/// with v1's `helper.register::<P>()` call shape — the implementation
/// now monomorphizes a [`ProcessorVTable`] per processor type and
/// routes through the host's `processor_register` callback instead
/// of dispatching through `&'static ProcessorInstanceFactory`.
pub struct RegisterHelper {}

impl RegisterHelper {
    /// Register a processor type with the host's registry.
    ///
    /// Builds the static per-P [`ProcessorVTable`], serializes
    /// `P::descriptor()` to msgpack, and calls the host's
    /// `processor_register` callback. Source-compatible at the call
    /// site (`helper.register::<P::Processor>()`).
    pub fn register<P>(&self)
    where
        P: crate::core::processors::GeneratedProcessor + 'static,
        P::Config: crate::core::processors::Config,
    {
        // Resolve the host's callback table. In a cdylib this was
        // populated by `install_host_services` above. In the host
        // process (where this code path also runs when a processor
        // is registered inline via `PROCESSOR_REGISTRY.register::<P>()`),
        // `HOST_CALLBACKS` is empty — the host-static path bypasses
        // FFI and registers directly with the factory.
        if let Some(callbacks) = host_callbacks() {
            register_via_callback::<P>(callbacks);
        } else {
            // Host-static path: same vtable shape, but registered
            // directly with the in-process factory (no FFI hop).
            crate::core::processors::PROCESSOR_REGISTRY.register::<P>();
        }
    }
}

/// Cdylib-side registration: build a vtable + descriptor msgpack and
/// call the host's `processor_register` callback.
fn register_via_callback<P>(callbacks: &HostCallbacks)
where
    P: crate::core::processors::GeneratedProcessor + 'static,
    P::Config: crate::core::processors::Config,
{
    let descriptor = match <P as crate::core::processors::GeneratedProcessor>::descriptor() {
        Some(d) => d,
        None => {
            tracing::warn!(
                "Processor {} has no descriptor, skipping registration",
                std::any::type_name::<P>()
            );
            return;
        }
    };

    let descriptor_msgpack = match rmp_serde::to_vec_named(&descriptor) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(
                "Failed to serialize descriptor for {}: {}",
                std::any::type_name::<P>(),
                e
            );
            return;
        }
    };

    let vtable = crate::core::plugin::processor_vtable::vtable_for::<P>();

    // SAFETY: msgpack bytes and vtable pointer live in this DSO's
    // process address space for the duration of the call. The host's
    // implementation copies any data it needs to retain (the
    // descriptor is decoded into a `ProcessorDescriptor`; the vtable
    // pointer is stored as-is and the cdylib is pinned via
    // `LOADED_PLUGIN_LIBRARIES`).
    let rc = unsafe {
        (callbacks.processor_register)(
            callbacks.host,
            descriptor_msgpack.as_ptr(),
            descriptor_msgpack.len(),
            vtable as *const ProcessorVTable,
        )
    };

    if rc != 0 {
        tracing::warn!(
            "processor_register for {} returned non-zero rc={}",
            descriptor.name,
            rc
        );
    }
}

// =============================================================================
// Host-side callback implementations
// =============================================================================

/// Concrete host-side service table the host's loader plugs into a
/// [`HostServices`] payload via [`runtime_facing::host_services_for_self`].
///
/// Holds the host's iceoryx2 node. Lives behind the
/// [`HostServices::host`] opaque pointer.
pub struct HostServiceImpls {
    pub iceoryx2_node: crate::iceoryx2::Iceoryx2Node,
}

unsafe impl Send for HostServiceImpls {}
unsafe impl Sync for HostServiceImpls {}

unsafe extern "C" fn host_tracing_register_callsite(
    _host: HostHandle,
    _target_ptr: *const u8,
    _target_len: usize,
    _level: HostLogLevel,
) -> HostInterest {
    // The host's `EnvFilter` filters at emit time via `host_tracing_emit`
    // (it calls `tracing::event!` which fires through the host's
    // subscriber chain). Returning `Always` here tells the cdylib's
    // forwarding `Subscriber` to cache "always emit" for the
    // callsite — every event reaches `host_tracing_emit`, where the
    // host's filter actually decides.
    //
    // Trade-off: cdylib pays for the FFI hop even on filtered-out
    // events, plus a string copy of the message. A future
    // refinement could push a (target, level)-keyed pre-filter
    // here; the current ABI shape doesn't constrain that.
    HostInterest::Always
}

unsafe extern "C" fn host_tracing_enabled(
    _host: HostHandle,
    _target_ptr: *const u8,
    _target_len: usize,
    _level: HostLogLevel,
) -> bool {
    // Paired with `host_tracing_register_callsite` returning
    // `Always`: this never fires from the cdylib side. Kept in the
    // ABI so a future register_callsite that returns `Sometimes`
    // has the per-event enable hook available.
    true
}

unsafe extern "C" fn host_tracing_emit(
    _host: HostHandle,
    target_ptr: *const u8,
    target_len: usize,
    level: HostLogLevel,
    message_ptr: *const u8,
    message_len: usize,
    fields_msgpack_ptr: *const u8,
    fields_msgpack_len: usize,
) {
    let target = unsafe {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(target_ptr, target_len))
    };
    let message = if message_len == 0 {
        ""
    } else {
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(message_ptr, message_len))
        }
    };
    let level_val = host_log_level_to_tracing(level);
    let fields_bytes = if fields_msgpack_len == 0 || fields_msgpack_ptr.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(fields_msgpack_ptr, fields_msgpack_len) }
    };

    // Decode the structured fields (msgpack map) and replay them
    // through the host's tracing pipeline alongside `message`. The
    // simplest shape that preserves field fidelity is to log via
    // the host's own subscriber using `event!`-style emission with
    // a single `message` field — structured fields go into the
    // event's value set as serde-derived JSON values, captured by
    // `JsonlSinkLayer::Capture::record_*`.
    let fields_map: serde_json::Value =
        rmp_serde::from_slice(fields_bytes).unwrap_or(serde_json::Value::Null);

    emit_via_host_dispatch(target, level_val, message, &fields_map);
}

unsafe extern "C" fn host_pubsub_publish(
    _host: HostHandle,
    topic_ptr: *const u8,
    topic_len: usize,
    event_msgpack_ptr: *const u8,
    event_msgpack_len: usize,
) {
    let topic = unsafe {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(topic_ptr, topic_len))
    };
    let event_bytes = unsafe { std::slice::from_raw_parts(event_msgpack_ptr, event_msgpack_len) };
    let event: Event = match rmp_serde::from_slice(event_bytes) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                target: "streamlib::plugin",
                "host_pubsub_publish: failed to decode event from cdylib: {e}"
            );
            return;
        }
    };
    crate::core::pubsub::PUBSUB.publish(topic, &event);
}

unsafe extern "C" fn host_schema_register(
    _host: HostHandle,
    canonical_id_ptr: *const u8,
    canonical_id_len: usize,
    yaml_ptr: *const u8,
    yaml_len: usize,
) {
    let canonical_id = unsafe {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(
            canonical_id_ptr,
            canonical_id_len,
        ))
    };
    let yaml =
        unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(yaml_ptr, yaml_len)) };
    crate::core::embedded_schemas::register_schema(canonical_id.to_string(), yaml);
}

unsafe extern "C" fn host_schema_lookup(
    _host: HostHandle,
    canonical_id_ptr: *const u8,
    canonical_id_len: usize,
    result_callback: extern "C" fn(*mut c_void, *const u8, usize),
    result_userdata: *mut c_void,
) {
    let canonical_id = unsafe {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(
            canonical_id_ptr,
            canonical_id_len,
        ))
    };
    match crate::core::embedded_schemas::get_embedded_schema_definition(canonical_id) {
        Some(yaml) => {
            let bytes = yaml.as_bytes();
            result_callback(result_userdata, bytes.as_ptr(), bytes.len());
        }
        None => {
            result_callback(result_userdata, std::ptr::null(), 0);
        }
    }
}

unsafe extern "C" fn host_iceoryx_log_emit(
    _host: HostHandle,
    level: HostLogLevel,
    origin_ptr: *const u8,
    origin_len: usize,
    message_ptr: *const u8,
    message_len: usize,
) {
    let origin = if origin_len == 0 {
        ""
    } else {
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(origin_ptr, origin_len))
        }
    };
    let message = if message_len == 0 {
        ""
    } else {
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(message_ptr, message_len))
        }
    };
    // Forward into the host's tracing pipeline at the appropriate level.
    match level {
        HostLogLevel::Trace => tracing::trace!(target: "iceoryx2", origin = %origin, "{message}"),
        HostLogLevel::Debug => tracing::debug!(target: "iceoryx2", origin = %origin, "{message}"),
        HostLogLevel::Info => tracing::info!(target: "iceoryx2", origin = %origin, "{message}"),
        HostLogLevel::Warn => tracing::warn!(target: "iceoryx2", origin = %origin, "{message}"),
        HostLogLevel::Error => tracing::error!(target: "iceoryx2", origin = %origin, "{message}"),
    }
}

/// Host-side `processor_register` callback. Decodes the descriptor
/// msgpack and routes to the in-process registry's
/// `register_via_vtable` path. Returns 0 on success, non-zero on
/// descriptor decode failure, vtable layout-version mismatch, or
/// duplicate registration.
unsafe extern "C" fn host_processor_register(
    _host: HostHandle,
    descriptor_msgpack_ptr: *const u8,
    descriptor_msgpack_len: usize,
    vtable: *const ProcessorVTable,
) -> i32 {
    if vtable.is_null() {
        tracing::warn!("host_processor_register: null vtable pointer");
        return -1;
    }

    let vtable_layout = unsafe { (*vtable).layout_version };
    if vtable_layout != PROCESSOR_VTABLE_LAYOUT_VERSION {
        tracing::warn!(
            "host_processor_register: vtable layout version mismatch (got {}, expected {})",
            vtable_layout,
            PROCESSOR_VTABLE_LAYOUT_VERSION
        );
        return -2;
    }

    let descriptor_bytes =
        unsafe { std::slice::from_raw_parts(descriptor_msgpack_ptr, descriptor_msgpack_len) };
    let descriptor: crate::core::descriptors::ProcessorDescriptor =
        match rmp_serde::from_slice(descriptor_bytes) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    "host_processor_register: failed to decode descriptor msgpack: {e}"
                );
                return -3;
            }
        };

    // SAFETY: `vtable` is `&'static ProcessorVTable` on the cdylib
    // side; the cdylib is pinned via `LOADED_PLUGIN_LIBRARIES`, so
    // the pointer outlives the host's usage.
    let vtable_ref: &'static ProcessorVTable = unsafe { &*vtable };

    match crate::core::processors::PROCESSOR_REGISTRY
        .register_via_vtable(descriptor, vtable_ref)
    {
        Ok(()) => 0,
        Err(e) => {
            tracing::warn!("host_processor_register: register_via_vtable failed: {e}");
            -4
        }
    }
}

// =============================================================================
// FFI conversions
// =============================================================================

pub(crate) fn tracing_level_to_host(level: tracing::Level) -> HostLogLevel {
    match level {
        tracing::Level::TRACE => HostLogLevel::Trace,
        tracing::Level::DEBUG => HostLogLevel::Debug,
        tracing::Level::INFO => HostLogLevel::Info,
        tracing::Level::WARN => HostLogLevel::Warn,
        tracing::Level::ERROR => HostLogLevel::Error,
    }
}

pub(crate) fn host_log_level_to_tracing(level: HostLogLevel) -> tracing::Level {
    match level {
        HostLogLevel::Trace => tracing::Level::TRACE,
        HostLogLevel::Debug => tracing::Level::DEBUG,
        HostLogLevel::Info => tracing::Level::INFO,
        HostLogLevel::Warn => tracing::Level::WARN,
        HostLogLevel::Error => tracing::Level::ERROR,
    }
}

pub(crate) fn host_interest_to_tracing(interest: HostInterest) -> tracing::subscriber::Interest {
    match interest {
        HostInterest::Never => tracing::subscriber::Interest::never(),
        HostInterest::Sometimes => tracing::subscriber::Interest::sometimes(),
        HostInterest::Always => tracing::subscriber::Interest::always(),
    }
}

// =============================================================================
// Emit-via-host-dispatch — used by `host_tracing_emit`
// =============================================================================

/// Replay a cdylib-emitted event into the host's JSONL drain
/// pipeline.
///
/// `tracing::event!` macros can't take a runtime `target:` — they
/// expand into a static `Callsite` whose target is baked at compile
/// time. To support arbitrary cdylib targets we bypass tracing and
/// push a [`LogRecord`] directly into the host's drain worker via
/// the same queue the polyglot subprocess log-relay uses, by way of
/// [`crate::core::logging::push_polyglot_record`].
///
/// Trade-off: host-side `EnvFilter` filtering doesn't apply on this
/// path; cdylib code is responsible for its own level filtering
/// (the cdylib's `ForwardingSubscriber::register_callsite` queries
/// `host_tracing_register_callsite` and caches the result). The
/// drain queue is bounded so an over-emitting plugin still
/// drop-oldests gracefully.
fn emit_via_host_dispatch(
    target: &str,
    level: tracing::Level,
    message: &str,
    fields: &serde_json::Value,
) {
    use crate::core::logging::push_polyglot_record;
    use crate::core::logging::LogRecord;

    let attrs = match fields {
        serde_json::Value::Object(map) => {
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        }
        _ => std::collections::BTreeMap::new(),
    };

    let record = LogRecord {
        host_ts: crate::core::logging::now_ns(),
        level: (level).into(),
        target: target.to_string(),
        message: message.to_string(),
        pipeline_id: None,
        processor_id: None,
        rhi_op: None,
        intercepted: false,
        channel: None,
        attrs,
        source: None,
        source_ts: None,
        source_seq: None,
    };

    push_polyglot_record(record);
}

// =============================================================================
// runtime_facing — host-side payload builder
// =============================================================================

/// Host-facing helpers used by `Runner::load_project` (and the
/// `streamlib-runtime` binary's plugin loader) to assemble a
/// [`HostServices`] payload pointing at this DSO's callback
/// implementations.
pub mod runtime_facing {
    use super::{
        host_iceoryx_log_emit, host_processor_register, host_pubsub_publish, host_schema_lookup,
        host_schema_register, host_tracing_emit, host_tracing_enabled,
        host_tracing_register_callsite, HostServiceImpls,
    };
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use streamlib_plugin_abi::{HostServices, HOST_SERVICES_LAYOUT_VERSION};

    /// Heap-allocated service impl table, leaked once per process.
    /// The `HostServices.host` opaque pointer points at this.
    static HOST_IMPLS: OnceLock<&'static HostServiceImpls> = OnceLock::new();

    fn host_impls_for_self(node: &crate::iceoryx2::Iceoryx2Node) -> &'static HostServiceImpls {
        HOST_IMPLS.get_or_init(|| {
            let impls = HostServiceImpls {
                iceoryx2_node: node.clone(),
            };
            Box::leak(Box::new(impls))
        })
    }

    /// Build a [`HostServices`] payload from this process's host
    /// callback impls. Callable repeatedly; the underlying
    /// [`HostServiceImpls`] is constructed once and reused for the
    /// process lifetime, matching `LOADED_PLUGIN_LIBRARIES`'s pinning
    /// lifetime for loaded cdylibs.
    pub fn host_services_for_self(node: &crate::iceoryx2::Iceoryx2Node) -> HostServices {
        let host_impls = host_impls_for_self(node);
        let host_handle = host_impls as *const HostServiceImpls as *const c_void;

        HostServices {
            abi_layout_version: HOST_SERVICES_LAYOUT_VERSION,
            _reserved_padding: 0,
            host: host_handle,
            tracing_register_callsite: host_tracing_register_callsite,
            tracing_enabled: host_tracing_enabled,
            tracing_emit: host_tracing_emit,
            pubsub_publish: host_pubsub_publish,
            schema_register: host_schema_register,
            schema_lookup: host_schema_lookup,
            iceoryx_log_emit: host_iceoryx_log_emit,
            processor_register: host_processor_register,
        }
    }
}
