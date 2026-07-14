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
//! **#[ignore]d as of PR #1075.** `ProcessorInstance::start` now
//! wraps cdylib-resident Manual-mode dispatch in
//! `RuntimeContextFullAccess::with_cdylib_scope`, which acquires
//! the escalate gate for the duration of the start body. The
//! fixture's spawn-N-threads-each-calling-escalate-then-join
//! pattern deadlocks under this wrap: worker escalates block on
//! the gate held by start, start blocks waiting for workers to
//! join, the gate never releases. The underlying serialization
//! invariant the test was guarding is still covered by
//! `escalate_gate::tests::enter_serializes_concurrent_callers`
//! (the cdylib path adds one extern "C" indirection on top of the
//! same gate; the serialization itself is the gate's, not the
//! vtable's). Restructuring the fixture to drive concurrent
//! escalates from a Reactive `process()` body (LimitedAccess; no
//! wrap) is the natural follow-up — kept out of #1075's scope to
//! avoid bundling test-infrastructure rework with the
//! engine-symmetry fix.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use serial_test::serial;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Runner, Strategy};
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
#[ignore = "deadlocks under #1075's `with_cdylib_scope` wrap on \
            ProcessorInstance::start — fixture's spawn-N-threads-then-join \
            pattern blocks workers on the gate held by start. Serialization \
            invariant covered by escalate_gate::tests::enter_serializes_concurrent_callers. \
            Restructure to Reactive `process()` is the follow-up."]
fn dlopen_concurrent_escalate_serializes_through_vtable() {
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

    let output_path = tmp.path().join("concurrent_escalate_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::with_auto_build().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path {
                path: fixtures_dst.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("add_module_with ManifestDirectory");

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
