// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-DSO plugin bridging.
//!
//! Rust plugin cdylibs loaded via `Runner::load_project` /
//! `Runner::load_package` (or the standalone `streamlib-runtime`
//! binary's `--plugin` flag) statically embed their entire transitive
//! dep tree. Without bridging, every process-wide static the engine
//! relies on (tracing dispatch, [`PUBSUB`], the schema registry,
//! iceoryx2's logger) exists as a per-DSO copy with no
//! dynamic-linker dedup — Rust mangled statics aren't in the dynsym
//! table.
//!
//! This module owns the typed [`HostServices`] payload the host
//! hands to plugin cdylibs through `STREAMLIB_PLUGIN.register`, and
//! the cdylib-side [`install_host_services`] helper that bridges
//! every static to the host's instances.
//!
//! [`PUBSUB`]: crate::core::pubsub::PUBSUB

pub(crate) mod forwarding_subscriber;
pub mod host_services;
pub(crate) mod iceoryx2_log_forwarder;

pub use host_services::{install_host_services, RegisterHelper};
pub use streamlib_plugin_abi::{HostServices, HOST_SERVICES_LAYOUT_VERSION};
