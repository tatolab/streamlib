// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-load rejection test for the plugin build-fingerprint handshake.
//!
//! Builds the `streamlib-test-fixtures-abi-mismatch` cdylib once per
//! tamper direction, assembles a minimal project directory, calls
//! `runtime.add_module_with_blocking(_, Strategy::Path)`, and asserts
//! the returned error is the typed variant for that direction — the
//! plugin's no-op `register` stub is never invoked:
//!
//! - `tamper-too-low` (`abi_version = 0`) and `tamper-too-high`
//!   (`abi_version = u32::MAX`) → `Error::PluginAbiVersionMismatch`.
//!   Both directions lock the *equality* check — a future `<` or `>`
//!   regression would only catch one.
//! - `tamper-abi-layout-fingerprint` (correct `abi_version` +
//!   bit-flipped `abi_layout_fingerprint`) → `Error::PluginBuildMismatch`,
//!   whose Display names both build identities + the rebuild remedy. A
//!   plugin built against a divergent `#[repr(C)]` dispatch surface is
//!   refused before `register` runs.
//!
//! Mental-revert: removing either check in
//! `validate_plugin_declaration` would let the runtime invoke the
//! fixture's no-op `register` stub, which never registers the
//! processor. The runtime's subsequent "processor not registered"
//! check would then surface a different error, regressing the contract
//! this test locks.
//!
//! No GPU required.

use std::path::Path;

use serial_test::serial;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Error;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::runtime::{BuildPolicy, Runner, Strategy};
use streamlib_engine::core::runtime::host_target_triple;

fn copy_dir_contents(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_entry = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_contents(&entry.path(), &dst_entry);
        } else {
            std::fs::copy(entry.path(), &dst_entry).unwrap();
        }
    }
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

fn build_tampered_cdylib(feature: &str) -> std::path::PathBuf {
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "-p",
            "streamlib-test-fixtures-abi-mismatch",
            "--no-default-features",
            "--features",
            feature,
        ])
        .status()
        .expect("invoking cargo build");
    assert!(
        status.success(),
        "cargo build of streamlib-test-fixtures-abi-mismatch --features {feature} must succeed"
    );

    let dylib_ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let dylib_name = format!("libstreamlib_test_fixtures_abi_mismatch.{}", dylib_ext);
    workspace_root()
        .join("target")
        .join("debug")
        .join(&dylib_name)
}

/// Build a project directory under `tmp` containing the fixture
/// crate's `streamlib.yaml` (plus the `@tatolab/core` patch dep)
/// and the tampered cdylib copied into `lib/<host_triple>/`.
fn stage_project(tmp: &Path, source_dylib: &Path) -> std::path::PathBuf {
    let fixture_src = workspace_root().join("packages/test-fixtures-abi-mismatch");
    let core_src = workspace_root().join("packages/core");
    let fixture_dst = tmp.join("test-fixtures-abi-mismatch");
    let core_dst = tmp.join("core");

    std::fs::create_dir_all(&fixture_dst).unwrap();
    std::fs::copy(
        fixture_src.join("streamlib.yaml"),
        fixture_dst.join("streamlib.yaml"),
    )
    .unwrap();

    std::fs::create_dir_all(&core_dst).unwrap();
    std::fs::copy(
        core_src.join("streamlib.yaml"),
        core_dst.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_contents(&core_src.join("schemas"), &core_dst.join("schemas"));

    let triple_dir = fixture_dst.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    let dylib_dst = triple_dir.join(source_dylib.file_name().unwrap());
    std::fs::copy(source_dylib, &dylib_dst).unwrap();

    fixture_dst
}

fn load_tampered_module(project_dir: &Path) -> Error {
    let runtime = Runner::with_auto_build().expect("Runner::new");
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures-abi-mismatch"),
            Strategy::Path {
                path: project_dir.to_path_buf(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect_err("add_module_with must REJECT a tampered declaration")
        .into()
}

fn assert_abi_version_mismatch_rejected(project_dir: &Path) {
    let err = load_tampered_module(project_dir);
    match &err {
        Error::PluginAbiVersionMismatch { .. } => {}
        other => panic!("expected Error::PluginAbiVersionMismatch, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains("ABI version mismatch"),
        "rejection message must call out the abi_version mismatch — got: {msg}"
    );
    assert!(
        msg.contains("Rebuild the plugin"),
        "rejection message must include the rebuild remedy — got: {msg}"
    );
}

#[test]
#[serial]
fn load_project_rejects_abi_version_below_runtime() {
    let dylib = build_tampered_cdylib("tamper-too-low");
    let tmp = tempfile::tempdir().unwrap();
    let project = stage_project(tmp.path(), &dylib);

    assert_abi_version_mismatch_rejected(&project);
}

#[test]
#[serial]
fn load_project_rejects_abi_version_above_runtime() {
    let dylib = build_tampered_cdylib("tamper-too-high");
    let tmp = tempfile::tempdir().unwrap();
    let project = stage_project(tmp.path(), &dylib);

    assert_abi_version_mismatch_rejected(&project);
}

#[test]
#[serial]
fn load_project_rejects_mismatched_abi_layout_fingerprint() {
    // Correct abi_version but a bit-flipped abi_layout_fingerprint: the
    // host must reject with PluginBuildMismatch BEFORE invoking
    // `register` — no segfault, no abort, a clean typed error naming
    // both build identities.
    let dylib = build_tampered_cdylib("tamper-abi-layout-fingerprint");
    let tmp = tempfile::tempdir().unwrap();
    let project = stage_project(tmp.path(), &dylib);

    let err = load_tampered_module(&project);
    match &err {
        Error::PluginBuildMismatch {
            plugin_identity,
            host_identity,
            ..
        } => {
            assert_eq!(
                plugin_identity, "tampered-fixture-build",
                "the plugin's build identity must be surfaced verbatim"
            );
            assert!(
                host_identity.starts_with("streamlib-engine "),
                "the host's build identity must be surfaced — got: {host_identity}"
            );
        }
        other => panic!("expected Error::PluginBuildMismatch, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains("Plugin build mismatch"),
        "rejection message must call out the build mismatch — got: {msg}"
    );
    assert!(
        msg.contains("tampered-fixture-build"),
        "rejection message must name the plugin build identity — got: {msg}"
    );
    assert!(
        msg.contains("Rebuild the plugin"),
        "rejection message must include the rebuild remedy — got: {msg}"
    );
}
