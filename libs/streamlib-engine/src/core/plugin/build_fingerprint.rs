// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine build fingerprint for the plugin-load handshake.
//!
//! The host refuses, with a typed error, any plugin cdylib whose
//! engine-internal transit surface could skew from its own. That
//! surface is the three non-`#[repr(C)]` engine types the FullAccess
//! vtable transits by raw `Arc` pointer (`HostVulkanDevice`,
//! `HostVulkanTexture`, `HostVulkanTimelineSemaphore`); a
//! separately-built plugin whose `repr(Rust)` layout differs reads them
//! at the wrong offsets and corrupts the GPU driver
//! (`docs/learnings/slpkg-raw-device-rhi-construction.md`).

use streamlib_plugin_abi::{
    fingerprint_fold_bytes, fingerprint_fold_u64, fingerprint_init,
    PLUGIN_ABI_LAYOUT_FINGERPRINT,
};

/// Human-readable identity of this engine build: engine version, rustc
/// version, target triple, and build profile. Carried in the error
/// message when a plugin is refused so the operator sees both build
/// identities. rustc version and profile appear here — and *only* here,
/// never in [`ENGINE_TRANSIT_FINGERPRINT`] — because identical measured
/// layouts across rustc releases are compatible and a debug plugin
/// loading into a release host is legitimate.
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

/// Structural fingerprint of the engine's plugin-ABI transit surface.
///
/// Folds, in order:
/// 1. [`PLUGIN_ABI_LAYOUT_FINGERPRINT`] — the `#[repr(C)]` dispatch
///    surface (binds this to a matching wire ABI).
/// 2. The engine crate version and the `streamlib-consumer-rhi` version
///    — refusing cross-engine-version loads even when layouts coincide,
///    because the transit types are engine-internal source that can
///    reshape between versions without any wire-ABI signal.
/// 3. On Linux, the first-order (`size_of` / `align_of`) layout of each
///    of the three raw-`Arc` transit types.
///
/// **Residual soundness gap (documented, not closed here):** the
/// first-order probe catches a transit type whose *size or alignment*
/// skews across two builds (the dominant mode — a divergent transitive
/// dependency reshaping a field type). It does not catch a pure
/// reorder-at-identical-size, which `repr(Rust)` permits across rustc
/// releases. The engine-version fold narrows the survivable window to
/// "same engine version, different transitive resolution, same
/// first-order layout"; the fully sound fix is the PluginAbiObject lift
/// of the remaining raw-`Arc` slots, which removes the transit entirely.
pub const ENGINE_TRANSIT_FINGERPRINT: u64 = compute_engine_transit_fingerprint();

const fn compute_engine_transit_fingerprint() -> u64 {
    let mut hash = fingerprint_init();
    hash = fingerprint_fold_u64(hash, PLUGIN_ABI_LAYOUT_FINGERPRINT);
    hash = fingerprint_fold_bytes(hash, env!("CARGO_PKG_VERSION").as_bytes());
    hash = fingerprint_fold_bytes(hash, streamlib_consumer_rhi::VERSION.as_bytes());
    fold_transit_layouts(hash)
}

#[cfg(target_os = "linux")]
const fn fold_transit_layouts(mut hash: u64) -> u64 {
    let device = crate::vulkan::rhi::host_vulkan_device_layout_probe();
    hash = fingerprint_fold_u64(hash, device[0]);
    hash = fingerprint_fold_u64(hash, device[1]);
    let texture = crate::vulkan::rhi::host_vulkan_texture_layout_probe();
    hash = fingerprint_fold_u64(hash, texture[0]);
    hash = fingerprint_fold_u64(hash, texture[1]);
    let timeline = crate::vulkan::rhi::host_vulkan_timeline_semaphore_layout_probe();
    hash = fingerprint_fold_u64(hash, timeline[0]);
    hash = fingerprint_fold_u64(hash, timeline[1]);
    hash
}

#[cfg(not(target_os = "linux"))]
const fn fold_transit_layouts(hash: u64) -> u64 {
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_transit_fingerprint_is_nonzero() {
        // A degenerate zero fold would accept every plugin; the
        // `engine_transit_fingerprint == 0` sentinel means "engine-free
        // plugin", so a host whose real value collided with 0 would
        // accept engine-linked plugins built against a different engine.
        assert_ne!(ENGINE_TRANSIT_FINGERPRINT, 0);
    }

    #[test]
    fn engine_transit_fingerprint_differs_from_abi_fingerprint() {
        // The transit fingerprint folds the abi fingerprint plus more;
        // if they were equal the extra folds were no-ops.
        assert_ne!(ENGINE_TRANSIT_FINGERPRINT, PLUGIN_ABI_LAYOUT_FINGERPRINT);
    }

    #[test]
    fn build_identity_carries_version_and_triple() {
        assert!(BUILD_IDENTITY.contains(env!("CARGO_PKG_VERSION")));
        assert!(BUILD_IDENTITY.contains(env!("STREAMLIB_HOST_TARGET")));
        assert!(BUILD_IDENTITY.starts_with("streamlib-engine "));
    }
}
