// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase G (#961) dlopen `ProcessorVTable::on_pause` /
//! `ProcessorVTable::on_resume` integration test.
//!
//! Loads a dlopen'd `LifecycleProbeProcessor` from test-fixtures,
//! starts the runtime, calls `runtime.pause()`, waits for the
//! processor thread to observe the pause-gate flip and dispatch
//! `on_pause` through the cdylib `ProcessorVTable`, then
//! `runtime.resume()` and waits for the `on_resume` dispatch.
//!
//! Asserts the probe's output file contains exactly one `PAUSE`
//! marker (proving `on_pause` fired through the vtable) and at
//! least one `RESUME` marker (proving `on_resume` fired). Mental-
//! revert: if either slot is removed from the `ProcessorVTable`
//! static or wired to a no-op, the corresponding marker never
//! appears and this test fails.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use serial_test::serial;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Strategy, Runner};
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

fn wait_for_line_count<F: Fn(&str) -> bool>(
    output_path: &Path,
    matches_target: F,
    target: usize,
    timeout: Duration,
) -> usize {
    let deadline = Instant::now() + timeout;
    let mut last = 0;
    while Instant::now() < deadline {
        if let Ok(contents) = std::fs::read_to_string(output_path) {
            last = contents.lines().filter(|l| matches_target(l)).count();
            if last >= target {
                return last;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}

#[test]
#[serial]
fn dlopen_processor_pause_resume_hooks_fire_through_vtable() {
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

    let output_path = tmp.path().join("pause_resume_lifecycle.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path { path: fixtures_dst.clone(), build: BuildPolicy::NeverBuild },
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
                // Allow plenty of headroom so we don't race the cap.
                "max_iterations": 1000u32,
            }),
        ))
        .expect("add_processor");

    runtime.start().expect("runtime.start");

    // Wait for at least one PROCESS line to confirm the loop is running
    // before exercising pause/resume.
    let initial = wait_for_line_count(
        &output_path,
        |l| l.starts_with("PROCESS:"),
        1,
        Duration::from_secs(5),
    );
    assert!(
        initial >= 1,
        "expected at least one PROCESS line before pause; got {initial}"
    );

    // Pause the runtime — pauses every processor, which flips the
    // pause-gate atomic. The continuous-loop thread observes the
    // flip on its next tick and dispatches `on_pause` through the
    // ProcessorVTable.
    runtime.pause().expect("runtime.pause");

    let pause_count = wait_for_line_count(
        &output_path,
        |l| l == "PAUSE",
        1,
        Duration::from_secs(5),
    );
    assert_eq!(
        pause_count, 1,
        "expected exactly one PAUSE marker after runtime.pause(); \
         on_pause must dispatch through the cdylib ProcessorVTable"
    );

    // Now resume — pause-gate flips back, continuous loop observes
    // the flip and dispatches `on_resume`.
    runtime.resume().expect("runtime.resume");

    let resume_count = wait_for_line_count(
        &output_path,
        |l| l == "RESUME",
        1,
        Duration::from_secs(5),
    );
    assert!(
        resume_count >= 1,
        "expected at least one RESUME marker after runtime.resume(); \
         on_resume must dispatch through the cdylib ProcessorVTable"
    );

    runtime.stop().ok();

    // Final read for the report contents.
    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        contents.contains("SETUP"),
        "expected SETUP marker before PAUSE; got:\n{contents}"
    );
    assert!(
        contents.contains("PAUSE") && contents.contains("RESUME"),
        "expected both PAUSE and RESUME markers; got:\n{contents}"
    );
}
