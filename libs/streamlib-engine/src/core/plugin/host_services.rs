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
use std::sync::{Arc, OnceLock};

use streamlib_plugin_abi::{
    AudioClockVTable, GpuContextLimitedAccessVTable, HostHandle, HostInterest, HostLogLevel,
    HostServices, ProcessorVTable, RuntimeContextVTable, RuntimeOpsVTable,
    AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
    HOST_SERVICES_LAYOUT_VERSION, PROCESSOR_VTABLE_LAYOUT_VERSION,
    RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
};

// Phase B + v3 layout: tokio is no longer exposed across the ABI.
// Lifecycle methods are synchronous at the trait surface; plugins
// that need async lifecycle work bring their own runtime. The host's
// tokio runtime stays invisible to plugins. See
// `streamlib_plugin_abi`'s `HOST_SERVICES_LAYOUT_VERSION` v3 doc.

use crate::core::context::{RuntimeContext, SharedAudioClock};
use crate::core::pubsub::Event;
use crate::core::runtime::RuntimeOperations;

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
    /// v3: host-installed [`RuntimeContextVTable`] pointer. Cached so
    /// the cdylib's shim constructors don't read [`HostServices`] on
    /// every shim build. The cdylib MUST read this from the cache
    /// (or `HostServices` direct) rather than reach for its local
    /// `&HOST_RUNTIME_CONTEXT_VTABLE` static — the local copy's fn
    /// pointers would dispatch into cdylib code instead of host code,
    /// which would break the no-shared-type-crossing invariant.
    pub runtime_context_vtable: *const RuntimeContextVTable,
    /// v3: host-installed [`AudioClockVTable`] pointer. Same rule as
    /// `runtime_context_vtable`.
    pub audio_clock_vtable: *const AudioClockVTable,
    /// v3: host-installed [`RuntimeOpsVTable`] pointer.
    pub runtime_ops_vtable: *const RuntimeOpsVTable,
    /// v4 (Phase C1, #901): host-installed
    /// [`GpuContextLimitedAccessVTable`] pointer. May be null on
    /// hosts that don't ship a GpuContext; cdylib must check before
    /// dispatching.
    pub gpu_context_limited_access_vtable: *const GpuContextLimitedAccessVTable,
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

    // Validate every inner vtable's layout_version before storing the
    // pointers. The outer `abi_layout_version` only covers the wire
    // shape of [`HostServices`] itself; a host that bumped, say, the
    // GpuContextLimitedAccessVTable to v4 but kept HostServices v4
    // would otherwise silently call through mismatched offsets from a
    // v3-built cdylib. Mismatch → refuse the install cleanly; the
    // host's post-call "processor not registered" check surfaces the
    // failure. (Inner vtables are validated only when non-null. The
    // GPU vtable pointer may legitimately be null on hosts that don't
    // ship a GpuContext, per `HOST_SERVICES_LAYOUT_VERSION` v4 docs.)
    use streamlib_plugin_abi::{
        AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
    };
    if !services.runtime_context_vtable.is_null() {
        // SAFETY: per the wire contract, when non-null this points at
        // a `&'static RuntimeContextVTable` owned by the host. The
        // first u32 in the struct is `layout_version` (pinned at
        // offset 0 by the layout-regression tests).
        let v = unsafe { (*services.runtime_context_vtable).layout_version };
        if v != RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.audio_clock_vtable.is_null() {
        // SAFETY: same shape as runtime_context_vtable.
        let v = unsafe { (*services.audio_clock_vtable).layout_version };
        if v != AUDIO_CLOCK_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.runtime_ops_vtable.is_null() {
        // SAFETY: same shape as runtime_context_vtable.
        let v = unsafe { (*services.runtime_ops_vtable).layout_version };
        if v != RUNTIME_OPS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.gpu_context_limited_access_vtable.is_null() {
        // SAFETY: same shape as runtime_context_vtable. Null is
        // allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.gpu_context_limited_access_vtable).layout_version };
        if v != GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION {
            return None;
        }
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
        runtime_context_vtable: services.runtime_context_vtable,
        audio_clock_vtable: services.audio_clock_vtable,
        runtime_ops_vtable: services.runtime_ops_vtable,
        gpu_context_limited_access_vtable: services.gpu_context_limited_access_vtable,
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

// ---------------- Panic safety helpers ----------------
//
// Unwinding through an `extern "C"` boundary is undefined behaviour.
// Every host-side callback below routes its body through
// [`run_host_extern_c`] so a panic in host code is caught and
// converted to a logged error plus a sensible default return value
// at the FFI boundary, instead of corrupting the cdylib's stack.
//
// The default-on-panic value per callback type:
//   - void                  → `()`
//   - bool                  → `false`
//   - u32 / usize           → `0`
//   - isize (used by id_copy with `-1` = None) → `-1`
//   - i32  (status codes; non-zero = error)   → `1`
//   - HostInterest          → `HostInterest::Never`
//   - `*const c_void` / `*mut u8` / `*const ProcessorVTable` → `null` / `null_mut`

/// Run an extern "C" callback body inside [`std::panic::catch_unwind`].
/// Panics are logged and converted to `default_on_panic` so the FFI
/// boundary stays sound. `callback_name` is included in the error
/// log to make the source obvious in mixed-callback traces.
///
/// Uses [`std::panic::AssertUnwindSafe`] internally because callback
/// bodies routinely touch raw pointers and `*mut` outputs that aren't
/// `UnwindSafe` by default — the pointer dereferences are sound under
/// the FFI contract regardless of unwinding.
///
/// `pub(crate)` so the cdylib-side trampolines in
/// [`crate::core::context::audio_clock_shim`],
/// [`crate::core::context::runtime_ops_shim`], and the per-processor
/// vtable wrappers in [`crate::core::plugin::processor_vtable`] can
/// route through the same helper. Every extern "C" boundary crossing
/// in the engine — host-side and cdylib-side — must be wrapped.
#[inline]
pub(crate) fn run_host_extern_c<F, T>(
    callback_name: &'static str,
    body: F,
    default_on_panic: T,
) -> T
where
    F: FnOnce() -> T,
{
    use std::panic::{catch_unwind, AssertUnwindSafe};
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            tracing::error!(
                target: "streamlib::ffi",
                callback = callback_name,
                panic = %msg,
                "host extern \"C\" callback panicked; FFI boundary converted panic to default return"
            );
            default_on_panic
        }
    }
}

unsafe extern "C" fn host_tracing_register_callsite(
    _host: HostHandle,
    _target_ptr: *const u8,
    _target_len: usize,
    _level: HostLogLevel,
) -> HostInterest {
    run_host_extern_c(
        "host_tracing_register_callsite",
        || {
            // The host's `EnvFilter` filters at emit time via
            // `host_tracing_emit` (it calls `tracing::event!` which
            // fires through the host's subscriber chain). Returning
            // `Always` here tells the cdylib's forwarding
            // `Subscriber` to cache "always emit" for the callsite —
            // every event reaches `host_tracing_emit`, where the
            // host's filter actually decides.
            //
            // Trade-off: cdylib pays for the FFI hop even on
            // filtered-out events, plus a string copy of the
            // message. A future refinement could push a (target,
            // level)-keyed pre-filter here; the current ABI shape
            // doesn't constrain that.
            HostInterest::Always
        },
        HostInterest::Never,
    )
}

unsafe extern "C" fn host_tracing_enabled(
    _host: HostHandle,
    _target_ptr: *const u8,
    _target_len: usize,
    _level: HostLogLevel,
) -> bool {
    run_host_extern_c(
        "host_tracing_enabled",
        || {
            // Paired with `host_tracing_register_callsite` returning
            // `Always`: this never fires from the cdylib side. Kept
            // in the ABI so a future register_callsite that returns
            // `Sometimes` has the per-event enable hook available.
            true
        },
        false,
    )
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
    run_host_extern_c(
        "host_tracing_emit",
        || {
            let target = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(target_ptr, target_len))
            };
            let message = if message_len == 0 {
                ""
            } else {
                unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        message_ptr,
                        message_len,
                    ))
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
        },
        (),
    )
}

