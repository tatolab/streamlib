// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test-only cdylib emitting `STREAMLIB_PLUGIN` with a tampered
//! `abi_version` so the load_project rejection path can be tested
//! end-to-end. The `register` callback is a no-op stub the host never
//! invokes — the runtime's `abi_version` mismatch check fires first
//! and rejects with `Error::Configuration`.
//!
//! The mismatch direction is feature-gated:
//! - `tamper-too-low` (default) → `abi_version = 0`
//! - `tamper-too-high`          → `abi_version = u32::MAX`
//!
//! Both directions exist so the integration test covers
//! `decl.abi_version < STREAMLIB_ABI_VERSION` and
//! `> STREAMLIB_ABI_VERSION` independently, locking the equality check
//! in `runtime.rs` (a future `<` or `>` regression would only catch one
//! direction).

#![cfg(target_os = "linux")]

use streamlib_plugin_abi::PluginDeclaration;

// At most one tamper-* feature may be enabled at a time. Cargo's
// feature unification can land both on by accident — fail loudly
// during the cdylib build rather than emit silently-wrong bytes.
#[cfg(all(feature = "tamper-too-low", feature = "tamper-too-high"))]
compile_error!(
    "streamlib-test-fixtures-abi-mismatch: features `tamper-too-low` and \
     `tamper-too-high` are mutually exclusive. Disable default features and \
     pick exactly one."
);

#[cfg(not(any(feature = "tamper-too-low", feature = "tamper-too-high")))]
compile_error!(
    "streamlib-test-fixtures-abi-mismatch: enable exactly one of \
     `tamper-too-low` (default) or `tamper-too-high`."
);

#[cfg(feature = "tamper-too-low")]
const TAMPERED_ABI_VERSION: u32 = 0;
#[cfg(feature = "tamper-too-high")]
const TAMPERED_ABI_VERSION: u32 = u32::MAX;

/// No-op register callback. The runtime's abi-version check rejects
/// the plugin BEFORE invoking `register`, so this body is unreachable
/// in practice. We keep it valid (rather than `unreachable!()`) so a
/// future runtime change that decoupled the order of checks doesn't
/// turn this stub into UB.
unsafe extern "C" fn __streamlib_plugin_register_stub(
    _host_services: *const ::core::ffi::c_void,
) {
    // Intentionally empty — no host services touched, no processors
    // registered.
}

/// Hand-crafted `STREAMLIB_PLUGIN` static — bypasses
/// [`streamlib_plugin_abi::export_plugin!`] which would always use the
/// current `STREAMLIB_ABI_VERSION`. The runtime's loader reads this
/// `abi_version` field directly and rejects the mismatch.
#[unsafe(no_mangle)]
pub static STREAMLIB_PLUGIN: PluginDeclaration = PluginDeclaration {
    abi_version: TAMPERED_ABI_VERSION,
    register: __streamlib_plugin_register_stub,
};
