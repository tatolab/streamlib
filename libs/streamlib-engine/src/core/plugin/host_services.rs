// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-services payload carried by the `STREAMLIB_PLUGIN` register
//! callback.
//!
//! When the host's `Runner::load_project` (or `streamlib-runtime`'s
//! standalone plugin loader) `dlopen`s a Rust plugin cdylib and
//! invokes its `STREAMLIB_PLUGIN.register(host_services)` callback,
//! `host_services` points at a [`HostServices`] payload owned by the
//! host. The cdylib's macro expansion forwards the pointer into
//! [`install_host_services`], which:
//!
//! 1. Validates the wire layout (`abi_layout_version`) before any
//!    other field is touched. On mismatch the helper logs and returns
//!    `None` — the host's post-call "processor not registered" check
//!    then surfaces an actionable error.
//! 2. Bridges every process-wide static the cdylib statically embeds
//!    a per-DSO copy of (tracing dispatch, [`PUBSUB`], schema
//!    registry, iceoryx2-log logger) into the host's instances.
//! 3. Returns the host's `&'static ProcessorInstanceFactory` so the
//!    macro can register the plugin's processor types into it.
//!
//! ## Why this lives in streamlib-engine, not streamlib-plugin-abi
//!
//! `streamlib-plugin-abi` is the bare wire-protocol header
//! (`PluginDeclaration`, `STREAMLIB_ABI_VERSION`, the macro). It
//! intentionally carries no streamlib deps so it doesn't pull the
//! whole engine into every dependent crate's graph (and so the
//! pre-#877 cyclic `engine → plugin-abi → sdk → engine` workaround
//! is gone). The typed host-services payload lives here because all
//! its field types (`ProcessorInstanceFactory`, `PubSub`,
//! `Iceoryx2Node`, `tracing::Dispatch`, etc.) are engine types.
//!
//! The cdylib reaches this module via
//! `streamlib::sdk::plugin::install_host_services` — the SDK
//! re-exports it under that path. Plugin-abi's macro hard-codes that
//! path in its expansion.
//!
//! ## Cross-DSO safety
//!
//! Every field of [`HostServices`] is a raw pointer or a value type
//! whose layout the dynamic linker does not dedupe across DSOs. The
//! safety contract for casting a `*const c_void` back to a typed
//! reference is that **both the host and cdylib link the same
//! version of every relevant crate** (workspace dep pins). The
//! milestone-level commitment is "rustc-version coupling stays";
//! this module rides that commitment for `streamlib-engine`,
//! `iceoryx2`, and `iceoryx2-log-types`.
//!
//! ## Known gap — tracing dispatch passthrough
//!
//! `set_global_default` succeeds in the cdylib and installs the
//! host's `Dispatch` as the cdylib's global. Empirically, events
//! emitted from cdylib code via `tracing::*!()` still do not reach
//! the host's `Subscriber::event` impl — they are absorbed
//! somewhere between the cdylib's `Dispatch::event` and the host's
//! `Layered<EnvFilter, ...>` Subscriber even with both DSOs on the
//! same workspace-pinned `tracing-core 0.1.36`. Cross-DSO Callsite
//! identity, vtable layout, or Registry span-stack semantics are
//! the prime suspects.
//!
//! Cross-DSO tracing-dispatch passthrough is a known-not-viable
//! pattern in the broader Rust ecosystem. The canonical fix is a
//! callback-table shape — host exports `extern "C"` callbacks for
//! `emit` / `enabled` / `enter` / `exit`, cdylib installs a thin
//! local Subscriber that forwards through them. `tracing-ext-ffi-subscriber`
//! is the reference implementation cited in #877's issue body.
//!
//! This module ships the partial bridge today (PUBSUB + schema
//! registry + iceoryx2-log work end-to-end; tracing dispatch
//! installs but events don't flow). The callback-table tracing
//! bridge is a follow-up. Until it lands, cdylib code that wants
//! observability should rely on PUBSUB events for its host-visible
//! signals.

use std::ffi::c_void;

use tracing::Dispatch;

use crate::core::embedded_schemas::SchemaRegistryStorage;
use crate::core::logging::iceoryx2_log_bridge;
use crate::core::processors::ProcessorInstanceFactory;
use crate::core::pubsub::PubSub;

