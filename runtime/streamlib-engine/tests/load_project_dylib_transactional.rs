// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Transactional-registration integration test for the dlopen path:
//! stage the real `packages/test-fixtures` cdylib with a DOCTORED
//! `streamlib.yaml` that declares one phantom Rust processor the dylib
//! never registers. The load fails at the declared-but-not-registered
//! validation AFTER the cdylib's real registrations ran through the
//! host callbacks (into the load's staging buffer) — so a rollback
//! regression would leave every real fixture processor + schema
//! visible. Asserts zero residue in both the processor and schema
//! registries, the process stays alive, and a reload with the
//! corrected manifest succeeds end-to-end including a
//! construct/destroy lifecycle smoke through the cdylib vtable.

use std::path::Path;

use serial_test::serial;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorInstance};
use streamlib::sdk::runtime::{BuildPolicy, Runner, Strategy};
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

/// Every fixture-owned entry that must NOT survive a failed load.
fn assert_zero_test_fixtures_residue(context: &str) {
    let leaked_processors: Vec<String> = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .filter(|desc| desc.name.package.as_str() == "test-fixtures")
        .map(|desc| desc.name.to_string())
        .collect();
    assert!(
        leaked_processors.is_empty(),
        "{context}: failed load leaked processor registrations: {leaked_processors:?}",
    );
    let leaked_schemas: Vec<String> = streamlib_engine::schemas::current_schema_idents()
        .into_iter()
        .filter(|id| id.starts_with("@tatolab/test-fixtures/"))
        .collect();
    assert!(
        leaked_schemas.is_empty(),
        "{context}: failed load leaked schema registrations: {leaked_schemas:?}",
    );
}

#[test]
#[serial]
fn failing_dlopen_load_leaves_zero_residue_and_reload_succeeds() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    // Build test-fixtures so the cdylib carries `STREAMLIB_PLUGIN`.
    // Idempotent: cargo's incremental machinery skips on a warm tree.
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

    // Stage test-fixtures + its `@tatolab/core` dep in a tempdir so the
    // `patch: path: ../core` entry resolves naturally.
    let tmp = tempfile::tempdir().unwrap();
    let fixtures_src = workspace_root.join("packages/test-fixtures");
    let core_src = workspace_root.join("packages/core");
    let fixtures_dst = tmp.path().join("test-fixtures");
    let core_dst = tmp.path().join("core");

    std::fs::create_dir_all(&fixtures_dst).unwrap();
    let pristine_manifest = std::fs::read_to_string(fixtures_src.join("streamlib.yaml")).unwrap();
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

    // DOCTOR the manifest: append a phantom Rust processor entry the
    // dylib does not register. Appended LAST, so every real processor's
    // declared-but-not-registered validation passes against the staged
    // registrations first — the failure fires with the full real
    // registration set already staged.
    let doctored_manifest = format!(
        "{pristine_manifest}\n  - name: PhantomUnregisteredProcessor\n    \
         version: 1.0.0\n    description: \"phantom Rust processor the dylib \
         does not register — transactional-rollback integration fixture\"\n    \
         execution: manual\n"
    );
    std::fs::write(fixtures_dst.join("streamlib.yaml"), &doctored_manifest).unwrap();

    let runtime = Runner::with_auto_build().unwrap();
    let err = runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path {
                path: fixtures_dst.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect_err("the phantom processor must fail the load");
    let message = err.to_string();
    assert!(
        message.contains("PhantomUnregisteredProcessor")
            && message.contains("not") // "not registered by the dylib"
            && message.contains("registered"),
        "the failure must name the phantom processor, got: {message}",
    );

    // Zero residue: the cdylib's REAL registrations ran through the host
    // callbacks into the staging buffer, and the failed load dropped it.
    assert_zero_test_fixtures_residue("after doctored load");

    // Reload with the corrected manifest on the SAME runtime — proves
    // the failed attempt poisoned nothing (memo, registries, ledger).
    std::fs::write(fixtures_dst.join("streamlib.yaml"), &pristine_manifest).unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path {
                path: fixtures_dst.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("reload with the corrected manifest must succeed");

    // Lifecycle smoke: construct + destroy round-trips the cdylib vtable.
    let descriptor = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .find(|d| d.name.r#type.as_str() == "TestConfiguredProcessor")
        .expect("TestConfiguredProcessor must be registered after the reload");
    let node = ProcessorNode::new(
        descriptor.name.clone(),
        "TestConfiguredProcessor",
        None,
        Vec::new(),
        Vec::new(),
    );
    let instance = PROCESSOR_REGISTRY
        .create(&node)
        .expect("factory.create() must succeed after the reload");
    assert!(matches!(instance, ProcessorInstance::VTable { .. }));
    drop(instance);
}
