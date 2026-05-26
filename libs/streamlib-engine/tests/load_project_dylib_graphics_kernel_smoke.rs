// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-resident graphics-kernel methods-vtable smoke test.
//!
//! Loads a dlopen'd `GraphicsKernelSmokeTestProcessor` from
//! `streamlib-test-fixtures` and drives it through the full
//! lifecycle (setup → start → stop → teardown). The processor's
//! `start()` body:
//!   1. Creates a `VulkanGraphicsKernel` via
//!      `gpu_full_access().create_graphics_kernel(...)` — exercises
//!      the FullAccess vtable's `create_graphics_kernel` slot
//!      end-to-end.
//!   2. Acquires a render-target `PooledTextureHandle` via
//!      `gpu_limited_access().acquire_texture(...)` — exercises
//!      the LimitedAccess vtable's `acquire_texture` slot.
//!   3. Stages push constants via
//!      `kernel.set_push_constants_value(0, &variant)` — exercises
//!      the `VulkanGraphicsKernelMethodsVTable::set_push_constants`
//!      slot.
//!   4. Drives `kernel.offscreen_render(...)` against the acquired
//!      texture — exercises the `offscreen_render` vtable slot
//!      end-to-end (including the parallel-array color-target
//!      marshaling and the `OffscreenDrawRepr` tagged-union
//!      encoding).
//!   5. Writes `OK` or `ERR:<message>` to the configured
//!      `output_path`.
//!
//! What this locks: a regression that breaks any of
//! `create_graphics_kernel`, `acquire_texture`,
//! `set_push_constants`, or `offscreen_render` at the cdylib
//! boundary surfaces here as either:
//!   - A missing output file (cdylib's `start()` didn't fire /
//!     panicked at the FFI boundary and `run_host_extern_c`
//!     swallowed the panic).
//!   - `ERR:<message>` in the file (vtable dispatch returned an
//!     error code).
//!
//! Smoke-only — pixel correctness is not asserted. This is
//! deliberately a narrower harness than the compute-kernel
//! CPU-reference test (#963) because the graphics-pipeline path
//! doesn't have a trivially-CPU-reproducible result; the vtable
//! round-trip is what the test locks.
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
fn dlopen_processor_runs_graphics_kernel_offscreen_render_smoke() {
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

    let output_path = tmp.path().join("graphics_kernel_smoke_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with(
            module_ident_any_version!("tatolab", "test-fixtures"),
            ModuleResolverStrategy::ManifestDirectory {
                path: fixtures_dst.clone(),
            },
        )
        .expect("load_project must succeed against the test-fixtures cdylib");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "GraphicsKernelSmokeTestProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({
                "output_path": output_path_str,
            }),
        ))
        .expect("add_processor must succeed for the dlopened GraphicsKernelSmokeTestProcessor");

    runtime
        .start()
        .expect("runtime.start() must succeed (requires Vulkan device on this host)");

    // Manual processors fire setup then start synchronously inside
    // the runtime's processor-spawn path — by the time `start()`
    // returns, the smoke round-trip has run. Poll for the file
    // with a short timeout to absorb scheduling jitter (kernel
    // build + descriptor pool + offscreen submit + fence wait can
    // take a beat on a cold pipeline cache).
    let deadline = Instant::now() + Duration::from_secs(10);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "GraphicsKernelSmokeTestProcessor.start() did not write {} within 10s — \
         either the cdylib's `start` lifecycle didn't fire, or one of the \
         graphics-kernel vtable dispatches panicked at the FFI boundary",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "GraphicsKernelSmokeTestProcessor reported an error: {contents}"
    );
    assert_eq!(
        contents.trim(),
        "OK",
        "graphics-kernel smoke output must be exactly 'OK', got {contents:?}"
    );
}