/// Wire layout version. Bump on any change to [`HostServices`]'s
/// field ordering or contents (including non-`#[repr(C)]` value-type
/// fields whose Rust layout may shift across compiler versions —
/// `tracing::Dispatch` is the canonical example).
///
/// Read first by [`install_host_services`] before any other field
/// is touched. On mismatch the helper bails out without dereferencing
/// the rest of the struct, so it's safe even if the cdylib was built
/// against an older `HostServices` layout.
pub const HOST_SERVICES_LAYOUT_VERSION: u32 = 1;

/// Payload the host passes through the `STREAMLIB_PLUGIN` register
/// callback. The cdylib's macro expansion casts the `*const c_void`
/// parameter to `*const HostServices` and reads the fields below.
///
/// Field ordering is **stable** within a layout-version bucket — the
/// first two fields (`abi_layout_version`, `fatal_log`) are pinned
/// at offset 0 forever so [`install_host_services`] can read them
/// even when the rest of the struct's layout has drifted. New
/// fields go at the end and bump [`HOST_SERVICES_LAYOUT_VERSION`].
///
/// Field semantics:
///
/// * `abi_layout_version` — must equal [`HOST_SERVICES_LAYOUT_VERSION`].
/// * `processor_registry` — host's `&'static ProcessorInstanceFactory`
///   (`PROCESSOR_REGISTRY`). The cdylib registers into this rather
///   than its own per-DSO `LazyLock<ProcessorInstanceFactory>`.
/// * `pubsub` — host's `&'static PubSub`
///   (`crate::core::pubsub::bus::local_pubsub()` value). The cdylib's
///   `crate::core::pubsub::bus::install_host_pubsub` installs it so
///   `PUBSUB.publish(...)` in cdylib code reaches the host's bus.
/// * `schema_registry` — host's `&'static
///   SchemaRegistryStorage`. Same shape as PUBSUB bridging:
///   `crate::core::embedded_schemas::install_host_schema_registry`
///   wires the cdylib's `register_schema` /
///   `get_embedded_schema_definition` through to the host's storage.
/// * `tracing_dispatch_ptr` — pointer to a `Dispatch` value the host
///   keeps alive (`Box::leak`-ed at first plugin load — see
///   [`runtime_facing::host_dispatch_for_self`]). The cdylib clones
///   it via `(&*ptr).clone()` and calls
///   `tracing::dispatcher::set_global_default(...)` against its own
///   `tracing-core` static. After this, `tracing::info!` in cdylib
///   code flows into the host's subscriber.
/// * `iceoryx2_logger_ptr` — pointer to a `&'static dyn Log`
///   trait-object value living in host memory (specifically a
///   reference to [`HOST_BRIDGE`], boxed-and-leaked once). The
///   cdylib installs it via
///   `crate::core::logging::iceoryx2_log_bridge::install_foreign_iceoryx2_logger`
///   so iceoryx2's internal log records in cdylib code reach the
///   host's tracing pipeline. May be null if the host hasn't wired
///   one (today: never null — `Runner::new` always installs the
///   bridge before any plugin load).
/// * `iceoryx2_node_ptr` — pointer to the host's `Iceoryx2Node`.
///   Reserved for future cdylib code that needs to create iceoryx2
///   services in its own DSO using the host's node identity; today
///   no cdylib code uses this, but it's the right shape and rounds
///   out the FFI surface so future consumers don't redo this design.
#[repr(C)]
pub struct HostServices {
    /// Wire layout version. See [`HOST_SERVICES_LAYOUT_VERSION`].
    pub abi_layout_version: u32,

    /// Padding to keep the following pointer fields naturally
    /// aligned across 32/64-bit hosts (not relevant on streamlib's
    /// 64-bit targets today; explicit for clarity).
    pub _reserved_padding: u32,

    /// Host's process-instance factory (`PROCESSOR_REGISTRY`).
    pub processor_registry: *const c_void,

    /// Host's `&'static PubSub` (`local_pubsub` value).
    pub pubsub: *const c_void,

    /// Host's `&'static SchemaRegistryStorage` (`local_schema_registry`
    /// value).
    pub schema_registry: *const c_void,

