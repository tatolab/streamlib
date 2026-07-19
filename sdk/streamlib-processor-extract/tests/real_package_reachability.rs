// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reachability resolution over a REAL in-tree package.
//!
//! `@tatolab/camera` is the canonical over-collection case: its Linux arm
//! (`src/linux/camera.rs`) and its parked Apple arm
//! (`src/_apple_impl_pending_/camera.rs`) BOTH declare a `@tatolab/camera/Camera`
//! processor. The parked arm is gated `#[cfg(any())]` in `src/lib.rs`, so it
//! never compiles on any target. This locks that:
//!
//! - the raw whole-tree scan over-collects (two `Camera`s), and
//! - the reachability-resolved scan for a Linux target yields exactly the set
//!   the package's committed `processors:` manifest lists — no parked duplicate.

use std::path::PathBuf;

use streamlib_processor_extract::{
    ModuleReachabilityTarget, extract_reachable_rust_processors, extract_rust_processors,
};

fn camera_package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("packages")
        .join("camera")
}

fn linux_target() -> ModuleReachabilityTarget {
    ModuleReachabilityTarget::new()
        .with_key_value("target_os", "linux")
        .with_key_value("target_arch", "x86_64")
        .with_key_value("target_family", "unix")
        .with_flag("unix")
}

fn sorted_names(procs: Vec<streamlib_processor_extract::ExtractedProcessor>) -> Vec<String> {
    let mut names: Vec<String> = procs.into_iter().map(|p| p.schema.name).collect();
    names.sort();
    names
}

#[test]
fn raw_scan_over_collects_the_parked_apple_arm() {
    let dir = camera_package_dir();
    if !dir.join("src").is_dir() {
        // The in-tree package moved; skip rather than fail a layout-coupled test.
        return;
    }
    let names = sorted_names(extract_rust_processors(&dir).unwrap());
    // Both the Linux and the parked Apple arm declare `Camera` → duplicate.
    let camera_count = names.iter().filter(|n| n.as_str() == "Camera").count();
    assert!(
        camera_count >= 2,
        "raw scan should over-collect the parked Apple `Camera`; got {names:?}"
    );
}

#[test]
fn reachable_scan_matches_the_committed_manifest() {
    let dir = camera_package_dir();
    if !dir.join("src").is_dir() {
        return;
    }
    let names = sorted_names(extract_reachable_rust_processors(&dir, &linux_target()).unwrap());
    assert_eq!(
        names,
        vec!["Camera".to_string(), "CameraToCudaCopy".to_string()],
        "reachable Linux scan must equal the package's `processors:` set, \
         with the parked Apple `Camera` excluded"
    );
}
