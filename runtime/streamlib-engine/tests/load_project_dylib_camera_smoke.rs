// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera dlopen lifecycle smoke test (#958 / Phase D follow-up to #914).
//!
//! Builds `@tatolab/camera` as a cdylib, stages
//! it as a streamlib project alongside `@tatolab/core`, dlopens it
//! via `Runner::add_module_with(_, ManifestDirectory)`, then drives the camera processor
//! through `setup()` + `start()` against vivid (`/dev/video0` on this
//! host — vivid is exposed at `/dev/video0` on the workstation per
//! `reference_vivid_video_device`; the streamlib testing doc names
//! `/dev/video2` but that's a different vivid class not present here).
//!
//! The camera is the canonical streamlib processor — its `start()`
//! body opens V4L2 and spawns the capture thread, then returns. The
//! capture thread then runs the one-shot FullAccess primitive set
//! inside a single `gpu_limited_access().escalate(|full| { ... })`
//! block: `gpu_capabilities`, `color_converter`,
//! `create_command_recorder`, `create_timeline_semaphore`,
//! `acquire_storage_buffer`, `acquire_render_target_dma_buf_image`,
//! `import_dma_buf_storage_buffer`. After the escalate returns the
//! thread publishes the timeline via
//! `gpu_limited_access.set_video_source_timeline_semaphore` (the v12
//! vtable slot added in this PR) and enters the per-frame capture
//! loop.
//!
//! Exit-criterion-#3 of #914 ("camera processor loads as a cdylib
//! via `runtime.add_module_with_blocking(..., ManifestDirectory)` and completes
//! `setup()` + `start()` without panicking") is what this test locks:
//! `runtime.start()` and `runtime.stop()` both return `Ok` against
//! vivid, proving the
//! synchronous lifecycle dispatches through the cdylib FFI boundary
//! cleanly.
//!
//! What this test does NOT lock — guarded separately:
//!
//! > ~~End-to-end per-frame capture through the color converter +
//! > command recorder. Those PluginAbiObject return-type methods
//! > (`RhiColorConverter::prepare_buffer_to_image_storage`,
//! > `RhiCommandRecorder::record_*` / `submit_*`,
//! > `HostVulkanTimelineSemaphore::wait`) still panic in cdylib mode
//! > via their `host_inner()` short-circuits — Phase E follow-on
//! > work that lifts method dispatch to the FullAccess vtable per
//! > #907's shape. The capture thread therefore crashes shortly
//! > after the first frame.~~
//! > ~~The vulkan-video-roundtrip manual gate from the `/verify-live` skill
//! > with camera loaded as a cdylib — blocked by the same Phase E
//! > follow-on.~~ — Superseded 2026-05-24. The recorder methods
//! > vtable (`RhiCommandRecorderMethodsVTable`), the color-converter
//! > methods vtable's `prepare_buffer_to_image_storage` slot, the
//! > timeline-semaphore wait dispatch, the PortSchemaSpec lossless
//! > wire serde, and the `make_*_borrow` cached-fields contract all
//! > landed; the per-frame path now completes cleanly in cdylib mode,
//! > and the manual gate
//! > (`examples/vulkan-video-roundtrip-cdylib-camera` against vivid)
//! > produces valid SMPTE color-bar output matching the baseline
//! > non-cdylib variant.
//!
//! What the test actually surfaces:
//!
//! - `cargo build -p streamlib-camera` produces a
//!   `libstreamlib_camera.so` cdylib carrying the `STREAMLIB_PLUGIN`
//!   symbol.
//! - `Runner::add_module_with(...)` accepts the camera's project
//!   manifest (with `@tatolab/core` resolved via the dev-time `patch`
//!   entry mirrored into the staging tmpdir).
//! - The runtime adds a `Camera` processor configured to point at
//!   vivid, then `runtime.start()` fires `setup()` (cheap — clones
//!   `gpu_limited_access`) and `start()` (opens V4L2, spawns the
//!   capture thread).
//! - The capture thread reaches `setup_inner` which runs the
//!   FullAccess escalation. The test sleeps long enough for the
//!   escalation to complete, then stops the runtime; teardown joins
//!   the capture thread. A panic at the cdylib FFI boundary during
//!   any of the FullAccess calls would surface as a thread panic /
//!   abort during this window.
//!
//! What this test does NOT lock — guarded elsewhere:
//!
//! - Per-vtable-slot byte layout — `streamlib-plugin-abi`'s
//!   `offset_of!` layout regression tests.
//! - Per-FullAccess-callback correctness — engine-side unit tests
//!   (`gpu_full_access_vtable_tests`).
//! - End-to-end frame output — the manual gate
//!   `vulkan-video-roundtrip` per the `/verify-live` skill covers that.
//!
//! Requires a working Vulkan device + vivid (`/dev/video0`) on the
//! test host. CI has no GPU runner planned
//! (`project_ci_strategy_no_gpu`); the test runs locally and fails
//! clean on hosts missing either prerequisite via the standard
//! assertion path.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use serial_test::serial;
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
fn dlopen_camera_processor_completes_lifecycle_against_vivid() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    // Build streamlib-camera as a cdylib carrying the
    // `STREAMLIB_PLUGIN` symbol that the runtime dlopens.
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "streamlib-camera"])
        .status()
        .expect("invoking cargo build for streamlib-camera");
    assert!(
        status.success(),
        "cargo build -p streamlib-camera must succeed"
    );

    let dylib_ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let dylib_name = format!("libstreamlib_camera.{dylib_ext}");
    let built_dylib = workspace_root
        .join("target")
        .join("debug")
        .join(&dylib_name);
    assert!(
        built_dylib.exists(),
        "camera cdylib not at expected path: {}",
        built_dylib.display()
    );

    // Stage as a streamlib project the runtime can `add_module_with`.
    // Mirror the host workspace's `packages/camera/` +
    // `packages/core/` layout inside the tmpdir so the camera's
    // `streamlib.yaml` dev-time `patch: "@tatolab/core": path:
    // ../core` resolves.
    let tmp = tempfile::tempdir().unwrap();
    let camera_src = workspace_root.join("packages/camera");
    let core_src = workspace_root.join("packages/core");
    let camera_dst = tmp.path().join("packages/camera");
    let core_dst = tmp.path().join("packages/core");

    std::fs::create_dir_all(&camera_dst).unwrap();
    std::fs::copy(
        camera_src.join("streamlib.yaml"),
        camera_dst.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_contents(&camera_src.join("schemas"), &camera_dst.join("schemas"));

    std::fs::create_dir_all(&core_dst).unwrap();
    std::fs::copy(
        core_src.join("streamlib.yaml"),
        core_dst.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_contents(&core_src.join("schemas"), &core_dst.join("schemas"));

    let triple_dir = camera_dst.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    std::fs::copy(&built_dylib, triple_dir.join(&dylib_name)).unwrap();

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "camera"),
            Strategy::Path {
                path: camera_dst.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("add_module_with must succeed against the camera cdylib");

    let ident = schema_ident!("tatolab", "camera", "Camera", "1.0.0");

    // vivid is at `/dev/video0` on this host. The streamlib testing
    // doc names `/dev/video2`, but that's a different vivid class
    // not present here (`reference_vivid_video_device`). If a future
    // workstation reshuffles this, pass the desired path via the
    // `STREAMLIB_CAMERA_DEVICE` env var.
    let device_id =
        std::env::var("STREAMLIB_CAMERA_DEVICE").unwrap_or_else(|_| "/dev/video0".to_string());
    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({
                "device_id": device_id,
                // Cap below vivid's max so the capture-loop allocations
                // stay light — the smoke test doesn't need 4K.
                "max_width": 1280u32,
                "max_height": 720u32,
            }),
        ))
        .expect("add_processor must succeed for the dlopened Camera");

    runtime.start().expect(
        "runtime.start() must succeed — proves Camera::setup() and \
         Camera::start() ran through the cdylib FFI boundary without \
         panicking (requires Vulkan device + vivid on this host)",
    );

    // The Camera processor's `start()` spawns the V4L2 capture
    // thread, which then runs the one-shot FullAccess escalation
    // (`gpu_capabilities`, `color_converter`, `create_command_recorder`,
    // `create_timeline_semaphore`, `acquire_storage_buffer`,
    // `acquire_render_target_dma_buf_image`, `import_dma_buf_storage_buffer`)
    // inside `gpu_limited_access().escalate(|full| { ... })`. Sleep
    // long enough for the escalation to complete on a cold pipeline
    // cache. A panic at the cdylib FFI boundary during any of those
    // FullAccess calls surfaces as a thread-side panic during this
    // window — `runtime.stop()`'s teardown joins the capture thread
    // and propagates the panic state.
    std::thread::sleep(Duration::from_secs(3));

    runtime
        .stop()
        .expect("runtime.stop() must succeed — proves teardown joined the capture thread cleanly");
}
