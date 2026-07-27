// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reachability resolution over REAL in-tree packages.
//!
//! `@tatolab/camera` is the canonical over-collection case: its Linux arm
//! (`processors/linux/camera.rs`) and its parked Apple arm
//! (`processors/_apple_impl_pending_/camera.rs`) BOTH declare a
//! `@tatolab/camera/Camera` processor. The parked arm carries a file-level
//! `#![cfg(any())]`, so it never compiles on any target. This locks that:
//!
//! - the raw whole-tree scan over-collects (two `Camera`s), and
//! - the reachability-resolved scan yields exactly the set the package's
//!   committed `processors:` manifest lists, per target — no parked duplicate.
//!
//! Every assertion here is hard. A layout-coupled test that returns early when
//! the layout it couples to is absent goes green the instant the source moves,
//! which is exactly when it should be loudest.

use std::path::{Path, PathBuf};

use streamlib_processor_extract::{
    ModuleReachabilityTarget, extract_reachable_rust_processors, extract_rust_processors,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn package_dir(name: &str) -> PathBuf {
    let dir = workspace_root().join("packages").join(name);
    assert!(
        dir.join("processors").is_dir(),
        "packages/{name} must author its processor modules under `processors/` — \
         folder-backed discovery scans no other root, so a moved package would \
         derive the empty set"
    );
    dir
}

fn target_for(os: &str) -> ModuleReachabilityTarget {
    ModuleReachabilityTarget::new()
        .with_key_value("target_os", os)
        .with_key_value("target_arch", "x86_64")
        .with_key_value(
            "target_family",
            if os == "windows" { "windows" } else { "unix" },
        )
        .with_flag(if os == "windows" { "windows" } else { "unix" })
}

fn sorted_names(procs: Vec<streamlib_processor_extract::ExtractedProcessor>) -> Vec<String> {
    let mut names: Vec<String> = procs.into_iter().map(|p| p.schema.name).collect();
    names.sort();
    names
}

fn reachable_names(dir: &Path, os: &str) -> Vec<String> {
    sorted_names(extract_reachable_rust_processors(dir, &target_for(os)).unwrap())
}

#[test]
fn raw_scan_over_collects_the_parked_apple_arm() {
    let dir = package_dir("camera");
    let names = sorted_names(extract_rust_processors(&dir).unwrap());
    // Both the Linux and the parked Apple arm declare `Camera` → duplicate.
    let camera_count = names.iter().filter(|n| n.as_str() == "Camera").count();
    assert!(
        camera_count >= 2,
        "raw scan should over-collect the parked Apple `Camera`; got {names:?}"
    );
}

#[test]
fn camera_reachable_scan_matches_the_committed_manifest_on_linux() {
    let names = reachable_names(&package_dir("camera"), "linux");
    assert_eq!(
        names,
        vec!["Camera".to_string(), "CameraToCudaCopy".to_string()],
        "reachable Linux scan must equal the package's `processors:` set, \
         with the parked Apple `Camera` excluded"
    );
}

/// Behavior delta pinned: `@tatolab/camera` on macOS/iOS used to emit no
/// `STREAMLIB_PLUGIN` at all (its whole `export_plugin!` was
/// `#[cfg(target_os = "linux")]`), so the loader rejected it with a
/// missing-symbol error. The unconditional `CameraToCudaCopy` arm now carries
/// the declaration, so the package loads and the processor fails in `setup()`
/// with its own configuration error instead.
#[test]
fn camera_keeps_its_unconditional_arm_on_apple_targets() {
    for os in ["macos", "ios"] {
        assert_eq!(
            reachable_names(&package_dir("camera"), os),
            vec!["CameraToCudaCopy".to_string()],
            "camera must still declare its cross-platform arm on {os}"
        );
    }
}

/// Behavior delta pinned: `@tatolab/audio` gated its whole `export_plugin!` on
/// `any(linux, macos, ios)`, so a Windows build registered 0 processors even
/// though 5 of the 7 are platform-free. Mirroring each arm's own `#[cfg]`
/// surfaces exactly those 5.
#[test]
fn audio_declares_its_platform_free_arms_on_windows() {
    assert_eq!(
        reachable_names(&package_dir("audio"), "windows"),
        vec![
            "AudioChannelConverter".to_string(),
            "AudioMixer".to_string(),
            "AudioResampler".to_string(),
            "BufferRechunker".to_string(),
            "ChordGenerator".to_string(),
        ],
        "the five platform-free audio processors must survive on Windows"
    );
}

/// The Linux and Apple audio arms both declare `AudioCapture` / `AudioOutput`;
/// only the target's arm may surface.
#[test]
fn audio_platform_arms_resolve_to_one_arm_per_target() {
    assert_eq!(
        reachable_names(&package_dir("audio"), "linux"),
        reachable_names(&package_dir("audio"), "macos"),
        "both platform arms must declare the same processor identities — a \
         divergence here means one arm silently ships a different set"
    );
    let linux = reachable_names(&package_dir("audio"), "linux");
    assert_eq!(linux.iter().filter(|n| *n == "AudioCapture").count(), 1);
    assert_eq!(linux.iter().filter(|n| *n == "AudioOutput").count(), 1);
}

/// `@tatolab/screen-capture`'s only processor is parked, so it declares none on
/// any target — which is why its generated crate root emits no `export_plugin!`
/// and its cdylib carries no `STREAMLIB_PLUGIN` symbol, unchanged from before
/// folder-backed discovery.
#[test]
fn a_fully_parked_package_declares_no_processor_on_any_target() {
    for os in ["linux", "macos", "windows"] {
        assert!(
            reachable_names(&package_dir("screen-capture"), os).is_empty(),
            "screen-capture must declare no processor on {os}"
        );
    }
}
