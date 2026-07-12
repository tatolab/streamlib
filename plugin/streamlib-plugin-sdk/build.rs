// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Captures the target triple, rustc version, and build profile so the
//! engine-free plugin SDK can expose a human-readable build-identity
//! string (`sdk::plugin::BUILD_IDENTITY`) for the plugin-load
//! fingerprint handshake. Mirrors the engine build.rs's identity
//! capture; the engine-free SDK carries no transit surface, so its
//! `ENGINE_TRANSIT_FINGERPRINT` is a plain `0` and needs no build-time
//! probe.

fn main() {
    let target = std::env::var("TARGET").expect("TARGET env var set by cargo for build.rs");
    println!("cargo:rustc-env=STREAMLIB_HOST_TARGET={}", target);
    println!("cargo:rerun-if-env-changed=TARGET");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = std::process::Command::new(&rustc)
        .arg("-V")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown-rustc".to_string());
    println!("cargo:rustc-env=STREAMLIB_RUSTC_VERSION={}", rustc_version);

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=STREAMLIB_BUILD_PROFILE={}", profile);
}
