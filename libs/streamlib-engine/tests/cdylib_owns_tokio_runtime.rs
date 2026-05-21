// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration test for the architectural claim from #885 / #895: a
//! dlopen'd processor whose `tokio` crate is statically linked into
//! the cdylib can spin up its OWN `tokio::runtime::Runtime` in `setup`
//! and call `tokio::net::TcpListener::bind` against it — even though
//! the host runtime never exposes its tokio handle across the plugin
//! ABI (the locked design from #885).
//!
//! What this locks: a refactor that removes the plugin-owned runtime
//! from `TcpBindTestProcessor`, or one that breaks the cdylib's
//! `start` lifecycle so the bind never fires, makes this test fail
//! by either not writing the output file at all or writing an
//! `ERR:<message>` line. Mentally revert the
//! `tokio::runtime::Builder::new_current_thread().enable_all().build()`
//! call in `tcp_bind_test_processor.rs` to a `tokio::runtime::Handle`
//! borrow from the engine and the bind future fails to find its TLS
//! and panics — this test catches that regression.

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
fn dlopen_processor_owns_tokio_runtime_and_binds_tcp_listener() {
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

    let output_path = tmp.path().join("tcp_bind_result.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    let runtime = Runner::new().unwrap();
    runtime
        .load_project(&fixtures_dst)
        .expect("load_project must succeed against a real test-fixtures cdylib");

    let ident = schema_ident!(
        "tatolab",
        "test-fixtures",
        "TcpBindTestProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({ "output_path": output_path_str }),
        ))
        .expect("add_processor must succeed for the dlopened TcpBindTestProcessor");

    runtime.start().expect("runtime.start() must succeed");

    // Manual processors fire setup then start synchronously inside the
    // runtime's processor-spawn path — by the time `start()` returns,
    // the bind has been driven on the plugin's own runtime. Poll for
    // the file with a short timeout to absorb any scheduling jitter
    // from the spawn op without making the test flaky.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "TcpBindTestProcessor.start() did not write {} within 5s — \
         either the cdylib's `start` lifecycle didn't fire, or the \
         plugin-owned tokio runtime couldn't drive the TcpListener::bind future",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "TcpBindTestProcessor reported a bind error: {contents}"
    );
    let port: u16 = contents
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("expected a bound port, got {contents:?}: {e}"));
    assert!(
        port > 0,
        "kernel-assigned ephemeral port must be non-zero, got {port}"
    );
}
