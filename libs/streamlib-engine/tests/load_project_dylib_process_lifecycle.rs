// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase G (#961) dlopen `ProcessorVTable::process` integration test.
//!
//! Loads a dlopen'd `LifecycleProbeProcessor` from test-fixtures,
//! starts the runtime, sleeps briefly to let the continuous-loop
//! thread fire `process()` repeatedly, then stops and inspects the
//! probe's output file. Asserts:
//!
//!   - At least one `SETUP` marker (proves
//!     `ProcessorVTable::setup` dispatched through the cdylib).
//!   - At least one `PROCESS:<n>` marker (proves
//!     `ProcessorVTable::process` dispatched through the cdylib
//!     hot path — the slot none of the existing smoke tests
//!     exercised).
//!   - At least one `TEARDOWN` marker (proves
//!     `ProcessorVTable::teardown` dispatched on the runtime stop
//!     path).
//!
//! Mental-revert: if `process` is removed from the
//! `ProcessorVTable` static or wired to a no-op, the probe never
//! writes a `PROCESS:<n>` line and this test fails.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use serial_test::serial;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{ModuleResolverStrategy, Runner};
use streamlib::sdk::schema_ident;
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
fn dlopen_processor_process_hook_fires_through_vtable() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

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

    let output_path = tmp.path().join("process_lifecycle.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with(
            module_ident_any_version!("tatolab", "test-fixtures"),
            ModuleResolverStrategy::ManifestDirectory {
                path: fixtures_dst.clone(),
            },
        )
        .expect("add_module_with ManifestDirectory");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "LifecycleProbeProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({
                "output_path": output_path_str,
                "max_iterations": 5u32,
            }),
        ))
        .expect("add_processor");

    runtime
        .start()
        .expect("runtime.start must succeed (requires Vulkan device on this host)");

    // Wait for at least 3 PROCESS lines or 5s. The probe's process()
    // hook fires on every iteration of the continuous loop (default
    // sleep ~100us); 3 iterations is far below the 5s budget unless
    // the vtable slot regressed.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(contents) = std::fs::read_to_string(&output_path) {
            let process_count = contents
                .lines()
                .filter(|l| l.starts_with("PROCESS:"))
                .count();
            if process_count >= 3 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    let contents = std::fs::read_to_string(&output_path)
        .expect("LifecycleProbe output file must exist after runtime.start()");

    let setup_count = contents.lines().filter(|l| *l == "SETUP").count();
    let process_count = contents
        .lines()
        .filter(|l| l.starts_with("PROCESS:"))
        .count();
    let teardown_count = contents.lines().filter(|l| *l == "TEARDOWN").count();

    assert_eq!(
        setup_count, 1,
        "expected exactly one SETUP marker; got contents:\n{contents}"
    );
    assert!(
        process_count >= 3,
        "expected >= 3 PROCESS:<n> markers (vtable process dispatch); \
         got {process_count}. Contents:\n{contents}"
    );
    assert_eq!(
        teardown_count, 1,
        "expected exactly one TEARDOWN marker (vtable teardown dispatch); \
         got contents:\n{contents}"
    );
}
