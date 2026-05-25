// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-resident GPU vtable round-trip integration test.
//!
//! Loads a dlopen'd `GpuAcquireTestProcessor` from test-fixtures and
//! drives it through a full lifecycle (setup → start → stop →
//! teardown). The processor's `start()` body:
//!   1. Clones `ctx.gpu_limited_access()` — exercises `clone_handle`.
//!   2. Acquires a `PixelBuffer` — exercises `acquire_pixel_buffer`
//!      (paired-out-param tuple).
//!   3. Reads `pixel_buffer.width` / `.height` — cached POD reads,
//!      no cross-DSO dispatch.
//!   4. Reads `plane_base_address(0)` — exercises
//!      `plane_base_address_pixel_buffer`.
//!   5. Writes a sentinel byte through the returned pointer — proves
//!      cdylib→host mapped-memory access is sound.
//!   6. Drops the `PixelBuffer` — exercises `drop_pixel_buffer`.
//!   7. Writes "OK\n<w>x<h>\nsentinel_addr=0x<hex>" to the configured
//!      `output_path`.
//!   8. `teardown()` drops the stashed `GpuContextLimitedAccess` —
//!      exercises `drop_handle`.
//!
//! What this locks: a regression that breaks any of the Arc-lifecycle,
//! plane-base-address, or pixel-buffer-acquire callbacks at the
//! cdylib boundary surfaces here as either:
//!   - A missing output file (cdylib's `start()` didn't fire / panicked
//!     at the FFI boundary and `run_host_extern_c` swallowed the
//!     panic).
//!   - `ERR:<message>` in the file (vtable dispatch returned an
//!     error code).
//!   - A garbage `sentinel_addr=0x0` line when the host's mapping
//!     was HOST_VISIBLE (plane_base_address callback returned null
//!     incorrectly).
//!
//! Runs locally with a working Vulkan device (Runner::start()
//! initializes `GpuContext::init_for_platform_sync()`). On
//! GPU-less hardware the runtime start will fail with a clean
//! Vulkan-device-init error and this test will report it through
//! the standard assertion path; CI has no GPU runner planned
//! (see `project_ci_strategy_no_gpu`) so this test runs locally.

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
fn dlopen_processor_round_trips_gpu_vtable_callbacks() {
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

    let output_path = tmp.path().join("gpu_acquire_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .load_project(&fixtures_dst)
        .expect("load_project must succeed against a real test-fixtures cdylib");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "GpuAcquireTestProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({
                "output_path": output_path_str,
                "width": 64u32,
                "height": 64u32,
            }),
        ))
        .expect("add_processor must succeed for the dlopened GpuAcquireTestProcessor");

    runtime
        .start()
        .expect("runtime.start() must succeed (requires Vulkan device on this host)");

    // Manual processors fire setup then start synchronously inside
    // the runtime's processor-spawn path — by the time `start()`
    // returns, the GPU vtable round-trip has run. Poll for the file
    // with a short timeout to absorb scheduling jitter.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "GpuAcquireTestProcessor.start() did not write {} within 5s — \
         either the cdylib's `start` lifecycle didn't fire, or the \
         GPU vtable dispatch path panicked at the FFI boundary",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "GpuAcquireTestProcessor reported an error: {contents}"
    );

    // Parse the three-line "OK\n<w>x<h>\nsentinel_addr=0x<hex>" body
    // and verify the cached width/height match the configured values
    // (proves the POD-read path from the cdylib's `PixelBuffer`
    // β-shape is intact).
    let mut lines = contents.lines();
    let first = lines.next().unwrap_or("");
    assert_eq!(
        first, "OK",
        "first line must be 'OK', got {first:?}"
    );
    let dims = lines.next().unwrap_or("");
    assert_eq!(
        dims, "64x64",
        "cached width/height must round-trip from the cdylib's PixelBuffer β-shape, got {dims:?}"
    );
    let addr_line = lines.next().unwrap_or("");
    assert!(
        addr_line.starts_with("sentinel_addr=0x"),
        "third line must be 'sentinel_addr=0x<hex>', got {addr_line:?}"
    );
    // On Linux+Vulkan, HOST_VISIBLE pool-backed buffers always have a
    // non-null mapped pointer. (If the host ever migrates the pool
    // backing to DEVICE_LOCAL-only, the cdylib would observe a null
    // here and the sentinel write would be skipped — that would be
    // a deliberate engine change and warrants updating this lock.)
    assert!(
        !addr_line.ends_with("0x0"),
        "plane_base_address(0) returned null for a HOST_VISIBLE pool buffer — \
         either the v3 `plane_base_address_pixel_buffer` vtable callback is \
         broken, or the pool's backing changed to DEVICE_LOCAL-only"
    );
}
