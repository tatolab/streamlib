// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Load-project rejection test for the ABI-version mismatch path.
//!
//! Builds the `streamlib-test-fixtures-abi-mismatch` cdylib once per
//! mismatch direction (`tamper-too-low` → `abi_version = 0`;
//! `tamper-too-high` → `abi_version = u32::MAX`), assembles a minimal
//! project directory, calls `runtime.load_project`, and asserts the
//! returned error is `Error::Configuration` with the documented
//! "ABI version mismatch" prefix.
//!
//! Both directions are covered to lock the equality check in
//! `core/runtime/runtime.rs` — a future `<` or `>` regression would
//! only catch one direction.
//!
//! Mental-revert: removing the `if decl.abi_version != STREAMLIB_ABI_VERSION`
//! check in `load_project` would let the runtime invoke the fixture's
//! no-op `register` stub, which never registers `AbiMismatchSentinel`.
//! The runtime's subsequent "processor not registered" check would
//! then surface a different error, regressing the contract this test
//! locks.
//!
//! No GPU required.

use std::path::Path;

use serial_test::serial;
use streamlib::sdk::error::Error;
use streamlib::sdk::runtime::Runner;
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
    let dylib_name = format!(
        "libstreamlib_test_fixtures_abi_mismatch.{}",
        dylib_ext
    );
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

fn assert_abi_mismatch_rejected(project_dir: &Path) {
    let runtime = Runner::new().expect("Runner::new");
    let err = runtime
        .load_project(project_dir)
        .expect_err("load_project must REJECT a tampered abi_version");

    let msg = match err {
        Error::Configuration(s) => s,
        other => panic!(
            "expected Error::Configuration with 'ABI version mismatch', \
             got {other:?}"
        ),
    };

    assert!(
        msg.contains("ABI version mismatch"),
        "rejection message must call out the abi_version mismatch — got: {msg}"
    );
    assert!(
        msg.contains("Rebuild the plugin"),
        "rejection message must include the rebuild hint — got: {msg}"
    );
}

#[test]
#[serial]
fn load_project_rejects_abi_version_below_runtime() {
    let dylib = build_tampered_cdylib("tamper-too-low");
    let tmp = tempfile::tempdir().unwrap();
    let project = stage_project(tmp.path(), &dylib);

    assert_abi_mismatch_rejected(&project);
}

#[test]
#[serial]
fn load_project_rejects_abi_version_above_runtime() {
    let dylib = build_tampered_cdylib("tamper-too-high");
    let tmp = tempfile::tempdir().unwrap();
    let project = stage_project(tmp.path(), &dylib);

    assert_abi_mismatch_rejected(&project);
}
