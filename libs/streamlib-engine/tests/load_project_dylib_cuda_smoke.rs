// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib smoke test for the cuda surface adapter (OPAQUE_FD
//! buffer path).
//!
//! Output is one of three forms:
//!   - "OK\n<w>x<h>\nvk_buffer=Buffer(0x<hex>)" on full round-trip.
//!   - "SKIP:<reason>" when the device doesn't expose an OPAQUE_FD
//!     buffer pool (e.g., external memory unsupported on this driver);
//!     the test tolerates this as a clean exit.
//!   - "ERR:<message>" on any other step failure.
//!
//! Requires a working Vulkan device.

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
fn dlopen_processor_round_trips_cuda_adapter() {
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

    let output_path = tmp.path().join("cuda_smoke_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .load_project(&fixtures_dst)
        .expect("load_project must succeed");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "CudaSmokeTestProcessor",
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
        "CudaSmokeTestProcessor.start() did not write {} within 10s",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();

    // SKIP is acceptable — driver doesn't expose OPAQUE_FD buffer pool.
    if contents.starts_with("SKIP:") {
        println!("cuda smoke skipped: {contents}");
        return;
    }

    assert!(
        !contents.starts_with("ERR:"),
        "CudaSmokeTestProcessor reported an error: {contents}"
    );

    // Format: "OK\n<w>x<h>\nvk_buffer=Buffer(0x<hex>)"
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
        lines[2].starts_with("vk_buffer=")
            && !lines[2].contains("0x0)")
            && lines[2] != "vk_buffer=Buffer(0)",
        "third line must be a non-null vk_buffer handle, got {:?}",
        lines[2]
    );
}
