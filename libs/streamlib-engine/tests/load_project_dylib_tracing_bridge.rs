// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `STREAMLIB_PLUGIN` ABI v2 — tracing-dispatch bridge end-to-end.
//!
//! The host's JSONL log carries the `[register]` line emitted by
//! `tracing::info!` inside `ProcessorInstanceFactory::register`
//! while running in the cdylib's address space. The cdylib's
//! `ForwardingSubscriber` (installed by `install_host_services`)
//! serializes the event's target / level / message / fields into
//! primitive payloads and calls the host's `tracing_emit` fn
//! pointer; the host builds a `LogRecord` and pushes it onto the
//! drain queue.
//!
//! Mentally revert
//! `crate::core::plugin::forwarding_subscriber::install_for_self` or
//! `emit_via_host_dispatch` in `host_services.rs` and this test
//! fails: cdylib `tracing::info!` calls drop silently.
//!
//! Runs in its own test binary so `PROCESSOR_REGISTRY` is fresh —
//! `register::<P>()` early-returns on a duplicate and would skip
//! the emit if a sibling test had already registered the same
//! processor.

use std::path::Path;
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::runtime::{BuildPolicy, Strategy, Runner};
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

fn build_and_stage_test_fixtures_dylib() -> (tempfile::TempDir, std::path::PathBuf) {
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
    assert!(
        built_dylib.exists(),
        "cdylib expected at {} after cargo build",
        built_dylib.display()
    );

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

    (tmp, fixtures_dst)
}

#[test]
#[serial]
fn plugin_register_tracing_event_reaches_host_jsonl() {
    let (_tmp, fixtures_dst) = build_and_stage_test_fixtures_dylib();

    let runtime = Runner::new().unwrap();
    let jsonl_path = runtime
        .jsonl_log_path()
        .map(|p| p.to_path_buf())
        .expect(
            "host runtime must own a JSONL log file — tracing bridge cannot be \
             verified without one",
        );

    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path { path: fixtures_dst.clone(), build: BuildPolicy::NeverBuild },
        )
        .expect("add_module_with must succeed");

    // Tracing→JSONL flow under the callback-table architecture:
    // cdylib's `ProcessorInstanceFactory::register::<P>()` calls
    // `tracing::info!` → cdylib's `ForwardingSubscriber::event` →
    // host's `tracing_emit` fn pointer → host's
    // `emit_via_host_dispatch` builds a `LogRecord` and pushes onto
    // the drain queue → host's drain worker writes JSONL. Worker
    // drain interval is ~25 ms by default; poll up to 2 s for the
    // line to land.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut found_line: Option<String> = None;
    while found_line.is_none() && Instant::now() < deadline {
        if let Ok(contents) = std::fs::read_to_string(&jsonl_path) {
            for line in contents.lines() {
                if line.contains("new processor type registered")
                    && line.contains("TestConfiguredProcessor")
                {
                    found_line = Some(line.to_string());
                    break;
                }
            }
        }
        if found_line.is_none() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    assert!(
        found_line.is_some(),
        "host JSONL at {} must contain the '[register] new processor type \
         registered' line for TestConfiguredProcessor — emitted from cdylib \
         code via tracing::info! and forwarded through the callback-table \
         tracing bridge. Without the ForwardingSubscriber or \
         emit_via_host_dispatch, the cdylib's tracing emit is dropped.",
        jsonl_path.display(),
    );
}