    /// Pointer to a `Dispatch` value the host keeps alive on the
    /// heap (see [`runtime_facing::host_dispatch_for_self`]).
    pub tracing_dispatch_ptr: *const c_void,

    /// Pointer to a `&'static dyn iceoryx2_log::Log` value the host
    /// keeps alive (see [`runtime_facing::host_iceoryx2_logger_for_self`]).
    /// May be null.
    pub iceoryx2_logger_ptr: *const c_void,

    /// Pointer to the host's `Iceoryx2Node` value, kept alive on the
    /// heap (see [`runtime_facing::host_iceoryx2_node_for_self`]).
    /// Reserved for future cdylib consumers; safe to ignore.
    pub iceoryx2_node_ptr: *const c_void,
}

// Safety: every field is a raw pointer or a primitive. The host
// guarantees the pointed-at values outlive the cdylib's process
// lifetime via the `LOADED_PLUGIN_LIBRARIES` pinning shape.
unsafe impl Send for HostServices {}
unsafe impl Sync for HostServices {}

/// Bridge every process-wide static this DSO embeds a per-DSO copy
/// of to the host's instances. Called from a plugin cdylib's
/// `STREAMLIB_PLUGIN` register callback via the
/// [`streamlib_plugin_abi::export_plugin`] macro expansion.
///
/// # Returns
///
/// `Some(&'static ProcessorInstanceFactory)` on success — the host's
/// registry the cdylib registers into. `None` on layout-version
/// mismatch (the cdylib was built against a stale `HostServices`
/// layout). The macro short-circuits processor registration on
/// `None`, and the host's post-call "processor not registered"
/// check surfaces a `Configuration` error.
///
/// # Safety
///
/// `host_services_ptr` must point at a [`HostServices`] value
/// initialized by the host. The host's loader guarantees this.
pub unsafe fn install_host_services(
    host_services_ptr: *const c_void,
) -> Option<&'static ProcessorInstanceFactory> {
    if host_services_ptr.is_null() {
        return None;
    }

    // SAFETY: per the caller's promise. Read `abi_layout_version`
    // before touching any other field — if the layout doesn't match,
    // the rest of the struct may have a different shape on the wire.
    let services = unsafe { &*(host_services_ptr as *const HostServices) };

    if services.abi_layout_version != HOST_SERVICES_LAYOUT_VERSION {
        // Logging may not be wired yet — emit through tracing
        // (which is silent if this DSO's dispatch hasn't been
        // installed) AND via the host's stderr as a backstop. The
        // host's loader will surface the failure via the post-call
        // "processor not registered" check.
        //
        // We deliberately do NOT write to stderr directly here
        // (banned by streamlib's logging discipline). The host can
        // tell something went wrong because no processors registered.
        tracing::error!(
            target: "streamlib::plugin",
            host_layout = HOST_SERVICES_LAYOUT_VERSION,
            cdylib_layout = services.abi_layout_version,
            "Plugin HostServices layout mismatch; aborting register"
        );
        return None;
    }

    // SAFETY: pointers were constructed by the host from `&'static`
    // references to its own statics. Both DSOs link the same
    // streamlib-engine version per the milestone-level rustc-version
    // coupling commitment, so the layouts match.
    let processor_registry =
        unsafe { &*(services.processor_registry as *const ProcessorInstanceFactory) };
    let pubsub = unsafe { &*(services.pubsub as *const PubSub) };
    let schema_registry =
        unsafe { &*(services.schema_registry as *const SchemaRegistryStorage) };

    // Best-effort tracing-dispatch bridge.
    //
    // `set_global_default` here transitions the cdylib's per-DSO
    // `tracing-core::GLOBAL_INIT` to `INITIALIZED` with a Dispatch
    // whose `Kind::Global` subscriber pointer is the host's. The
    // call returns `Ok` in production. Empirically, however, events
    // emitted from cdylib code via `tracing::*!()` do NOT reach the
    // host's `Subscriber::event` impl through this path — events
    // are silently absorbed somewhere between the cdylib's
    // `Dispatch::event` and the host's `Layered<EnvFilter, ...>`
    // Subscriber, even though both DSOs link the same workspace-
    // pinned `tracing-core 0.1.36` version. The exact cause is
    // unclear; cross-DSO Callsite identity, vtable, or Registry
    // span-stack semantics are the prime suspects.
    //
    // Cross-DSO tracing-dispatch passthrough is a known-not-viable
    // pattern in the broader Rust ecosystem — the canonical fix is
    // a callback-table shape (see `tracing-ext-ffi-subscriber`,
    // cited in #877's issue body) where the host exports
    // `extern "C"` callbacks for emit/enabled/enter/exit and the
    // cdylib installs a thin local Subscriber that forwards through
    // them. That work is tracked as a follow-up to #877; see the
    // gap section in this module's docs.
    //
    // We still call `set_global_default` here so the cdylib's
    // `GLOBAL_DISPATCH` isn't the no-op default — that lets
    // `tracing::*!()` macros short-circuit fast and avoids any
    // tracing-subscriber lazy-init costs on the cdylib's hot path.
    if !services.tracing_dispatch_ptr.is_null() {
        let host_dispatch = unsafe { &*(services.tracing_dispatch_ptr as *const Dispatch) };
        let _ = tracing::dispatcher::set_global_default(host_dispatch.clone());
    }

    // Bridge PUBSUB + schema registry. Order doesn't matter — both
    // are `OnceLock<&'static T>` writes.
    crate::core::pubsub::install_host_pubsub(pubsub);
    crate::core::embedded_schemas::install_host_schema_registry(schema_registry);

    // Bridge iceoryx2-log if the host installed one. The host
    // currently always installs `HOST_BRIDGE` at `Runner::new`, so
    // the null branch is unreachable in production — kept for
    // forward-compat with embedded contexts that may not.
    if !services.iceoryx2_logger_ptr.is_null() {
        // The host stores `&'static dyn Log` (a fat pointer) at the
        // heap location pointed to by `iceoryx2_logger_ptr`. Read
        // and forward to this DSO's `iceoryx2-log`.
        let logger_ref =
            unsafe { *(services.iceoryx2_logger_ptr as *const &'static dyn iceoryx2_log::Log) };
        unsafe { iceoryx2_log_bridge::install_foreign_iceoryx2_logger(logger_ref) };
    }

    // `iceoryx2_node_ptr` is reserved — no in-tree consumer reads
    // it yet. The cdylib leaves it as null-or-set; future code that
    // wants the host's Iceoryx2Node can clone via
    // `Arc<Mutex<Node>>` semantics (Iceoryx2Node is `Clone`).

    Some(processor_registry)
}

// =============================================================================
// Host-facing helpers for building a HostServices payload.
// =============================================================================

/// Host-facing helpers — used by `Runner::load_project` and
/// `streamlib-runtime`'s plugin loader to build a [`HostServices`]
/// payload from the host's own statics.
pub mod runtime_facing {
    use super::{HostServices, HOST_SERVICES_LAYOUT_VERSION};
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use tracing::Dispatch;

    /// Heap-allocated `Dispatch` clone the host keeps alive so the
    /// pointer handed to plugin cdylibs stays valid. Lazy-built on
    /// first call; subsequent calls reuse the same boxed value.
    ///
    /// Uses `Box::leak` — the leak lasts the process lifetime, which
    /// matches `LOADED_PLUGIN_LIBRARIES`'s pinning lifetime for
    /// loaded cdylibs.
    static HOST_TRACING_DISPATCH: OnceLock<&'static Dispatch> = OnceLock::new();

    /// Heap-allocated `&'static dyn Log` trait-object pointer the
    /// host hands to plugin cdylibs. Boxed-and-leaked once for the
    /// same lifetime reason.
    type StaticLogRef = &'static dyn iceoryx2_log::Log;
    static HOST_ICEORYX2_LOGGER: OnceLock<&'static StaticLogRef> = OnceLock::new();

    /// Heap-allocated `Iceoryx2Node` clone the host keeps alive for
    /// the same reason. Boxed once and reused.
    static HOST_ICEORYX2_NODE: OnceLock<&'static crate::iceoryx2::Iceoryx2Node> = OnceLock::new();

    /// Return a `&'static Dispatch` pointing at a clone of
    /// tracing-core's current `GLOBAL_DISPATCH`. Captured on first
    /// call so plugins loaded later still see the dispatch active
    /// at the time `Runner::new` wired tracing.
    ///
    /// Must not be called before `Runner::new` returns — if no
    /// `set_global_default` has fired yet, `get_default` returns
    /// the no-op dispatch and the capture freezes that for the
    /// process lifetime, silently dropping every plugin emit. The
    /// debug-build `assert` below makes the ordering violation
    /// loud; release builds rely on the documented invariant.
    fn host_dispatch_for_self() -> &'static Dispatch {
        HOST_TRACING_DISPATCH.get_or_init(|| {
            debug_assert!(
                tracing::dispatcher::has_been_set(),
                "host_dispatch_for_self called before any global tracing dispatch \
                 was installed — Runner::new must run before plugin loading so \
                 HostServices captures a configured Dispatch rather than the noop default"
            );
            let dispatch = tracing::dispatcher::get_default(|d| d.clone());
            Box::leak(Box::new(dispatch))
        })
    }

    /// Return a `&'static StaticLogRef` pointing at the host's
    /// iceoryx2 logger bridge.
    fn host_iceoryx2_logger_for_self() -> &'static StaticLogRef {
        HOST_ICEORYX2_LOGGER.get_or_init(|| {
            let logger: StaticLogRef = &crate::core::logging::iceoryx2_log_bridge::HOST_BRIDGE;
            Box::leak(Box::new(logger))
        })
    }

    /// Return a `&'static Iceoryx2Node` pointing at a clone of the
    /// caller's node. Captured on first call.
    fn host_iceoryx2_node_for_self(
        node: &crate::iceoryx2::Iceoryx2Node,
    ) -> &'static crate::iceoryx2::Iceoryx2Node {
        HOST_ICEORYX2_NODE.get_or_init(|| Box::leak(Box::new(node.clone())))
    }

    /// Build a [`HostServices`] payload from the host's own statics.
    /// Heap-leaks the underlying `Dispatch`, `&dyn Log`, and
    /// `Iceoryx2Node` values on first call; pointers are stable for
    /// the process lifetime, which matches `LOADED_PLUGIN_LIBRARIES`'s
    /// pinning lifetime for loaded cdylibs.
    ///
    /// Returns by value; the caller binds it to a local and passes
    /// `&services as *const HostServices as *const c_void` into the
    /// cdylib's register callback. For multi-plugin loaders that
    /// retain the payload past one call site, prefer
    /// [`leaked_host_services_for_self`].
    pub fn host_services_for_self(node: &crate::iceoryx2::Iceoryx2Node) -> HostServices {
        let dispatch_ptr = host_dispatch_for_self() as *const Dispatch as *const c_void;
        let logger_ptr = host_iceoryx2_logger_for_self() as *const StaticLogRef as *const c_void;
        let node_ptr = host_iceoryx2_node_for_self(node) as *const crate::iceoryx2::Iceoryx2Node
            as *const c_void;

        HostServices {
            abi_layout_version: HOST_SERVICES_LAYOUT_VERSION,
            _reserved_padding: 0,
            processor_registry: &*crate::core::processors::PROCESSOR_REGISTRY
                as *const crate::core::processors::ProcessorInstanceFactory
                as *const c_void,
            pubsub: crate::core::pubsub::local_pubsub() as *const _ as *const c_void,
            schema_registry: crate::core::embedded_schemas::local_schema_registry()
                as *const _ as *const c_void,
            tracing_dispatch_ptr: dispatch_ptr,
            iceoryx2_logger_ptr: logger_ptr,
            iceoryx2_node_ptr: node_ptr,
        }
    }

    /// Same as [`host_services_for_self`] but returns a
    /// `&'static HostServices` keyed off the host's iceoryx2 node —
    /// constructed once per process and reused across every dlopen
    /// from this host.
    pub fn leaked_host_services_for_self(
        node: &crate::iceoryx2::Iceoryx2Node,
    ) -> &'static HostServices {
        static LEAKED: OnceLock<&'static HostServices> = OnceLock::new();
        LEAKED.get_or_init(|| Box::leak(Box::new(host_services_for_self(node))))
    }
}
