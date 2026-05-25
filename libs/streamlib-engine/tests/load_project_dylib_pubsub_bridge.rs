// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `STREAMLIB_PLUGIN` ABI v2 — PUBSUB bridge end-to-end.
//!
//! A listener subscribed in the host before `load_project` receives
//! the `RuntimeDidRegisterProcessorType` event that cdylib code
//! publishes when it invokes `host_registry.register::<P>()`.
//! Mentally revert the `host_callbacks()`-routing branch in
//! `PubSub::publish` (or `host_pubsub_publish` in
//! `libs/streamlib-engine/src/core/plugin/host_services.rs`) and
//! this test fails: the cdylib's per-DSO `PUBSUB` falls through to
//! its own uninitialized iceoryx2 service and the publish lands in
//! a bus the host's subscriber never sees.
//!
//! Runs in its own test binary so `PROCESSOR_REGISTRY` and the
//! one-shot tracing/logging globals are fresh. The same-binary
//! `#[serial]`-guarded shape leaves `PROCESSOR_REGISTRY` populated
//! between tests, and `register::<P>()` early-returns on a
//! duplicate type-name — silently skipping the PUBSUB publish this
//! test asserts.

use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serial_test::serial;
use streamlib::sdk::pubsub::{topics, Event, EventListener, RuntimeEvent, PUBSUB};
use streamlib::sdk::runtime::Runner;
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

struct CountingListener {
    matched: Arc<AtomicUsize>,
}

impl EventListener for CountingListener {
    fn on_event(&mut self, event: &Event) -> streamlib::sdk::error::Result<()> {
        if let Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
            processor_type,
        }) = event
        {
            if processor_type.r#type.as_str() == "TestConfiguredProcessor" {
                self.matched.fetch_add(1, Ordering::SeqCst);
            }
        }
        Ok(())
    }
}

#[test]
#[serial]
fn plugin_register_pubsub_event_reaches_host_subscriber() {
    let (_tmp, fixtures_dst) = build_and_stage_test_fixtures_dylib();

    let runtime = Runner::new().unwrap();

    // Subscribe BEFORE load_project so the cdylib's register-time
    // publish has somewhere to land. Sleep ~200 ms so the subscriber
    // thread opens its iceoryx2 service before the publish (per the
    // pubsub-lazy-init-silent-noop learning's setup-time recipe).
    let matched = Arc::new(AtomicUsize::new(0));
    let listener_arc: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(CountingListener {
            matched: Arc::clone(&matched),
        }));
    PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener_arc));
    std::thread::sleep(Duration::from_millis(200));

    runtime
        .load_project(&fixtures_dst)
        .expect("load_project must succeed");

    let deadline = Instant::now() + Duration::from_secs(2);
    while matched.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        matched.load(Ordering::SeqCst) >= 1,
        "host subscriber should have received a RuntimeDidRegisterProcessorType \
         event for TestConfiguredProcessor — the cdylib's PUBSUB.publish call \
         from inside ProcessorInstanceFactory::register did not reach the \
         host's bus. Check the host_callbacks() routing branch in \
         `PubSub::publish` and the `host_pubsub_publish` callback in \
         libs/streamlib-engine/src/core/plugin/host_services.rs."
    );

    drop(listener_arc);
}
