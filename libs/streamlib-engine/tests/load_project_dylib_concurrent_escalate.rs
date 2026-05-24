// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase H (#1006 scenario 1) dlopen-cdylib concurrent-escalate
//! integration test.
//!
//! Loads `ConcurrentEscalateTestProcessor` from streamlib-test-fixtures
//! and drives it through `runtime.start()` → `runtime.stop()`. The
//! fixture spawns 8 threads from inside its `start()` callback; each
//! thread clones `gpu_limited_access()` and calls escalate
//! concurrently. The escalate gate is documented to serialize
//! concurrent callers — overlapping closures across the cdylib
//! `escalate_via_vtable` path would be a regression.
//!
//! Assertions:
//!   - Output file format: `OK\n<thread_count>\noverlaps=0`.
//!   - `overlaps=0` is the load-bearing lock; any other count means
//!     the gate let two callers in simultaneously.
//!
//! Mental revert: collapsing the engine's `EscalateGate::enter_scoped`
//! to a no-op would let multiple cdylib threads interleave; this test
//! catches that as `overlaps>0`.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use serial_test::serial;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
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
fn dlopen_concurrent_escalate_serializes_through_vtable() {
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
            "--features",
            "streamlib-test-fixtures/plugin",
        ])
        .status()
        .expect("invoking cargo build");
    assert!(
        status.success(),
        "cargo build -p streamlib-test-fixtures --features plugin must succeed"
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

    let output_path = tmp.path().join("concurrent_escalate_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime.load_project(&fixtures_dst).expect("load_project");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "ConcurrentEscalateTestProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({
                "output_path": output_path_str,
                "thread_count": 8u32,
                "hold_ms": 10u32,
            }),
        ))
        .expect("add_processor");

    runtime.start().expect("runtime.start must succeed");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "ConcurrentEscalateTestProcessor.start() did not write {} within 10s",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "ConcurrentEscalateTest reported an error: {contents}"
    );
    let mut lines = contents.lines();
    assert_eq!(lines.next().unwrap_or(""), "OK", "first line must be 'OK'");
    assert_eq!(lines.next().unwrap_or(""), "8", "thread_count line");
    let overlap_line = lines.next().unwrap_or("");
    assert_eq!(
        overlap_line, "overlaps=0",
        "escalate-gate must serialize cdylib-path concurrent callers; got {overlap_line:?}"
    );
}
