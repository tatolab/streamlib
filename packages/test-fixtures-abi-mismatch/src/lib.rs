// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test-only cdylib emitting a hand-crafted `STREAMLIB_PLUGIN`
//! declaration with a tampered field, so the load-path rejection can be
//! tested end-to-end. The `register` callback is a no-op stub the host
//! never invokes — one of the host's `validate_plugin_declaration`
//! checks fires first and rejects with a typed error.
//!
//! The tamper direction is feature-gated (exactly one at a time):
//! - `tamper-too-low` (default) → `abi_version = 0`
//!   → [`streamlib_error::Error::PluginAbiVersionMismatch`]
//! - `tamper-too-high`          → `abi_version = u32::MAX`
//!   → [`streamlib_error::Error::PluginAbiVersionMismatch`]
//! - `tamper-transit-fingerprint` → correct `abi_version` +
//!   correct `abi_layout_fingerprint` (computed from this crate's own
//!   `streamlib-plugin-abi` dep) + a bit-flipped, non-zero
//!   `engine_transit_fingerprint`
//!   → [`streamlib_error::Error::PluginBuildMismatch`]
//!
//! The three directions cover the two typed error variants and the
//! full check order in `validate_plugin_declaration`.

#![cfg(target_os = "linux")]

use streamlib_plugin_abi::PluginDeclaration;

// Exactly one tamper-* feature may be enabled at a time. Cargo's
// feature unification can land more than one on by accident — fail
// loudly during the cdylib build rather than emit silently-wrong bytes.
#[cfg(all(feature = "tamper-too-low", feature = "tamper-too-high"))]
compile_error!(
    "streamlib-test-fixtures-abi-mismatch: `tamper-too-low` and \
     `tamper-too-high` are mutually exclusive."
);
#[cfg(all(feature = "tamper-too-low", feature = "tamper-transit-fingerprint"))]
compile_error!(
    "streamlib-test-fixtures-abi-mismatch: `tamper-too-low` and \
     `tamper-transit-fingerprint` are mutually exclusive."
);
#[cfg(all(feature = "tamper-too-high", feature = "tamper-transit-fingerprint"))]
compile_error!(
    "streamlib-test-fixtures-abi-mismatch: `tamper-too-high` and \
     `tamper-transit-fingerprint` are mutually exclusive."
);
#[cfg(not(any(
    feature = "tamper-too-low",
    feature = "tamper-too-high",
    feature = "tamper-transit-fingerprint"
)))]
compile_error!(
    "streamlib-test-fixtures-abi-mismatch: enable exactly one of \
     `tamper-too-low` (default), `tamper-too-high`, or \
     `tamper-transit-fingerprint`."
);

// Per-direction tamper values. Fields the host never reads for a given
// direction (because an earlier check fires) are left at benign zeros.
#[cfg(feature = "tamper-too-low")]
mod tamper {
    pub const ABI_VERSION: u32 = 0;
    pub const ABI_LAYOUT_FINGERPRINT: u64 = 0;
    pub const ENGINE_TRANSIT_FINGERPRINT: u64 = 0;
    pub const BUILD_IDENTITY: &str = "tamper-too-low fixture";
}

#[cfg(feature = "tamper-too-high")]
mod tamper {
    pub const ABI_VERSION: u32 = u32::MAX;
    pub const ABI_LAYOUT_FINGERPRINT: u64 = 0;
    pub const ENGINE_TRANSIT_FINGERPRINT: u64 = 0;
    pub const BUILD_IDENTITY: &str = "tamper-too-high fixture";
}

#[cfg(feature = "tamper-transit-fingerprint")]
mod tamper {
    // Correct wire ABI version + correct dispatch-surface fingerprint
    // (both from this crate's own streamlib-plugin-abi dep — the same
    // workspace crate the host links), so the `abi_version` and
    // `abi_layout_fingerprint` checks pass and the host reaches the
    // engine-transit check, which must reject the bit-flipped value.
    pub const ABI_VERSION: u32 = streamlib_plugin_abi::STREAMLIB_ABI_VERSION;
    pub const ABI_LAYOUT_FINGERPRINT: u64 =
        streamlib_plugin_abi::PLUGIN_ABI_LAYOUT_FINGERPRINT;
    // Non-zero (so NOT the engine-free sentinel) and guaranteed not to
    // match any real host engine transit fingerprint.
    pub const ENGINE_TRANSIT_FINGERPRINT: u64 = 0xDEAD_BEEF_DEAD_BEEF;
    pub const BUILD_IDENTITY: &str = "tampered-fixture-build";
}

/// No-op register callback. The host rejects the plugin BEFORE invoking
/// `register`, so this body is unreachable in practice. We keep it valid
/// (rather than `unreachable!()`) so a future change to the check order
/// doesn't turn this stub into UB.
unsafe extern "C" fn __streamlib_plugin_register_stub(
    _host_services: *const ::core::ffi::c_void,
) {
    // Intentionally empty — no host services touched, no processors
    // registered.
}

/// Hand-crafted `STREAMLIB_PLUGIN` static — bypasses
/// [`streamlib_plugin_abi::export_plugin!`] (which would always emit the
/// current, correct fingerprints) so a single field can be tampered.
#[unsafe(no_mangle)]
pub static STREAMLIB_PLUGIN: PluginDeclaration = PluginDeclaration {
    abi_version: tamper::ABI_VERSION,
    _reserved_padding: 0,
    register: __streamlib_plugin_register_stub,
    abi_layout_fingerprint: tamper::ABI_LAYOUT_FINGERPRINT,
    engine_transit_fingerprint: tamper::ENGINE_TRANSIT_FINGERPRINT,
    build_identity_ptr: tamper::BUILD_IDENTITY.as_ptr(),
    build_identity_len: tamper::BUILD_IDENTITY.len(),
};
