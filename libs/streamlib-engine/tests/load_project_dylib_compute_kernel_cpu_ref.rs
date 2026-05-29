// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-resident compute-kernel CPU-reference integration test.
//!
//! Loads a dlopen'd `ComputeKernelTestProcessor` from
//! `streamlib-test-fixtures` and drives it through the full
//! lifecycle (setup → start → stop → teardown). The processor's
//! `start()` body:
//!   1. Creates a `VulkanComputeKernel` via
//!      `gpu_full_access().create_compute_kernel(...)` — exercises
//!      the FullAccess vtable's `create_compute_kernel` slot
//!      end-to-end.
//!   2. Acquires input + output `StorageBuffer` handles via
//!      `gpu_limited_access().acquire_storage_buffer(...)` —
//!      exercises the LimitedAccess vtable's
//!      `acquire_storage_buffer` slot.
//!   3. Populates the input through `mapped_ptr()` with synthetic
//!      data `[1, 2, ..., element_count]`.
//!   4. Binds input + output via
//!      `kernel.set_storage_buffer_storage(...)` — exercises the
//!      `VulkanComputeKernelMethodsVTable::set_storage_buffer_storage`
//!      slot for each binding.
//!   5. Stages push constants via
//!      `kernel.set_push_constants_value(&element_count)` —
//!      exercises the `set_push_constants` slot.
//!   6. Dispatches via `kernel.dispatch(group_count, 1, 1)` —
//!      exercises the `dispatch` slot.
//!   7. Reads back the output buffer and compares to the CPU
//!      reference (`input[i] * 2`).
//!   8. Writes `OK\n<element_count>` or `ERR:<message>` to the
//!      configured `output_path`.
//!
//! What this locks: a regression that breaks any of
//! `create_compute_kernel`, `acquire_storage_buffer`,
//! `set_storage_buffer_storage`, `set_push_constants`, or
//! `dispatch` at the cdylib boundary surfaces here as either:
//!   - A missing output file (cdylib's `start()` didn't fire /
//!     panicked at the FFI boundary and `run_host_extern_c`
//!     swallowed the panic).
//!   - `ERR:<message>` in the file (vtable dispatch returned an
//!     error code).
//!   - The output buffer disagreeing with the CPU reference (the
//!     dispatch ran but produced wrong output — e.g. a binding
//!     handle was routed to the wrong slot).
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
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Strategy, Runner};
use streamlib::sdk::RunnerAutoBuild;
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
fn dlopen_processor_dispatches_compute_kernel_against_cpu_reference() {
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

    let output_path = tmp.path().join("compute_kernel_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let element_count: u32 = 256;

    let runtime = Runner::with_auto_build().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path { path: fixtures_dst.clone(), build: BuildPolicy::NeverBuild },
        )
        .expect("add_module_with must succeed against the test-fixtures cdylib");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "ComputeKernelTestProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({
                "output_path": output_path_str,
                "element_count": element_count,
            }),
        ))
        .expect("add_processor must succeed for the dlopened ComputeKernelTestProcessor");

    runtime
        .start()
        .expect("runtime.start() must succeed (requires Vulkan device on this host)");

    // Manual processors fire setup then start synchronously inside
    // the runtime's processor-spawn path — by the time `start()`
    // returns, the compute-kernel round-trip has run. Poll for the
    // file with a short timeout to absorb scheduling jitter (kernel
    // build + dispatch + fence wait can take a beat on a cold
    // pipeline cache).
    let deadline = Instant::now() + Duration::from_secs(10);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "ComputeKernelTestProcessor.start() did not write {} within 10s — \
         either the cdylib's `start` lifecycle didn't fire, or one of the \
         compute-kernel vtable dispatches panicked at the FFI boundary",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "ComputeKernelTestProcessor reported an error: {contents}"
    );

    // Body is "OK\n<element_count>" on success. The element_count
    // echo proves the cdylib's `element_count` config field flowed
    // through the lifecycle dispatch path and the CPU-reference
    // comparison loop walked every element.
    let mut lines = contents.lines();
    let first = lines.next().unwrap_or("");
    assert_eq!(first, "OK", "first line must be 'OK', got {first:?}");
    let count_line = lines.next().unwrap_or("");
    let observed_count: u32 = count_line.parse().unwrap_or_else(|_| {
        panic!("second line must be a u32 element_count, got {count_line:?}")
    });
    assert_eq!(
        observed_count, element_count,
        "element_count echoed by cdylib must match config value"
    );
}