unsafe extern "C" fn host_pubsub_publish(
    _host: HostHandle,
    topic_ptr: *const u8,
    topic_len: usize,
    event_msgpack_ptr: *const u8,
    event_msgpack_len: usize,
) {
    run_host_extern_c(
        "host_pubsub_publish",
        || {
            let topic = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(topic_ptr, topic_len))
            };
            let event_bytes =
                unsafe { std::slice::from_raw_parts(event_msgpack_ptr, event_msgpack_len) };
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
        },
        (),
    )
}

unsafe extern "C" fn host_schema_register(
    _host: HostHandle,
    canonical_id_ptr: *const u8,
    canonical_id_len: usize,
    yaml_ptr: *const u8,
    yaml_len: usize,
) {
    run_host_extern_c(
        "host_schema_register",
        || {
            let canonical_id = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                    canonical_id_ptr,
                    canonical_id_len,
                ))
            };
            let yaml = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(yaml_ptr, yaml_len))
            };
            crate::core::embedded_schemas::register_schema(canonical_id.to_string(), yaml);
        },
        (),
    )
}

unsafe extern "C" fn host_schema_lookup(
    _host: HostHandle,
    canonical_id_ptr: *const u8,
    canonical_id_len: usize,
    result_callback: extern "C" fn(*mut c_void, *const u8, usize),
    result_userdata: *mut c_void,
) {
    run_host_extern_c(
        "host_schema_lookup",
        || {
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
        },
        (),
    )
}

unsafe extern "C" fn host_iceoryx_log_emit(
    _host: HostHandle,
    level: HostLogLevel,
    origin_ptr: *const u8,
    origin_len: usize,
    message_ptr: *const u8,
    message_len: usize,
) {
    run_host_extern_c(
        "host_iceoryx_log_emit",
        || {
            let origin = if origin_len == 0 {
                ""
            } else {
                unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        origin_ptr, origin_len,
                    ))
                }
            };
            let message = if message_len == 0 {
                ""
            } else {
                unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        message_ptr,
                        message_len,
                    ))
                }
            };
            // Forward into the host's tracing pipeline at the appropriate level.
            match level {
                HostLogLevel::Trace => {
                    tracing::trace!(target: "iceoryx2", origin = %origin, "{message}")
                }
                HostLogLevel::Debug => {
                    tracing::debug!(target: "iceoryx2", origin = %origin, "{message}")
                }
                HostLogLevel::Info => {
                    tracing::info!(target: "iceoryx2", origin = %origin, "{message}")
                }
                HostLogLevel::Warn => {
                    tracing::warn!(target: "iceoryx2", origin = %origin, "{message}")
                }
                HostLogLevel::Error => {
                    tracing::error!(target: "iceoryx2", origin = %origin, "{message}")
                }
            }
        },
        (),
    )
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
    run_host_extern_c(
        "host_processor_register",
        || {
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

            let descriptor_bytes = unsafe {
                std::slice::from_raw_parts(descriptor_msgpack_ptr, descriptor_msgpack_len)
            };
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
        },
        // Non-zero on panic = error. Discriminate from the explicit
        // failure codes (-1 .. -4) with a fresh value.
        -5,
    )
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
// Host-side static vtables (RuntimeContext / AudioClock / RuntimeOps)
// =============================================================================
//
// The host installs these `&'static` vtables into [`HostServices`] at
// `host_services_for_self` time. Every callback derefs the opaque
// `ctx` / `handle` pointer back to a host-owned Rust type and routes
// through that type's normal Rust accessor — `ctx` for the
// RuntimeContext vtable is a `*const RuntimeContext`, `handle` for
// the audio-clock vtable is a `*const SharedAudioClock`, and `handle`
// for the runtime-ops vtable is a `*const Arc<dyn RuntimeOperations>`.
// The cdylib treats them all as opaque, dispatching through fn
// pointers and reading nothing about layout.

// ---------------- RuntimeContext vtable ----------------

