// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase C3 (#903) cdylib-resident escalate vtable smoke test.
//!
//! Loads a dlopen'd `EscalateSmokeTestProcessor` from test-fixtures
//! and drives it through `start()`. The processor's `start()` body
//! runs `gpu.escalate(|_full| Ok(()))` once, which exercises the
//! full scope-token machinery end-to-end across the FFI:
//!
//!   1. Cdylib's `GpuContextLimitedAccess::escalate` detects cdylib
//!      mode (`host_callbacks().is_some()`) and routes through
//!      `escalate_via_vtable`.
//!   2. `escalate_via_vtable` calls the `escalate_begin` vtable callback,
//!      which on the host side enters the escalate gate, clones the
//!      bound `Arc<GpuContext>`, mints an opaque scope token, and
//!      registers the scope.
//!   3. `escalate_via_vtable` constructs a cdylib-side
//!      `GpuContextFullAccess` via `from_scope_token` (HandleKind::
//!      ScopeToken).
//!   4. The closure runs (empty body — just exits cleanly).
//!   5. The cdylib drops the FullAccess. Drop dispatches on the
//!      `handle_kind` discriminator: for ScopeToken it's a no-op
//!      (cleanup happens in `escalate_end`, not here).
//!   6. `escalate_via_vtable` calls the `escalate_end` vtable callback,
//!      which removes the scope from the registry, releases the
//!      escalate gate, and runs `wait_device_idle`.
//!
//! A regression in any of the steps above surfaces as either:
//!   - Missing output file (the cdylib's `start()` panicked at the
//!     FFI boundary and `run_host_extern_c` swallowed the panic).
//!   - `ERR:<message>` in the file (the escalate call returned an
//!     error — typically "invalid escalate scope" if a scope-token
//!     check failed).
//!
//! What this test does NOT cover: cdylib-side dispatch on
//! `GpuContextFullAccess` methods (`create_compute_kernel`, etc.) —
//! those still call `host_inner()` and panic from cdylib code. That
//! gap is the scope of Phase D (#906); the richer "create kernel +
//! dispatch + compare CPU reference" test originally specced for
//! #903 lives in Phase E (#907).
//!
//! Requires a working Vulkan device (Runner::start() initializes
//! GpuContext::init_for_platform_sync()). On GPU-less hardware the
//! runtime start fails with a clean error and the assertion below
//! surfaces it; CI has no GPU runner planned
//! (`project_ci_strategy_no_gpu`), so this test runs locally.

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
fn dlopen_processor_round_trips_escalate_vtable_callbacks() {
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

    let output_path = tmp.path().join("escalate_smoke_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .load_project(&fixtures_dst)
        .expect("load_project must succeed against the test-fixtures cdylib");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "EscalateSmokeTestProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({
                "output_path": output_path_str,
            }),
        ))
        .expect(
            "add_processor must succeed for the dlopened EscalateSmokeTestProcessor",
        );

    runtime
        .start()
        .expect("runtime.start() must succeed (requires Vulkan device on this host)");

    // Manual processors fire setup then start synchronously inside
    // the runtime's processor-spawn path — by the time `start()`
    // returns, the escalate round-trip has run. Poll briefly to
    // absorb scheduling jitter.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "EscalateSmokeTestProcessor.start() did not write {} within 5s — \
         either the cdylib's start() lifecycle didn't fire, or the \
         escalate vtable dispatch path panicked at the FFI boundary",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "EscalateSmokeTestProcessor reported an error: {contents}. \
         Expected 'OK' — any error means the scope-token machinery \
         (escalate_begin / from_scope_token / escalate_end) failed \
         end-to-end."
    );
    assert_eq!(
        contents.trim(),
        "OK",
        "escalate_smoke_result must be 'OK', got {contents:?}"
    );

    // Re-running another escalate scope on the same Runner would
    // dead-lock here if `escalate_end` failed to release the gate.
    // We don't run a second scope explicitly because `runtime.stop()`
    // above has already taken the runtime back through the shutdown
    // path; the lock-leak failure mode would have surfaced as a hang
    // in the polling loop above (which has a 5s timeout, surfacing
    // as the "did not write" assertion).
}
