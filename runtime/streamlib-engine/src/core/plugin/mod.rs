// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plugin ABI bridging.
//!
//! Rust plugin cdylibs loaded via `Runner::add_module`
//! statically embed their entire transitive
//! dep tree. Without bridging, every process-wide static the engine
//! relies on (tracing dispatch, [`PUBSUB`], the schema registry,
//! iceoryx2's logger) exists as a per-plugin copy with no
//! dynamic-linker dedup — Rust mangled statics aren't in the dynsym
//! table.
//!
//! This module owns the typed [`HostServices`] payload the host
//! hands to plugin cdylibs through `STREAMLIB_PLUGIN.register`, and
//! the cdylib-side [`install_host_services`] helper that bridges
//! every static to the host's instances.
//!
//! [`PUBSUB`]: crate::core::pubsub::PUBSUB

pub mod build_fingerprint;
pub(crate) mod forwarding_subscriber;
pub mod host_services;
pub(crate) mod iceoryx2_log_forwarder;
pub(crate) mod processor_vtable;
/// CI drift guard: fails if the engine↔SDK twin marshalling copies diverge.
#[cfg(test)]
mod twin_drift_guard;

pub use host_services::{RegisterHelper, install_host_services};
pub use streamlib_plugin_abi::{HOST_SERVICES_LAYOUT_VERSION, HostServices};

// Build-fingerprint handshake surface. The facade `streamlib` SDK's
// `sdk::plugin` re-exports `core::plugin`, so the `#[processor]` macro
// resolves these names against the plugin's build; the engine-free
// `streamlib-plugin-sdk` exports the same names.
pub use build_fingerprint::BUILD_IDENTITY;
pub use streamlib_plugin_abi::PLUGIN_ABI_LAYOUT_FINGERPRINT;
