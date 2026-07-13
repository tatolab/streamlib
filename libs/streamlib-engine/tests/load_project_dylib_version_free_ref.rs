// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Version-free processor-reference integration test: reference a processor by
//! `@org/package/Type` with **no version anywhere in the app code** and have
//! the installed provider from `streamlib_modules/` lazily load, resolve, and
//! run through its vtable lifecycle.
//!
//! The only reference form in this "app" is
//! `processor_type_ref!("org", "package", "Type")` — grep this file for a
//! version string and you will find none in the pipeline code.
//!
//! This lives in its own test binary (separate process) on purpose: the
//! processor registry and the retained dlopen'd plugin images are
//! process-global, so a sibling test that already loaded `@tatolab/test-fixtures`
//! would defeat the "exactly one cold load" assertion. A fresh process gives a
//! deterministic cold load.

use std::path::Path;

use serial_test::serial;
use streamlib::sdk::error::Error;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorInstance, ProcessorSpec};
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::{RunnerAutoBuild, processor_type_ref};
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

/// Clears the process-wide app-modules override on drop, so a panicking test
/// can't leak a stale (deleted-tempdir) root into a later one.
struct AppModulesDirGuard;
impl Drop for AppModulesDirGuard {
    fn drop(&mut self) {
        Runner::clear_app_modules_dir();
    }
}

/// Stage `packages/test-fixtures` (prebuilt cdylib) + its `@tatolab/core` dep
/// into `<app_root>/streamlib_modules/@tatolab/{test-fixtures,core}`. The
/// `patch: path: ../core` in test-fixtures' manifest resolves `@tatolab/core`
/// to the sibling slot, which is exactly where core lands.
fn stage_app_modules(app_root: &Path) {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    // Build test-fixtures so the cdylib carries `STREAMLIB_PLUGIN`. Idempotent
    // — cargo's incremental machinery skips on a warm tree.
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
    assert!(
        built_dylib.exists(),
        "cdylib expected at {} after cargo build",
        built_dylib.display()
    );

    let modules_dir = app_root.join("streamlib_modules");
    let fixtures_src = workspace_root.join("packages/test-fixtures");
    let core_src = workspace_root.join("packages/core");
    let fixtures_dst = modules_dir.join("@tatolab").join("test-fixtures");
    let core_dst = modules_dir.join("@tatolab").join("core");

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

    // Stage the prebuilt cdylib under `lib/<host-triple>/` — the layout
    // `Strategy::InstalledCache` prefers (compiler-free load, no orchestrator
    // build needed).
    let triple_dir = fixtures_dst.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    std::fs::copy(&built_dylib, triple_dir.join(&dylib_name)).unwrap();
}

#[test]
#[serial]
fn add_processor_lazily_loads_plugin_via_version_free_reference() {
    // The headline goal: reference a processor with NO version at the reference
    // site and have the installed provider lazily load + resolve + run. The
    // pipeline code below carries no version string — only
    // `processor_type_ref!("org", "package", "Type")`.
    let app = tempfile::tempdir().unwrap();
    stage_app_modules(app.path());

    let _guard = AppModulesDirGuard;
    Runner::set_app_modules_dir(app.path());
    let runtime = Runner::with_auto_build().unwrap();

    // (1) A version-free reference lazily discovers + loads the provider. This
    // process is fresh, so the load is genuinely cold: exactly one dlopen.
    let libraries_before = loaded_plugin_library_count();
    runtime
        .add_processor(ProcessorSpec::new(
            processor_type_ref!("tatolab", "test-fixtures", "TestConfiguredProcessor"),
            serde_json::json!({}),
        ))
        .expect("a version-free reference must lazily load @tatolab/test-fixtures and resolve");

    let registered = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .any(|desc| desc.name.r#type.as_str() == "TestConfiguredProcessor");
    assert!(
        registered,
        "TestConfiguredProcessor must be registered after the version-free lazy load"
    );
    let libraries_after_first = loaded_plugin_library_count();
    assert_eq!(
        libraries_after_first,
        libraries_before + 1,
        "the version-free reference must dlopen the provider exactly once"
    );

    // (2) A second version-free reference to the now-loaded type must NOT
    // reload the plugin — the installed-tuple fast path short-circuits.
    runtime
        .add_processor(ProcessorSpec::new(
            processor_type_ref!("tatolab", "test-fixtures", "TestConfiguredProcessor"),
            serde_json::json!({}),
        ))
        .expect("a second version-free reference to an already-loaded type must succeed");
    assert_eq!(
        loaded_plugin_library_count(),
        libraries_after_first,
        "a repeat version-free reference must NOT reload the plugin"
    );

    // (3) A version-free reference to an absent type returns the recoverable
    // typed error, and the runtime keeps operating.
    let absent = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "definitely-not-installed", "Ghost"),
        serde_json::json!({}),
    ));
    assert!(
        matches!(absent, Err(Error::UnknownProcessorType { .. })),
        "an absent version-free type must return a typed recoverable error, got {absent:?}"
    );
    // Runtime keeps operating: a subsequent valid version-free add succeeds.
    runtime
        .add_processor(ProcessorSpec::new(
            processor_type_ref!("tatolab", "test-fixtures", "TestConfiguredProcessor"),
            serde_json::json!({}),
        ))
        .expect("the runtime must keep operating after a failed version-free add_processor");

    // (4) Lifecycle smoke: the lazily-loaded cdylib constructs through its
    // vtable and destroys cleanly — resolved via the version-free path.
    let descriptor = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .find(|d| d.name.r#type.as_str() == "TestConfiguredProcessor")
        .expect("TestConfiguredProcessor descriptor must be present");
    let node = ProcessorNode::new(
        descriptor.name.clone(),
        "TestConfiguredProcessor",
        None,
        Vec::new(),
        Vec::new(),
    );
    let instance = PROCESSOR_REGISTRY
        .create(&node)
        .expect("factory.create() must construct the version-free lazily-loaded processor");
    assert!(matches!(instance, ProcessorInstance::VTable { .. }));
    drop(instance);
    // `_guard` clears the process-wide override on drop.
}
