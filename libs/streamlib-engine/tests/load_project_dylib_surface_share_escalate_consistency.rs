// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib smoke test verifying surface-share and escalate-IPC
//! paths agree on resource lifetime for the same `surface_id`.
//!
//! Loads the `SurfaceShareEscalateConsistencyTestProcessor` from
//! test-fixtures and drives it through `start()`. The processor's body
//! exercises every leg of the dual-registration / dual-lookup story
//! from inside a cdylib `gpu.escalate(|full| ...)` closure (see the
//! fixture's module docs for the full step list).
//!
//! Mental-revert: breaking the surface-share daemon's `lookup_texture`
//! wire format OR the escalate `resolve_texture_registration_by_surface_id`
//! vtable slot would surface as an `ERR:<msg>` line in the fixture's
//! output. Breaking the dual-registration drop semantics (host-side
//! `register_texture` / `register_texture_with_layout` double-counting
//! the Arc) would surface as a panic during `runtime.stop()` —
//! teardown is intentionally NOT silenced so a double-free shows up
//! in the test binary's exit.
//!
//! Requires a Vulkan device (`acquire_render_target_dma_buf_image`).

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
fn dlopen_processor_round_trips_surface_share_and_escalate_paths() {
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
    assert!(status.success(), "cargo build must succeed");

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

    let output_path = tmp.path().join("surface_share_escalate_smoke_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with(
            module_ident_any_version!("tatolab", "test-fixtures"),
            ModuleResolverStrategy::ManifestDirectory {
                path: fixtures_dst.clone(),
            },
        )
        .expect("add_module_with must succeed");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "SurfaceShareEscalateConsistencyTestProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({
                "output_path": output_path_str,
            }),
        ))
        .expect("add_processor must succeed");

    runtime
        .start()
        .expect("runtime.start() must succeed");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    // runtime.stop() runs all per-processor teardown plus the
    // surface-share daemon's unbind. Any double-free / leak in the
    // dual-registration path surfaces here as a panic; we don't
    // .ok() it so it can propagate.
    runtime.stop().expect("runtime.stop() must drain cleanly");

    assert!(
        output_path.exists(),
        "fixture did not write {} within 10s",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "fixture reported an error: {contents}"
    );

    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("OK"),
        "first line must be 'OK', got {contents:?}"
    );

    // Each of the four legs must report success.
    let expected = [
        "register_texture_with_layout=ok",
        "surface_store_register_texture=ok",
        "resolve_texture_registration=ok",
        "surface_store_lookup_texture=ok",
    ];
    for tag in expected {
        assert!(
            lines.iter().any(|l| *l == tag),
            "expected '{tag}' line in output, got {contents:?}"
        );
    }
}
