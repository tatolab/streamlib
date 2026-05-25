// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib smoke test for the cpu-readback surface adapter.
//!
//! Loads the `CpuReadbackSmokeTestProcessor` from test-fixtures and
//! drives it through `start()`. The processor's body runs a full
//! cdylib-side adapter-construction round-trip inside
//! `gpu.escalate(|full| ...)`:
//!
//!   1. `host_vulkan_device_arc()` (v9 bridge) — obtain
//!      `Arc<HostVulkanDevice>`.
//!   2. `HostVulkanBuffer::new(&device_arc, size)` — allocate
//!      HOST_VISIBLE staging buffer through the cdylib-reachable
//!      constructor (verifies the route-2 path documented on
//!      `HostVulkanBuffer`).
//!   3. `HostVulkanTimelineSemaphore::new_exportable(device_arc.device(), 0)`
//!      — allocate the timeline through the cdylib-reachable
//!      constructor.
//!   4. `CpuReadbackSurfaceAdapter::new(device_arc, trigger)` +
//!      `register_host_surface(...)`.
//!   5. `adapter.acquire_write(&surface)` → `view_mut → plane_mut →
//!      bytes_mut`, write one sentinel byte, drop the guard.
//!
//! A regression in any step surfaces as either:
//!   - Missing output file (cdylib's `start()` panicked at the FFI
//!     boundary and `run_host_extern_c` swallowed the panic).
//!   - `ERR:<message>` (any step returned a `Result::Err`).
//!
//! Requires a working Vulkan device. CI has no GPU runner planned
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
fn dlopen_processor_round_trips_cpu_readback_adapter() {
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

    let output_path = tmp.path().join("cpu_readback_smoke_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .load_project(&fixtures_dst)
        .expect("load_project must succeed against the test-fixtures cdylib");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "CpuReadbackSmokeTestProcessor",
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
            "add_processor must succeed for the dlopened CpuReadbackSmokeTestProcessor",
        );

    runtime
        .start()
        .expect("runtime.start() must succeed (requires Vulkan device on this host)");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "CpuReadbackSmokeTestProcessor.start() did not write {} within 10s — \
         either the cdylib's start() lifecycle didn't fire, or one of the \
         cdylib-reach paths (host_vulkan_device_arc, HostVulkanBuffer::new, \
         HostVulkanTimelineSemaphore::new_exportable, \
         CpuReadbackSurfaceAdapter::register_host_surface, acquire_write) \
         panicked at the FFI boundary",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "CpuReadbackSmokeTestProcessor reported an error: {contents}. \
         Any error means at least one cdylib-side adapter-construction \
         step regressed."
    );

    // Format: "OK\n<width>x<height>\nbytes_written=<n>"
    let lines: Vec<&str> = contents.lines().collect();
    assert!(
        lines.len() >= 3,
        "expected 3 lines (OK / dims / bytes_written), got {contents:?}"
    );
    assert_eq!(lines[0], "OK", "first line must be 'OK', got {:?}", lines[0]);
    assert_eq!(
        lines[1], "64x64",
        "second line must be the surface dimensions, got {:?}",
        lines[1]
    );
    assert_eq!(
        lines[2], "bytes_written=1",
        "third line must report the sentinel write, got {:?}",
        lines[2]
    );
}
