// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Lifecycle smoke test for the dynamic Rust-plugin vtable path.
//!
//! Where `load_project_dylib_smoke.rs` asserts the registration
//! callback fired, this test goes one step further: it walks the
//! registered `TestConfiguredProcessor` through `factory.create(&node)`
//! to confirm the cdylib-side `extern "C" fn construct(...)` wrapper
//! returns a non-null instance pointer, then drops it to exercise
//! `extern "C" fn destroy(...)` — the plugin ABI heap dance that the
//! vtable shape introduces.
//!
//! Mentally revert the `Drop` impl in
//! `processor_instance_factory.rs::ProcessorInstance` and this test
//! still passes (no crash on drop today), but a future regression
//! that breaks the construct→destroy contract would surface as a
//! double-free / leak in this test plus the runtime.

use std::path::Path;

use serial_test::serial;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::{ProcessorInstance, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::{BuildPolicy, Strategy, Runner};
use streamlib::sdk::RunnerAutoBuild;
use streamlib_engine::core::graph::ProcessorNode;
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
fn dylib_processor_create_and_drop_round_trips_through_vtable() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    // Build test-fixtures with the `plugin` feature so the cdylib carries
    // the `STREAMLIB_PLUGIN` symbol.
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

    let triple_dir = fixtures_dst.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    std::fs::copy(&built_dylib, triple_dir.join(&dylib_name)).unwrap();

    let runtime = Runner::with_auto_build().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path { path: fixtures_dst.clone(), build: BuildPolicy::NeverBuild },
        )
        .expect("add_module_with must succeed against a real test-fixtures cdylib");

    // Locate the registered TestConfiguredProcessor's descriptor so we
    // can build a ProcessorNode against the exact structured ident the
    // cdylib emitted at registration time.
    let descriptor = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .find(|d| d.name.r#type.as_str() == "TestConfiguredProcessor")
        .expect("TestConfiguredProcessor must be registered after add_module");

    // Build a minimal ProcessorNode. The factory's `create()` path
    // serializes `node.config` to msgpack and hands it to the
    // cdylib's `extern "C" fn construct(...)` wrapper, which
    // deserializes into `P::Config`. Default config (empty bytes) is
    // sufficient — the wrapper falls back to `P::Config::default()`.
    let node = ProcessorNode::new(
        descriptor.name.clone(),
        "TestConfiguredProcessor",
        None,
        Vec::new(),
        Vec::new(),
    );

    // Cross the FFI boundary: cdylib's vtable.construct allocates a
    // Box<P> on the cdylib's heap and returns a thin pointer.
    let instance = PROCESSOR_REGISTRY
        .create(&node)
        .expect("factory.create() must succeed for a registered cdylib processor");

    // Verify we got the VTable variant (cdylib path, not LegacyDyn).
    assert!(
        matches!(instance, ProcessorInstance::VTable { .. }),
        "create() must return ProcessorInstance::VTable for cdylib-registered processors"
    );

    // Drop the instance — this fires the cdylib's vtable.destroy
    // wrapper, which Box::from_raw + drops on the cdylib's heap. A
    // mismatched alloc/free across DSO heaps would surface as a
    // double-free or heap corruption.
    drop(instance);

    // Final sanity: registry still reports the processor as
    // registered (drop on instance must not touch the registry).
    assert!(
        PROCESSOR_REGISTRY.is_registered(&descriptor.name),
        "registry must still hold the processor type after instance drop"
    );
}
