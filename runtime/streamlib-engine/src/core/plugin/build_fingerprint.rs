// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine build identity for the plugin-load handshake.
//!
//! Carried in the typed error the host raises when it refuses a plugin
//! whose `#[repr(C)]` dispatch-surface fingerprint
//! ([`streamlib_plugin_abi::PLUGIN_ABI_LAYOUT_FINGERPRINT`]) diverges
//! from its own, so the operator sees both build identities.

/// Human-readable identity of this engine build: engine version, rustc
/// version, target triple, and build profile.
pub const BUILD_IDENTITY: &str = concat!(
    "streamlib-engine ",
    env!("CARGO_PKG_VERSION"),
    " / ",
    env!("STREAMLIB_RUSTC_VERSION"),
    " / ",
    env!("STREAMLIB_HOST_TARGET"),
    " / ",
    env!("STREAMLIB_BUILD_PROFILE"),
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_identity_carries_version_and_triple() {
        assert!(BUILD_IDENTITY.contains(env!("CARGO_PKG_VERSION")));
        assert!(BUILD_IDENTITY.contains(env!("STREAMLIB_HOST_TARGET")));
        assert!(BUILD_IDENTITY.starts_with("streamlib-engine "));
    }
}
