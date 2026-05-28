// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib smoke test for the opengl surface adapter.
//!
//! Loads the `OpenGlSmokeTestProcessor` from test-fixtures and drives
//! it through `start()`. The processor's body runs a full cdylib-side
//! adapter-construction round-trip inside `gpu.escalate(|full| ...)`.
//!
//! Output is one of three forms:
//!   - "OK\n<w>x<h>\ngl_texture=<n>" on full round-trip success.
//!   - "SKIP:<reason>" when EGL initialization fails (host lacks
//!     EGL display / extensions / runs in a sandbox); the test
//!     tolerates this as a clean exit because the cdylib-reach
//!     story up through `host_vulkan_texture_arc` has already been
//!     validated by the cpu-readback / vulkan smoke tests.
//!   - "ERR:<message>" on any other step failure.
//!
//! Requires a working Vulkan device. EGL is best-effort.

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

#[test]
#[serial]
fn dlopen_processor_round_trips_opengl_adapter() {
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

    let output_path = tmp.path().join("opengl_smoke_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path { path: fixtures_dst.clone(), build: BuildPolicy::NeverBuild },
        )
        .expect("add_module_with must succeed");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "OpenGlSmokeTestProcessor",
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

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "OpenGlSmokeTestProcessor.start() did not write {} within 10s",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();

    // SKIP is acceptable — EGL not available on this host.
    if contents.starts_with("SKIP:") {
        println!("opengl smoke skipped: {contents}");
        return;
    }

    assert!(
        !contents.starts_with("ERR:"),
        "OpenGlSmokeTestProcessor reported an error: {contents}"
    );

    // Format: "OK\n<w>x<h>\ngl_texture=<n>"
    let lines: Vec<&str> = contents.lines().collect();
    assert!(
        lines.len() >= 3,
        "expected 3 lines, got {contents:?}"
    );
    assert_eq!(lines[0], "OK", "first line must be 'OK', got {:?}", lines[0]);
    assert_eq!(
        lines[1], "64x64",
        "second line must be dims, got {:?}",
        lines[1]
    );
    assert!(
        lines[2].starts_with("gl_texture=") && lines[2] != "gl_texture=0",
        "third line must be a non-zero gl_texture id, got {:?}",
        lines[2]
    );
}
