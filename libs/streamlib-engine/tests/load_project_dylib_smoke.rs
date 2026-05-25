// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Smoke test for the dynamic Rust-plugin load path: build a real
//! `packages/test-fixtures` cdylib, stage it next to its manifest in a
//! tempdir, call `runtime.load_project(...)`, and assert
//! `TestConfiguredProcessor` registered via the `STREAMLIB_PLUGIN`
//! callback. Mentally revert the `export_plugin!` invocation in
//! `packages/test-fixtures/src/lib.rs` and this test fails — the
//! dlopen path validates the processor was registered by the dylib
//! and surfaces a `Configuration` error when it wasn't.

use std::path::Path;

use serial_test::serial;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
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

#[test]
#[serial]
fn load_project_real_dylib_registers_processor_via_export_plugin() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    // Build test-fixtures with the `plugin` feature so the cdylib carries
    // the `STREAMLIB_PLUGIN` symbol. Default-off — see the feature note
    // in `packages/test-fixtures/Cargo.toml`. Idempotent: cargo's
    // incremental machinery skips the rebuild on a warm tree.
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "-p",
            "streamlib-test-fixtures",
        ])
        .status()
        .expect("invoking cargo build");
    assert!(
        status.success(),
        "cargo build -p streamlib-test-fixtures must succeed"
    );

    let dylib_ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let dylib_name = format!("libstreamlib_test_fixtures.{}", dylib_ext);
    let built_dylib = workspace_root.join("target").join("debug").join(&dylib_name);
    assert!(
        built_dylib.exists(),
        "cdylib expected at {} after cargo build",
        built_dylib.display()
    );

    // Stage test-fixtures + its `@tatolab/core` dep in a tempdir so the
    // `patch: path: ../core` entry in test-fixtures' streamlib.yaml
    // resolves naturally without any yaml rewriting.
    let tmp = tempfile::tempdir().unwrap();
    let fixtures_src = workspace_root.join("packages/test-fixtures");
    let core_src = workspace_root.join("packages/core");
    let fixtures_dst = tmp.path().join("test-fixtures");
    let core_dst = tmp.path().join("core");

    std::fs::create_dir_all(&fixtures_dst).unwrap();
    std::fs::copy(
        fixtures_src.join("streamlib.yaml"),
        fixtures_dst.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_contents(&fixtures_src.join("schemas"), &fixtures_dst.join("schemas"));

    std::fs::create_dir_all(&core_dst).unwrap();
    std::fs::copy(
        core_src.join("streamlib.yaml"),
        core_dst.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_contents(&core_src.join("schemas"), &core_dst.join("schemas"));

    // Stage the cdylib under `lib/<host-triple>/` — the layout
    // `load_project` expects for `runtime: rust` processor entries.
    let triple_dir = fixtures_dst.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    std::fs::copy(&built_dylib, triple_dir.join(&dylib_name)).unwrap();

    let runtime = Runner::new().unwrap();
    runtime
        .load_project(&fixtures_dst)
        .expect("load_project must succeed against a real test-fixtures cdylib");

    let registered = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .any(|desc| desc.name.r#type.as_str() == "TestConfiguredProcessor");
    assert!(
        registered,
        "TestConfiguredProcessor must be registered after load_project — \
         STREAMLIB_PLUGIN callback should have fired"
    );
}