unsafe extern "C" fn host_rcv_runtime_id_copy(
    ctx: *const c_void,
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> usize {
    run_host_extern_c(
        "host_rcv_runtime_id_copy",
        || {
            // SAFETY: host-side construction passes &RuntimeContext as ctx.
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            let id_bytes = rc.runtime_id().as_str().as_bytes();
            write_id_bytes(id_bytes, out_buf, out_buf_cap, out_len)
        },
        0,
    )
}

unsafe extern "C" fn host_rcv_processor_id_copy(
    ctx: *const c_void,
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> isize {
    run_host_extern_c(
        "host_rcv_processor_id_copy",
        || {
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            match rc.processor_id() {
                Some(pid) => {
                    let bytes = pid.as_str().as_bytes();
                    write_id_bytes(bytes, out_buf, out_buf_cap, out_len) as isize
                }
                None => -1,
            }
        },
        -1,
    )
}

unsafe extern "C" fn host_rcv_is_paused(ctx: *const c_void) -> bool {
    run_host_extern_c(
        "host_rcv_is_paused",
        || {
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            rc.is_paused()
        },
        // Pause-on-panic is the conservative default: a panicking
        // is_paused() callback shouldn't keep a runaway processor
        // running. `true` halts further work until the host clears
        // the panic state.
        true,
    )
}

unsafe extern "C" fn host_rcv_should_process(ctx: *const c_void) -> bool {
    run_host_extern_c(
        "host_rcv_should_process",
        || {
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            rc.should_process()
        },
        // Same conservative default as is_paused — false halts the
        // processor until the host clears state.
        false,
    )
}

unsafe extern "C" fn host_rcv_gpu_full_access(_ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_gpu_full_access",
        || {
            // Phase B: the shim still embeds `GpuContextFullAccess`
            // directly alongside the handle/vtable pair, so the
            // cdylib never reaches through this callback. Phase C
            // (#886) replaces the embedded value with
            // `(gpu_full_handle, &HOST_GPU_CONTEXT_VTABLE)` and
            // wires this callback to return the real handle.
            std::ptr::null()
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_gpu_limited_access(_ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_gpu_limited_access",
        || std::ptr::null(),
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_audio_clock_handle(ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_audio_clock_handle",
        || {
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            // The shim's audio-clock handle is a `&SharedAudioClock` —
            // the accompanying [`HOST_AUDIO_CLOCK_VTABLE`] callbacks
            // cast it back to that type and invoke the Rust trait
            // methods.
            rc.audio_clock() as *const SharedAudioClock as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_runtime_ops_handle(ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_runtime_ops_handle",
        || {
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            // `rc.runtime()` produces an owned `Arc<dyn
            // RuntimeOperations>` each call; the per-RuntimeContext
            // handle we hand the cdylib must outlive the call
            // boundary. We keep the canonical handle as
            // `&Arc<dyn RuntimeOperations>` borrowed out of the
            // RuntimeContext's internal storage, which lives as long
            // as the RuntimeContext itself.
            rc.runtime_operations_ref() as *const Arc<dyn RuntimeOperations> as *const c_void
        },
        std::ptr::null(),
    )
}

/// Static [`RuntimeContextVTable`] installed once per process and
/// reused for every cdylib's `RuntimeContext*Access` shim
/// construction. The host-side `RuntimeContextFullAccess::new` /
/// `RuntimeContextLimitedAccess::new` constructors capture
/// `&HOST_RUNTIME_CONTEXT_VTABLE` directly.
pub static HOST_RUNTIME_CONTEXT_VTABLE: RuntimeContextVTable = RuntimeContextVTable {
    layout_version: RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    runtime_id_copy: host_rcv_runtime_id_copy,
    processor_id_copy: host_rcv_processor_id_copy,
    is_paused: host_rcv_is_paused,
    should_process: host_rcv_should_process,
    gpu_full_access: host_rcv_gpu_full_access,
    gpu_limited_access: host_rcv_gpu_limited_access,
    audio_clock_handle: host_rcv_audio_clock_handle,
    runtime_ops_handle: host_rcv_runtime_ops_handle,
};

/// Pointer to the [`RuntimeContextVTable`] this DSO should dispatch
/// through. In the host process this returns the host's local
/// `&HOST_RUNTIME_CONTEXT_VTABLE` static (the canonical vtable). In
/// a cdylib `install_host_services` has populated the cached pointer
/// from `HostServices`, so this returns the HOST'S vtable — meaning
/// every callback invocation lands in host-resident extern "C"
/// functions, not in the cdylib's local copy of those functions.
/// That distinction is load-bearing: the host's functions read
/// host-owned Rust types (`RuntimeContext`) with the host's compiled
/// layout, while the cdylib's local copies would re-interpret the
/// same memory through the cdylib's compiled layout.
pub fn host_runtime_context_vtable() -> *const RuntimeContextVTable {
    match host_callbacks() {
        Some(c) if !c.runtime_context_vtable.is_null() => c.runtime_context_vtable,
        _ => &HOST_RUNTIME_CONTEXT_VTABLE,
    }
}

// ---------------- AudioClock vtable ----------------

unsafe extern "C" fn host_acv_sample_rate(handle: *const c_void) -> u32 {
    run_host_extern_c(
        "host_acv_sample_rate",
        || {
            let clock = unsafe { &*(handle as *const SharedAudioClock) };
            clock.sample_rate()
        },
        0,
    )
}

unsafe extern "C" fn host_acv_buffer_size(handle: *const c_void) -> usize {
    run_host_extern_c(
        "host_acv_buffer_size",
        || {
            let clock = unsafe { &*(handle as *const SharedAudioClock) };
            clock.buffer_size()
        },
        0,
    )
}

unsafe extern "C" fn host_acv_on_tick(
    handle: *const c_void,
    callback: unsafe extern "C" fn(*mut c_void, streamlib_plugin_abi::AudioTickContextRepr),
    user_data: *mut c_void,
    drop_user_data: unsafe extern "C" fn(*mut c_void),
) {
    run_host_extern_c(
        "host_acv_on_tick",
        || {
            let clock = unsafe { &*(handle as *const SharedAudioClock) };

            // Wrap the (callback, user_data, drop_user_data) trio in a single
            // Send + Sync struct that the host's AudioClock holds as its
            // callback's owned state. The struct's `fire` method takes
            // `&self`, which forces the dispatching closure to capture the
            // whole struct (avoiding Rust 2021 disjoint-capture splitting,
            // which would otherwise lift the inner `*mut c_void` out and
            // break Send).
            let bridge = OnTickBridge {
                callback,
                user_data,
                drop_user_data,
            };
            let cb: Box<dyn Fn(crate::core::context::AudioTickContext) + Send + Sync> =
                Box::new(move |ctx_local| bridge.fire(ctx_local));
            clock.on_tick(cb);
        },
        // If on_tick registration panics, the cdylib's `drop_user_data`
        // callback must still fire to reclaim the cdylib-allocated
        // user_data box. The bridge's `Drop` impl runs only when
        // the bridge was actually constructed, so a panic before
        // construction would leak. Mitigate by invoking
        // `drop_user_data` directly when the protected body
        // panicked. SAFETY: cdylib's ABI promises this fn is safe
        // to invoke with the user_data pointer.
        {
            unsafe { (drop_user_data)(user_data) };
        },
    )
}

/// Holder for the cdylib's `(callback, user_data, drop_user_data)`
/// trio. Owns the user-data pointer for the lifetime of the on-tick
/// registration; the deleter fires when the registration drops.
struct OnTickBridge {
    callback: unsafe extern "C" fn(*mut c_void, streamlib_plugin_abi::AudioTickContextRepr),
    user_data: *mut c_void,
    drop_user_data: unsafe extern "C" fn(*mut c_void),
}

// SAFETY: cdylib's ABI contract requires the callback + drop pair to be
// thread-safe. The on-tick callback may fire from any thread the host's
// audio clock chooses (today, the audio-clock thread).
unsafe impl Send for OnTickBridge {}
unsafe impl Sync for OnTickBridge {}

impl OnTickBridge {
    fn fire(&self, ctx: crate::core::context::AudioTickContext) {
        let repr = streamlib_plugin_abi::AudioTickContextRepr {
            timestamp_ns: ctx.timestamp_ns,
            samples_needed: ctx.samples_needed as u64,
            sample_rate: ctx.sample_rate,
            _reserved_padding: 0,
            tick_number: ctx.tick_number,
        };
        // SAFETY: callback + user_data come from the cdylib's ABI
        // promise; valid for the lifetime of this bridge.
        unsafe { (self.callback)(self.user_data, repr) };
    }
}

impl Drop for OnTickBridge {
    fn drop(&mut self) {
        // SAFETY: drop_user_data is part of the cdylib's ABI contract
        // and is called exactly once when this bridge is released.
        unsafe { (self.drop_user_data)(self.user_data) };
    }
}

/// Static [`AudioClockVTable`] installed once per process. Paired
/// with the per-RuntimeContext audio-clock handle returned by
/// [`HOST_RUNTIME_CONTEXT_VTABLE`]`::audio_clock_handle`.
pub static HOST_AUDIO_CLOCK_VTABLE: AudioClockVTable = AudioClockVTable {
    layout_version: AUDIO_CLOCK_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    sample_rate: host_acv_sample_rate,
    buffer_size: host_acv_buffer_size,
    on_tick: host_acv_on_tick,
};

/// Pointer to the [`AudioClockVTable`] this DSO should dispatch
/// through. Same DSO-routing rule as
/// [`host_runtime_context_vtable`]: cdylib reads the host's pointer
/// from the cache populated by `install_host_services`; host falls
/// back to its local static.
pub fn host_audio_clock_vtable() -> *const AudioClockVTable {
    match host_callbacks() {
        Some(c) if !c.audio_clock_vtable.is_null() => c.audio_clock_vtable,
        _ => &HOST_AUDIO_CLOCK_VTABLE,
    }
}

// ---------------- RuntimeOps vtable ----------------
//
// The cdylib-side `RuntimeOpsShim` wraps each submit-with-completion
// callback in a `tokio::sync::oneshot` whose Sender is boxed and
// shipped across the FFI as the `user_data` pointer. The host's
// callback impl spawns on the host's tokio runtime (held in
// `HOST_RUNTIME_TOKIO_HANDLE`), awaits the real
// `RuntimeOperations::*_async` method, encodes the response payload,
// and fires the completion callback.

/// Set by the host once at startup before any cdylib registers. The
/// runtime-ops vtable's callbacks block on this handle to run the
/// real `*_async` methods on the host's tokio runtime, completely
/// invisible to the cdylib (which sees only a `oneshot` it polls on
/// its own runtime).
static HOST_RUNTIME_TOKIO_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// Install the host's tokio handle so the [`HOST_RUNTIME_OPS_VTABLE`]
/// callbacks can spawn `*_async` futures against it. The host's
/// `Runner::start` calls this once before any cdylib is loaded.
/// Idempotent: subsequent calls with a different handle are silently
/// ignored.
pub fn install_host_runtime_tokio_handle(handle: tokio::runtime::Handle) {
    let _ = HOST_RUNTIME_TOKIO_HANDLE.set(handle);
}

fn host_tokio_handle() -> Option<&'static tokio::runtime::Handle> {
    HOST_RUNTIME_TOKIO_HANDLE.get()
}

unsafe fn invoke_completion(
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
    status: i32,
    bytes: &[u8],
) {
    // SAFETY: cdylib promises completion is safe to invoke with the
    // user_data pointer; payload bytes are valid for the call.
    unsafe { completion(user_data, status, bytes.as_ptr(), bytes.len()) };
}

/// RAII guard around the cdylib's submit-with-completion contract.
/// The ABI promises the host fires `completion(user_data, ...)`
/// exactly once. Without this guard a panic inside the spawned
/// `async` body (or a runtime shutdown that drops the future before
/// it awaits) would leak the cdylib's boxed `oneshot::Sender` and
/// hang the cdylib's `rx.await` forever. With the guard, the Drop
/// impl fires an aborted-task error completion if the explicit fire
/// path didn't run.
///
/// Holds `user_data` as a `usize` so the guard is `Send + Sync` (raw
/// pointers aren't). The completion fn pointer is naturally Send.
struct CompletionGuard {
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data_addr: usize,
    fired: bool,
}

impl CompletionGuard {
    fn new(
        completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ) -> Self {
        Self {
            completion,
            user_data_addr: user_data as usize,
            fired: false,
        }
    }

    fn fire_with_result<T: serde::Serialize>(mut self, result: crate::core::Result<T>) {
        self.fired = true;
        let user_data_ptr = self.user_data_addr as *mut c_void;
        match result {
            Ok(value) => match rmp_serde::to_vec_named(&value) {
                Ok(bytes) => unsafe {
                    invoke_completion(self.completion, user_data_ptr, 0, &bytes)
                },
                Err(e) => {
                    let msg = format!("response msgpack encode failed: {e}");
                    unsafe {
                        invoke_completion(self.completion, user_data_ptr, -1, msg.as_bytes())
                    };
                }
            },
            Err(e) => {
                let msg = e.to_string();
                unsafe { invoke_completion(self.completion, user_data_ptr, -1, msg.as_bytes()) };
            }
        }
    }

    fn fire_err_msg(mut self, msg: &[u8]) {
        self.fired = true;
        let user_data_ptr = self.user_data_addr as *mut c_void;
        unsafe { invoke_completion(self.completion, user_data_ptr, -1, msg) };
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if !self.fired {
            // SAFETY: contract promise — completion is always fired
            // exactly once. A drop without a fire signals the host's
            // tokio task aborted (panic or runtime shutdown before
            // the future completed). The cdylib's completion
            // trampoline reclaims its boxed `Sender` either way.
            let user_data_ptr = self.user_data_addr as *mut c_void;
            let msg = b"runtime-ops host task aborted before completion";
            unsafe {
                invoke_completion(self.completion, user_data_ptr, -1, msg);
            }
        }
    }
}

// SAFETY: completion fn pointer is naturally Send; user_data is held
// as a `usize` so the guard can cross `.await` boundaries inside
// tokio task bodies.
unsafe impl Send for CompletionGuard {}
unsafe impl Sync for CompletionGuard {}

unsafe extern "C" fn host_rov_add_processor(
    handle: *const c_void,
    spec_msgpack_ptr: *const u8,
    spec_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_add_processor",
        || {
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let spec_bytes = if spec_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(spec_msgpack_ptr, spec_msgpack_len) }.to_vec()
            };
            rt.spawn(async move {
                let result = match rmp_serde::from_slice::<crate::core::processors::ProcessorSpec>(
                    &spec_bytes,
                ) {
                    Ok(spec) => ops.add_processor_async(spec).await,
                    Err(e) => Err(crate::core::Error::Config(format!(
                        "add_processor: spec msgpack decode failed: {e}"
                    ))),
                };
                guard.fire_with_result(result);
            });
        },
        // Sync-body panic: CompletionGuard's Drop fires the abort
        // completion if `guard` was constructed before the panic;
        // otherwise the cdylib's `rx.await` hangs. The cdylib's
        // RAII-on-Drop trampoline reclaims its boxed Sender either
        // way.
        (),
    )
}

unsafe extern "C" fn host_rov_remove_processor(
    handle: *const c_void,
    processor_id_msgpack_ptr: *const u8,
    processor_id_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_remove_processor",
        || {
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let id_bytes = if processor_id_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe {
                    std::slice::from_raw_parts(processor_id_msgpack_ptr, processor_id_msgpack_len)
                }
                .to_vec()
            };
            rt.spawn(async move {
                let result = match rmp_serde::from_slice::<crate::core::graph::ProcessorUniqueId>(
                    &id_bytes,
                ) {
                    Ok(pid) => ops.remove_processor_async(pid).await,
                    Err(e) => Err(crate::core::Error::Config(format!(
                        "remove_processor: processor_id msgpack decode failed: {e}"
                    ))),
                };
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_connect(
    handle: *const c_void,
    from_msgpack_ptr: *const u8,
    from_msgpack_len: usize,
    to_msgpack_ptr: *const u8,
    to_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_connect",
        || {
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let from_bytes = if from_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(from_msgpack_ptr, from_msgpack_len) }.to_vec()
            };
            let to_bytes = if to_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(to_msgpack_ptr, to_msgpack_len) }.to_vec()
            };
            rt.spawn(async move {
                let from: crate::core::OutputLinkPortRef =
                    match rmp_serde::from_slice(&from_bytes) {
                        Ok(v) => v,
                        Err(e) => {
                            let result: crate::core::Result<crate::core::graph::LinkUniqueId> =
                                Err(crate::core::Error::Config(format!(
                                    "connect: from-port msgpack decode failed: {e}"
                                )));
                            guard.fire_with_result(result);
                            return;
                        }
                    };
                let to: crate::core::InputLinkPortRef = match rmp_serde::from_slice(&to_bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        let result: crate::core::Result<crate::core::graph::LinkUniqueId> =
                            Err(crate::core::Error::Config(format!(
                                "connect: to-port msgpack decode failed: {e}"
                            )));
                        guard.fire_with_result(result);
                        return;
                    }
                };
                let result = ops.connect_async(from, to).await;
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_disconnect(
    handle: *const c_void,
    link_id_msgpack_ptr: *const u8,
    link_id_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_disconnect",
        || {
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let bytes = if link_id_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(link_id_msgpack_ptr, link_id_msgpack_len) }
                    .to_vec()
            };
            rt.spawn(async move {
                let result =
                    match rmp_serde::from_slice::<crate::core::graph::LinkUniqueId>(&bytes) {
                        Ok(link_id) => ops.disconnect_async(link_id).await,
                        Err(e) => Err(crate::core::Error::Config(format!(
                            "disconnect: link_id msgpack decode failed: {e}"
                        ))),
                    };
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_to_json(
    handle: *const c_void,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_to_json",
        || {
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            rt.spawn(async move {
                let result = ops.to_json_async().await;
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

/// Take a (borrowed) handle returned from
/// [`RuntimeContextVTable::runtime_ops_handle`] (a `*const Arc<dyn
/// RuntimeOperations>` pointing into `RuntimeContext`-owned storage)
/// and return a new owned handle: a `Box<Arc<dyn RuntimeOperations>>`
/// with an Arc refcount bump. The owned handle stays alive even if
/// the originating `RuntimeContext` is dropped, because the inner Arc
/// keeps the underlying `dyn RuntimeOperations` impl alive
/// independently. Cdylib drops it via [`host_rov_drop_handle`].
unsafe extern "C" fn host_rov_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rov_clone_handle",
        || {
            if borrowed_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `borrowed_handle` came from `host_rcv_runtime_ops_handle`
            // which cast `&RuntimeContext.runtime_ops` to `*const c_void`.
            let original = unsafe { &*(borrowed_handle as *const Arc<dyn RuntimeOperations>) };
            let cloned: Arc<dyn RuntimeOperations> = Arc::clone(original);
            Box::into_raw(Box::new(cloned)) as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rov_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "host_rov_drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: paired with `host_rov_clone_handle`'s `Box::into_raw`.
            unsafe {
                let _ = Box::from_raw(owned_handle as *mut Arc<dyn RuntimeOperations>);
            }
        },
        (),
    )
}

/// Static [`RuntimeOpsVTable`] installed once per process. Paired
/// with the per-RuntimeContext runtime-ops handle returned by
/// [`HOST_RUNTIME_CONTEXT_VTABLE`]`::runtime_ops_handle`.
pub static HOST_RUNTIME_OPS_VTABLE: RuntimeOpsVTable = RuntimeOpsVTable {
    layout_version: RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    add_processor: host_rov_add_processor,
    remove_processor: host_rov_remove_processor,
    connect: host_rov_connect,
    disconnect: host_rov_disconnect,
    to_json: host_rov_to_json,
    clone_handle: host_rov_clone_handle,
    drop_handle: host_rov_drop_handle,
};

/// Pointer to the [`RuntimeOpsVTable`] this DSO should dispatch
/// through. Same DSO-routing rule as
/// [`host_runtime_context_vtable`].
pub fn host_runtime_ops_vtable() -> *const RuntimeOpsVTable {
    match host_callbacks() {
        Some(c) if !c.runtime_ops_vtable.is_null() => c.runtime_ops_vtable,
        _ => &HOST_RUNTIME_OPS_VTABLE,
    }
}

// ---------------- GpuContextLimitedAccess vtable ----------------
//
// Phase C1 (#901) scaffold: layout-versioned vtable plus passthrough
// `clone_handle` / `drop_handle` stubs. Per-method GPU callbacks
// (`acquire_pixel_buffer`, `release_pixel_buffer`, etc.) append to
// this static in subsequent C1 commits and bump
// [`GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION`].

unsafe extern "C" fn host_gpu_lim_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_clone_handle",
        || {
            if borrowed_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `borrowed_handle` was produced by
            // `GpuContextLimitedAccess::new` (or a prior
            // `clone_handle`) as
            // `Box::into_raw(Box::new(Arc::new(GpuContext)))`.
            // Reading through `&*` and cloning the Arc bumps the
            // underlying refcount; we re-leak via
            // `Box::into_raw(Box::new(...))` so the caller gets a
            // fresh owned handle that matches `drop_handle`'s
            // expected shape.
            let original =
                unsafe { &*(borrowed_handle as *const std::sync::Arc<crate::core::context::GpuContext>) };
            Box::into_raw(Box::new(original.clone())) as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: paired with `GpuContextLimitedAccess::new` and
            // `host_gpu_lim_clone_handle` — both produce
            // `Box::into_raw(Box::new(Arc<GpuContext>))`. Reclaiming
            // via `Box::from_raw` drops the Arc, which decrements
            // the host's `Arc<GpuContext>` refcount and frees the
            // underlying `GpuContext` when the count reaches zero.
            unsafe {
                let _ = Box::from_raw(
                    owned_handle as *mut std::sync::Arc<crate::core::context::GpuContext>,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_pixel_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_pixel_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is a `*const c_void` cast of
            // `Arc::into_raw(Arc<PixelBufferRef>)` produced by
            // `PixelBuffer::new` (host-side). Re-interpreting it as
            // `*const PixelBufferRef` and bumping the strong count is the
            // documented `Arc::increment_strong_count` contract.
            unsafe {
                Arc::increment_strong_count(handle as *const crate::core::rhi::PixelBufferRef);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_pixel_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_pixel_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `host_gpu_lim_clone_pixel_buffer` and
            // `PixelBuffer::new`'s `Arc::into_raw` initial bump.
            // `Arc::decrement_strong_count` decrements; when refcount hits
            // zero the underlying `PixelBufferRef` is dropped along with
            // its platform buffer.
            unsafe {
                Arc::decrement_strong_count(handle as *const crate::core::rhi::PixelBufferRef);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_strong_count_pixel_buffer(handle: *const c_void) -> usize {
    run_host_extern_c(
        "host_gpu_lim_strong_count_pixel_buffer",
        || {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`-shaped
            // (see `PixelBuffer::new`'s `from_arc_into_raw`). We
            // reconstruct the `Arc` temporarily, read the strong count, and
            // immediately re-leak it via `Arc::into_raw` so the strong count
            // returns to its pre-call value — `Arc::strong_count_from_raw`
            // is not part of the public stable API. The reconstruction runs
            // in HOST-COMPILED code regardless of caller DSO, so the cdylib
            // never has to know `PixelBufferRef`'s in-memory layout.
            unsafe {
                let arc =
                    Arc::from_raw(handle as *const crate::core::rhi::PixelBufferRef);
                let count = Arc::strong_count(&arc);
                let _ = Arc::into_raw(arc);
                count
            }
        },
        0,
    )
}

unsafe extern "C" fn host_gpu_lim_plane_base_address_pixel_buffer(
    handle: *const c_void,
    plane_index: u32,
) -> *mut u8 {
    run_host_extern_c(
        "host_gpu_lim_plane_base_address_pixel_buffer",
        || {
            if handle.is_null() {
                return core::ptr::null_mut();
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`-shaped;
            // the leaked strong count keeps the `PixelBufferRef` alive for
            // the duration of the call. We borrow `&PixelBufferRef` rather
            // than reconstructing the Arc to avoid touching the refcount.
            unsafe {
                let pb_ref = &*(handle as *const crate::core::rhi::PixelBufferRef);
                pb_ref.plane_base_address(plane_index)
            }
        },
        core::ptr::null_mut(),
    )
}

unsafe extern "C" fn host_gpu_lim_plane_size_pixel_buffer(
    handle: *const c_void,
    plane_index: u32,
) -> u64 {
    run_host_extern_c(
        "host_gpu_lim_plane_size_pixel_buffer",
        || {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: same as `host_gpu_lim_plane_base_address_pixel_buffer`.
            unsafe {
                let pb_ref = &*(handle as *const crate::core::rhi::PixelBufferRef);
                pb_ref.plane_size(plane_index)
            }
        },
        0,
    )
}

// -------------------------------------------------------------------------
// Texture Arc-handle lifecycle (v4 — Phase 2A)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_clone_texture(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_texture",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is a `*const c_void` cast of
            // `Arc::into_raw(Arc<TextureInner>)` produced by host
            // code (see `Texture::from_arc_into_raw`).
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_texture(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_texture",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `Texture::from_arc_into_raw` and any prior
            // `clone_texture` bumps.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// PooledTextureHandle lifecycle — drop-only (v4)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_drop_pooled_texture_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_pooled_texture_handle",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw(Box<...>)` in
            // `PooledTextureHandle::from_parts`. Reclaiming via
            // `Box::from_raw` runs `Drop for PooledTextureHandleInner`
            // which releases the pool slot exactly once.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::core::context::texture_pool::PooledTextureHandleInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Method dispatch — Texture-related (v4)
// -------------------------------------------------------------------------

/// Borrow a `&Arc<GpuContext>` from a `*const Arc<GpuContext>`-shaped
/// host handle. Caller must guarantee `handle` came from
/// [`crate::core::context::GpuContextLimitedAccess::new`] or
/// [`host_gpu_lim_clone_handle`]; both produce
/// `Box::into_raw(Box::new(Arc::new(...))) as *const c_void`.
unsafe fn handle_as_gpu_context(
    handle: *const c_void,
) -> Option<&'static Arc<crate::core::context::GpuContext>> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: caller-supplied contract; the Box keeps the Arc alive
    // for the duration of the dispatch through the vtable.
    unsafe { Some(&*(handle as *const Arc<crate::core::context::GpuContext>)) }
}

unsafe fn slice_from_raw(ptr: *const u8, len: usize) -> &'static [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: caller-supplied UTF-8 byte slice; the lifetime is
    // bounded by the dispatch (we never store the slice past return).
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn write_err(msg: &str, err_buf: *mut u8, err_buf_cap: usize, err_len: *mut usize) {
    let bytes = msg.as_bytes();
    let written = bytes.len().min(err_buf_cap);
    if written > 0 && !err_buf.is_null() {
        // SAFETY: caller-provided `err_buf` is writable for `err_buf_cap`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), err_buf, written) };
    }
    if !err_len.is_null() {
        // SAFETY: caller-provided `err_len` is writable.
        unsafe { *err_len = written };
    }
}

unsafe extern "C" fn host_gpu_lim_register_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    texture_handle: *const c_void,
    initial_layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_register_texture",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            if texture_handle.is_null() {
                return;
            }
            // SAFETY: `texture_handle` is `Arc::into_raw(Arc<TextureInner>)`-shaped.
            // Bump the refcount so we can hand the cache its own owned
            // Arc; the caller's Texture continues to own its own.
            unsafe {
                Arc::increment_strong_count(
                    texture_handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
            // SAFETY: same shape as above; from_raw + the bump above
            // gives us a fresh Arc with the right refcount.
            let texture_arc = unsafe {
                Arc::from_raw(
                    texture_handle as *const crate::core::rhi::texture::TextureInner,
                )
            };
            let inner_ref = &*texture_arc;
            let width = inner_ref.width();
            let height = inner_ref.height();
            let format = inner_ref.format();
            // Re-wrap into a Texture via the host's from_arc_into_raw
            // helper — leaks the Arc back into the texture cache shape.
            let texture =
                crate::core::rhi::texture::Texture::from_arc_into_raw(
                    texture_arc, width, height, format,
                );
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            #[cfg(target_os = "linux")]
            {
                let layout = streamlib_consumer_rhi::VulkanLayout(initial_layout_raw);
                gpu.register_texture_with_layout(id_str, texture, layout);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = initial_layout_raw;
                gpu.register_texture(id_str, texture);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_update_texture_registration_layout(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_update_texture_registration_layout",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            #[cfg(target_os = "linux")]
            {
                let layout = streamlib_consumer_rhi::VulkanLayout(layout_raw);
                gpu.update_texture_registration_layout(id_str, layout);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (id_str, layout_raw);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_acquire_texture(
    handle: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    usage_bits: u32,
    out_pooled_handle: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_texture",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err("acquire_texture: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_pooled_handle.is_null() {
                write_err("acquire_texture: null out_pooled_handle", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    let msg = format!("acquire_texture: invalid format_raw {}", format_raw);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    return 1;
                }
            };
            let usage =
                streamlib_consumer_rhi::TextureUsages::from_bits_truncate(usage_bits);
            let desc = crate::core::context::TexturePoolDescriptor {
                width,
                height,
                format,
                usage,
                label: None,
            };
            match gpu.acquire_texture(&desc) {
                Ok(pooled) => {
                    // Move the host-built PooledTextureHandle into the
                    // caller's out-slot. The caller (cdylib) owns it
                    // after this — its Drop runs `drop_pooled_texture_handle`.
                    unsafe {
                        std::ptr::write(
                            out_pooled_handle
                                as *mut crate::core::context::PooledTextureHandle,
                            pooled,
                        );
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_resolve_texture_by_surface_id(
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
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_texture_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "resolve_texture_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_texture.is_null() {
                write_err(
                    "resolve_texture_by_surface_id: null out_texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_texture_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let texture_layout = if has_layout != 0 {
                Some(layout_raw)
            } else {
                None
            };
            match gpu.resolve_texture_by_surface_id(id_str, texture_layout, width, height) {
                Ok(texture) => {
                    // Hand the texture to the caller's out-slot. The
                    // caller (cdylib) owns it after this — its Drop
                    // runs `drop_texture`.
                    unsafe {
                        std::ptr::write(
                            out_texture as *mut crate::core::rhi::Texture,
                            texture,
                        );
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_unregister_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
) {
    run_host_extern_c(
        "host_gpu_lim_unregister_texture",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            gpu.unregister_texture(id_str);
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Linux-only buffer Arc-handle lifecycle (v5 — Phase 2B)
// -------------------------------------------------------------------------
//
// All 4 buffer types (`StorageBuffer`, `UniformBuffer`, `VertexBuffer`,
// `IndexBuffer`) wrap `Arc<HostVulkanBuffer>` under the hood. The per-
// type callbacks are individually addressable in the vtable (so future
// per-type divergence doesn't force a re-version) but share the same
// host-side bookkeeping today. On non-Linux hosts the buffer types
// don't exist, so the callbacks compile to no-ops / error returns —
// the vtable slot is unconditional for ABI stability.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_clone_host_vulkan_buffer_arc(handle: *const c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` is `Arc::into_raw(Arc<HostVulkanBuffer>)`-shaped
    // (see each buffer type's `from_arc_into_raw` constructor).
    unsafe {
        Arc::increment_strong_count(handle as *const crate::vulkan::rhi::HostVulkanBuffer);
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_drop_host_vulkan_buffer_arc(handle: *const c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: matched with the `Arc::into_raw` in each buffer type's
    // `from_arc_into_raw` constructor.
    unsafe {
        Arc::decrement_strong_count(handle as *const crate::vulkan::rhi::HostVulkanBuffer);
    }
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_clone_host_vulkan_buffer_arc(_handle: *const c_void) {
    // Buffer types only exist on Linux; this callback is unreachable
    // on other platforms. Defensive no-op.
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_drop_host_vulkan_buffer_arc(_handle: *const c_void) {
    // Buffer types only exist on Linux; defensive no-op.
}

// Per-type wrappers. Each just delegates to the shared
// `host_vulkan_buffer_arc` pair today but lives in the vtable as a
// dedicated slot, so a future per-type divergence (e.g. UniformBuffer
// growing a per-type cached field that needs its own clone semantics)
// only edits the wrapper without touching the vtable surface.

unsafe extern "C" fn host_gpu_lim_clone_storage_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_storage_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_storage_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_storage_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_uniform_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_uniform_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_uniform_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_uniform_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_vertex_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_vertex_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_vertex_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_vertex_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_index_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_index_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_index_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_index_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

// -------------------------------------------------------------------------
// Linux-only acquire_*_buffer method dispatch (v5)
// -------------------------------------------------------------------------

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_storage_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_storage_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_storage_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_storage_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_storage_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::StorageBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_uniform_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_uniform_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_uniform_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_uniform_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_uniform_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::UniformBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_vertex_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_vertex_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_vertex_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_vertex_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_vertex_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::VertexBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_index_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_index_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_index_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_index_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_index_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::IndexBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_storage_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_storage_buffer: StorageBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_uniform_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_uniform_buffer: UniformBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_vertex_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_vertex_buffer: VertexBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_index_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_index_buffer: IndexBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// -------------------------------------------------------------------------
// TextureRegistration Arc-handle lifecycle (v6 — Phase 2C)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_clone_texture_registration(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_texture_registration",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::texture_registration::TextureRegistrationInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_texture_registration(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_texture_registration",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `TextureRegistration::from_arc_into_raw`.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::texture_registration::TextureRegistrationInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// TextureRegistration method dispatch (v6)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_texture_registration_texture(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_texture",
        || {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`-shaped;
            // the Arc's strong count keeps the inner alive. We return
            // a pointer to the inner's `texture` field; the caller
            // (cdylib) deref's it as `*const Texture`. The pointer is
            // alive as long as the caller's `TextureRegistration` is.
            unsafe {
                let inner = &*(handle
                    as *const crate::core::context::texture_registration::TextureRegistrationInner);
                &inner.texture as *const crate::core::rhi::Texture as *const c_void
            }
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_gpu_lim_texture_registration_current_layout(
    handle: *const c_void,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_current_layout",
        || {
            if handle.is_null() {
                return 0; // VK_IMAGE_LAYOUT_UNDEFINED
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `handle` is `Arc::into_raw(...)`-shaped.
                unsafe {
                    let inner = &*(handle
                        as *const crate::core::context::texture_registration::TextureRegistrationInner);
                    inner
                        .current_layout
                        .load(std::sync::atomic::Ordering::Acquire)
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = handle;
                0
            }
        },
        0,
    )
}

unsafe extern "C" fn host_gpu_lim_texture_registration_update_layout(
    handle: *const c_void,
    layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_update_layout",
        || {
            if handle.is_null() {
                return;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: same shape as
                // `host_gpu_lim_texture_registration_current_layout`.
                unsafe {
                    let inner = &*(handle
                        as *const crate::core::context::texture_registration::TextureRegistrationInner);
                    inner
                        .current_layout
                        .store(layout_raw, std::sync::atomic::Ordering::Release);
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (handle, layout_raw);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_resolve_texture_registration_by_surface_id(
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
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_texture_registration_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "resolve_texture_registration_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_registration.is_null() {
                write_err(
                    "resolve_texture_registration_by_surface_id: null out_registration",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_texture_registration_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let texture_layout = if has_layout != 0 {
                Some(layout_raw)
            } else {
                None
            };
            match gpu.resolve_texture_registration_by_surface_id(id_str, texture_layout, width, height) {
                Ok(reg) => {
                    // SAFETY: out_registration points at caller-allocated
                    // stack storage for a `TextureRegistration` value.
                    unsafe {
                        std::ptr::write(
                            out_registration
                                as *mut crate::core::context::TextureRegistration,
                            reg,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

/// Static [`GpuContextLimitedAccessVTable`] installed once per process.
/// Paired with the per-RuntimeContext gpu-limited handle returned by
/// [`HOST_RUNTIME_CONTEXT_VTABLE`]`::gpu_limited_access`.
pub static HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE: GpuContextLimitedAccessVTable =
    GpuContextLimitedAccessVTable {
        layout_version: GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        clone_handle: host_gpu_lim_clone_handle,
        drop_handle: host_gpu_lim_drop_handle,
        clone_pixel_buffer: host_gpu_lim_clone_pixel_buffer,
        drop_pixel_buffer: host_gpu_lim_drop_pixel_buffer,
        strong_count_pixel_buffer: host_gpu_lim_strong_count_pixel_buffer,
        plane_base_address_pixel_buffer: host_gpu_lim_plane_base_address_pixel_buffer,
        plane_size_pixel_buffer: host_gpu_lim_plane_size_pixel_buffer,
        clone_texture: host_gpu_lim_clone_texture,
        drop_texture: host_gpu_lim_drop_texture,
        drop_pooled_texture_handle: host_gpu_lim_drop_pooled_texture_handle,
        register_texture: host_gpu_lim_register_texture,
        update_texture_registration_layout: host_gpu_lim_update_texture_registration_layout,
        acquire_texture: host_gpu_lim_acquire_texture,
        resolve_texture_by_surface_id: host_gpu_lim_resolve_texture_by_surface_id,
        unregister_texture: host_gpu_lim_unregister_texture,
        clone_storage_buffer: host_gpu_lim_clone_storage_buffer,
        drop_storage_buffer: host_gpu_lim_drop_storage_buffer,
        clone_uniform_buffer: host_gpu_lim_clone_uniform_buffer,
        drop_uniform_buffer: host_gpu_lim_drop_uniform_buffer,
        clone_vertex_buffer: host_gpu_lim_clone_vertex_buffer,
        drop_vertex_buffer: host_gpu_lim_drop_vertex_buffer,
        clone_index_buffer: host_gpu_lim_clone_index_buffer,
        drop_index_buffer: host_gpu_lim_drop_index_buffer,
        acquire_storage_buffer: host_gpu_lim_acquire_storage_buffer,
        acquire_uniform_buffer: host_gpu_lim_acquire_uniform_buffer,
        acquire_vertex_buffer: host_gpu_lim_acquire_vertex_buffer,
        acquire_index_buffer: host_gpu_lim_acquire_index_buffer,
        clone_texture_registration: host_gpu_lim_clone_texture_registration,
        drop_texture_registration: host_gpu_lim_drop_texture_registration,
        texture_registration_texture: host_gpu_lim_texture_registration_texture,
        texture_registration_current_layout: host_gpu_lim_texture_registration_current_layout,
        texture_registration_update_layout: host_gpu_lim_texture_registration_update_layout,
        resolve_texture_registration_by_surface_id:
            host_gpu_lim_resolve_texture_registration_by_surface_id,
    };

/// Pointer to the [`GpuContextLimitedAccessVTable`] this DSO should
/// dispatch through. Same DSO-routing rule as
/// [`host_runtime_context_vtable`].
pub fn host_gpu_context_limited_access_vtable() -> *const GpuContextLimitedAccessVTable {
    match host_callbacks() {
        Some(c) if !c.gpu_context_limited_access_vtable.is_null() => {
            c.gpu_context_limited_access_vtable
        }
        _ => &HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
    }
}

// ---------------- Shared scratch-buffer helper ----------------

fn write_id_bytes(
    bytes: &[u8],
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> usize {
    let required = bytes.len();
    let written = required.min(out_buf_cap);
    if written > 0 && !out_buf.is_null() {
        // SAFETY: caller guarantees `out_buf` is writable for
        // `out_buf_cap` bytes; we only write `written` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, written) };
    }
    if !out_len.is_null() {
        // SAFETY: caller guarantees `out_len` is writable.
        unsafe { *out_len = written };
    }
    required
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
        host_tracing_register_callsite, HostServiceImpls, HOST_AUDIO_CLOCK_VTABLE,
        HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE, HOST_RUNTIME_CONTEXT_VTABLE,
        HOST_RUNTIME_OPS_VTABLE,
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
            runtime_context_vtable: &HOST_RUNTIME_CONTEXT_VTABLE,
            audio_clock_vtable: &HOST_AUDIO_CLOCK_VTABLE,
            runtime_ops_vtable: &HOST_RUNTIME_OPS_VTABLE,
            gpu_context_limited_access_vtable: &HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
        }
    }
}
