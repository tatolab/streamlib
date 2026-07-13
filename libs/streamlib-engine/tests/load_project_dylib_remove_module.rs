// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `remove_module` integration test for the dlopen path: load the real
//! `packages/test-fixtures` cdylib, run a construct/destroy lifecycle
//! smoke, remove the module (registrations gone, dylib image RETAINED —
//! `dlclose` is never called), then `add_module` the same package again
//! and prove the lifecycle runs again — the full load/unload/reload
//! cycle on one runtime.

use std::path::Path;

use serial_test::serial;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorInstance};
use streamlib::sdk::runtime::{BuildPolicy, Runner, Strategy};
use streamlib_engine::core::graph::ProcessorNode;
use streamlib_engine::core::runtime::{host_target_triple, loaded_plugin_library_count};

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

fn construct_destroy_lifecycle_smoke(context: &str) {
    let descriptor = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .find(|d| d.name.r#type.as_str() == "TestConfiguredProcessor")
        .unwrap_or_else(|| panic!("{context}: TestConfiguredProcessor must be registered"));
    let node = ProcessorNode::new(
        descriptor.name.clone(),
        "TestConfiguredProcessor",
        None,
        Vec::new(),
        Vec::new(),
    );
    let instance = PROCESSOR_REGISTRY
        .create(&node)
        .unwrap_or_else(|e| panic!("{context}: factory.create() must succeed: {e}"));
    assert!(matches!(instance, ProcessorInstance::VTable { .. }));
    drop(instance);
}

#[test]
#[serial]
fn remove_module_unloads_dlopen_module_and_reload_runs_lifecycle_again() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "streamlib-test-fixtures"])
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
    let built_dylib = workspace_root
        .join("target")
        .join("debug")
        .join(&dylib_name);

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

    let strategy = || Strategy::Path {
        path: fixtures_dst.clone(),
        build: BuildPolicy::NeverBuild,
    };

    // Load + lifecycle.
    let runtime = Runner::with_auto_build().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            strategy(),
        )
        .expect("initial load must succeed");
    construct_destroy_lifecycle_smoke("initial load");
    let libraries_after_first_load = loaded_plugin_library_count();
    assert!(
        libraries_after_first_load >= 1,
        "the dlopen'd image must be retained after the first load"
    );

    // Remove: registrations gone, dylib image retained.
    runtime
        .remove_module(module_ident_any_version!("tatolab", "test-fixtures"))
        .expect("remove_module must succeed with no graph consumers");
    assert!(
        !PROCESSOR_REGISTRY
            .list_registered()
            .iter()
            .any(|d| d.name.package.as_str() == "test-fixtures"),
        "remove_module must unregister every fixture processor"
    );
    assert!(
        !streamlib_engine::schemas::current_schema_idents()
            .iter()
            .any(|id| id.starts_with("@tatolab/test-fixtures/")),
        "remove_module must unregister the fixture's package-owned schemas"
    );
    assert_eq!(
        loaded_plugin_library_count(),
        libraries_after_first_load,
        "remove_module must RETAIN the dylib image (dlclose is never called)"
    );

    // Reload the same package on the same runtime; lifecycle runs again.
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            strategy(),
        )
        .expect("reload after remove_module must succeed");
    construct_destroy_lifecycle_smoke("reload after remove_module");
    assert!(
        loaded_plugin_library_count() > libraries_after_first_load,
        "the reload dlopens a fresh image entry — retention never dedups by path"
    );
}
