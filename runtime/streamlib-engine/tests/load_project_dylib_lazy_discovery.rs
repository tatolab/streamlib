// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Lazy plugin auto-discovery integration test: stage the real
//! `packages/test-fixtures` cdylib inside a temp app's `streamlib_modules/`
//! folder, tell the runtime where that folder is, then drive the whole thing
//! through `add_processor` ALONE — no `add_module` call in the "app" code.
//!
//! Locks the shipped contract of #1325:
//! 1. `add_processor(<type>)` for a type whose package sits in
//!    `streamlib_modules/` lazily loads the providing plugin on first
//!    reference and the processor lands healthy.
//! 2. A second reference to a type from the already-loaded package does NOT
//!    reload the plugin (the retained-image count stays flat).
//! 3. `add_processor` for an absent package returns a typed error AND the
//!    runtime keeps operating — a subsequent valid `add_processor` succeeds.
//! 4. The lazily-loaded cdylib constructs through its vtable
//!    (construct/destroy lifecycle smoke).

use std::path::Path;

use serial_test::serial;
use streamlib::sdk::error::Error;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorInstance, ProcessorSpec};
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::{RunnerAutoBuild, schema_ident};
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

/// Stage a manifest-only package at
/// `<app_root>/streamlib_modules/<dir_org>/<dir_name>/streamlib.yaml` declaring
/// `<pkg_org>/<pkg_name>` and one Rust processor `<proc>`. When `with_cargo` is
/// set, drop a `Cargo.toml` beside it so the loader classifies it as buildable
/// Rust source (no prebuilt) — an unbuildable package under a no-orchestrator
/// runtime.
fn write_manifest_package(
    app_root: &Path,
    dir_org: &str,
    dir_name: &str,
    pkg_org: &str,
    pkg_name: &str,
    proc: &str,
    with_cargo: bool,
) {
    let dir = app_root
        .join("streamlib_modules")
        .join(dir_org)
        .join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("streamlib.yaml"),
        format!(
            "package:\n  org: {pkg_org}\n  name: {pkg_name}\n  version: 1.0.0\nprocessors:\n  \
             - name: {proc}\n    version: 1.0.0\n    description: d\n    runtime: rust\n    \
             execution: manual\n    inputs: []\n    outputs: []\n"
        ),
    )
    .unwrap();
    if with_cargo {
        std::fs::write(dir.join("Cargo.toml"), b"[package]\nname='x'\nversion='0.0.0'\n").unwrap();
    }
}

/// Stage `packages/test-fixtures` (prebuilt cdylib) + its `@tatolab/core` dep
/// into `<app_root>/streamlib_modules/@tatolab/{test-fixtures,core}` and return
/// the app root. The `patch: path: ../core` in test-fixtures' manifest resolves
/// `@tatolab/core` to the sibling slot, which is exactly where core lands.
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
fn add_processor_lazily_loads_plugin_from_streamlib_modules_with_no_load_call() {
    let app = tempfile::tempdir().unwrap();
    stage_app_modules(app.path());

    // The runtime is TOLD which directory holds streamlib_modules/ — the
    // daemon/host modules-dir form (process-wide, GST_PLUGIN_PATH-style). No
    // add_module / add_module_with anywhere in this "app" code.
    let _guard = AppModulesDirGuard;
    Runner::set_app_modules_dir(app.path());
    let runtime = Runner::with_auto_build().unwrap();

    let fixtures_ident = || schema_ident!("tatolab", "test-fixtures", "TestConfiguredProcessor", "1.0.0");

    // (1) First reference lazily discovers + loads the providing package.
    let libraries_before = loaded_plugin_library_count();
    let processor_id = runtime
        .add_processor(ProcessorSpec::new(fixtures_ident(), serde_json::json!({})))
        .expect("add_processor must lazily load @tatolab/test-fixtures and succeed");
    let _ = processor_id;

    let registered = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .any(|desc| desc.name.r#type.as_str() == "TestConfiguredProcessor");
    assert!(
        registered,
        "TestConfiguredProcessor must be registered after the lazy load"
    );
    let libraries_after_first = loaded_plugin_library_count();
    assert_eq!(
        libraries_after_first,
        libraries_before + 1,
        "the lazy load must dlopen the plugin exactly once"
    );

    // (2) A second reference to a type from the ALREADY-loaded package must NOT
    // reload the plugin — the registry fast path short-circuits discovery.
    runtime
        .add_processor(ProcessorSpec::new(fixtures_ident(), serde_json::json!({})))
        .expect("a second reference to an already-loaded type must succeed");
    assert_eq!(
        loaded_plugin_library_count(),
        libraries_after_first,
        "a repeat reference must NOT reload the plugin (single lazy load)"
    );

    // (3) Referencing an ABSENT package returns a typed error, and the runtime
    // keeps operating.
    let absent = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "definitely-not-installed", "Ghost", "1.0.0"),
        serde_json::json!({}),
    ));
    assert!(
        matches!(absent, Err(Error::UnknownProcessorType { .. })),
        "an absent package must return a typed recoverable error, got {absent:?}"
    );
    // The graph keeps operating: a subsequent valid add still succeeds.
    runtime
        .add_processor(ProcessorSpec::new(fixtures_ident(), serde_json::json!({})))
        .expect("the runtime must keep operating after a failed add_processor");

    // (4) Lifecycle smoke: the lazily-loaded cdylib constructs through its
    // vtable and destroys cleanly.
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
        .expect("factory.create() must construct the lazily-loaded processor");
    assert!(matches!(instance, ProcessorInstance::VTable { .. }));
    drop(instance);
    // `_guard` clears the process-wide override on drop.
}

#[test]
#[serial]
fn add_processor_returns_ambiguous_error_for_duplicate_providers() {
    // Two folders whose manifests both declare @tatolab/dup/Thing — a
    // malformed install. add_processor must surface the typed ambiguity error
    // end-to-end (through the lazy hook into add_processor_impl), and the
    // runtime must keep operating afterward.
    let app = tempfile::tempdir().unwrap();
    write_manifest_package(app.path(), "@tatolab", "dup", "tatolab", "dup", "Thing", false);
    write_manifest_package(
        app.path(),
        "@tatolab",
        "dup-alias",
        "tatolab",
        "dup",
        "Thing",
        false,
    );

    let _guard = AppModulesDirGuard;
    Runner::set_app_modules_dir(app.path());
    let runtime = Runner::new().unwrap();

    let result = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "dup", "Thing", "1.0.0"),
        serde_json::json!({}),
    ));
    assert!(
        matches!(result, Err(Error::AmbiguousProcessorTypeProviders { .. })),
        "duplicate providers must surface a typed ambiguity error, got {result:?}"
    );

    // Runtime keeps operating: a subsequent reference to an absent package
    // returns a typed error rather than wedging.
    let after = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "not-there", "Nope", "1.0.0"),
        serde_json::json!({}),
    ));
    assert!(matches!(after, Err(Error::UnknownProcessorType { .. })));
}

#[test]
#[serial]
fn add_processor_returns_lazy_load_failed_for_unbuildable_package() {
    // A package that discovers cleanly but fails to load: a Rust package with
    // source (Cargo.toml) but no prebuilt cdylib, loaded under a runtime with
    // NO build orchestrator → the lazy load fails and add_processor returns the
    // recoverable LazyModuleLoadFailed while the runtime keeps operating.
    let app = tempfile::tempdir().unwrap();
    write_manifest_package(
        app.path(),
        "@tatolab",
        "broken",
        "tatolab",
        "broken",
        "BrokenProcessor",
        true,
    );

    let _guard = AppModulesDirGuard;
    Runner::set_app_modules_dir(app.path());
    let runtime = Runner::new().unwrap(); // no orchestrator wired

    let result = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "broken", "BrokenProcessor", "1.0.0"),
        serde_json::json!({}),
    ));
    assert!(
        matches!(result, Err(Error::LazyModuleLoadFailed { .. })),
        "a discovered-but-unbuildable package must surface LazyModuleLoadFailed, got {result:?}"
    );

    // The failed lazy load left zero partial state and the runtime keeps
    // operating.
    let after = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "still-absent", "Nope", "1.0.0"),
        serde_json::json!({}),
    ));
    assert!(matches!(after, Err(Error::UnknownProcessorType { .. })));
}
