// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Local reproducer for the residual concurrent-GPU-setup race
//! (RACE2_FINDINGS.md): a wide multi-processor `start()` fan-out where
//! cdylib processors build compute kernels, mirroring the drone-racer
//! pipeline (`UdpSource → … → JpegDecoder → …`) WITHOUT the Gitea
//! registry / publish loop. The whole thing is local: it `cargo build`s
//! the in-tree `streamlib-test-fixtures` cdylib and loads it via
//! `add_module(Strategy::Path)`.
//!
//! The drone-racer crash is an NVIDIA-Linux driver-internal corruption
//! during the GPU-heavy startup; it floats between device-init and the
//! setup fan-out and only fires under low logging overhead (`RUST_LOG=warn`).
//! The single-processor `load_project_dylib_*_smoke` tests don't trigger
//! it; this scales the fan-out width + GPU concurrency to try to.
//!
//! `#[ignore]` by default — run explicitly, it may SIGSEGV the test
//! process (that IS the reproduction):
//!   cargo test -p streamlib-engine --test concurrent_gpu_setup_repro \
//!     -- --ignored --nocapture --test-threads=1
//! Tune width/runs via env: `REPRO_GPU_PROCS` (default 8),
//! `REPRO_NONGPU_PROCS` (default 4).

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use serial_test::serial;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Runner, Strategy};
use streamlib::sdk::schema_ident;
use streamlib::sdk::RunnerAutoBuild;
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

/// Build the in-tree test-fixtures cdylib and stage it (+ its `@tatolab/core`
/// schema dep) as a `Strategy::Path` package in `tmp`. Returns the staged
/// test-fixtures package dir.
fn stage_test_fixtures(tmp: &Path) -> std::path::PathBuf {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "streamlib-test-fixtures"])
        .status()
        .expect("invoking cargo build");
    assert!(status.success(), "cargo build -p streamlib-test-fixtures must succeed");

    let dylib_name = format!("libstreamlib_test_fixtures.{}", "so");
    let built_dylib = workspace_root.join("target").join("debug").join(&dylib_name);

    let fixtures_src = workspace_root.join("packages/test-fixtures");
    let core_src = workspace_root.join("packages/core");
    let fixtures_dst = tmp.join("test-fixtures");
    let core_dst = tmp.join("core");

    std::fs::create_dir_all(&fixtures_dst).unwrap();
    std::fs::copy(
        fixtures_src.join("streamlib.yaml"),
        fixtures_dst.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_contents(&fixtures_src.join("schemas"), &fixtures_dst.join("schemas"));

    std::fs::create_dir_all(&core_dst).unwrap();
    std::fs::copy(core_src.join("streamlib.yaml"), core_dst.join("streamlib.yaml")).unwrap();
    copy_dir_contents(&core_src.join("schemas"), &core_dst.join("schemas"));

    let triple_dir = fixtures_dst.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    std::fs::copy(&built_dylib, triple_dir.join(&dylib_name)).unwrap();

    fixtures_dst
}

#[test]
#[serial]
#[ignore = "race #2 reproducer — run explicitly; may SIGSEGV the test process on NVIDIA"]
fn wide_concurrent_cdylib_gpu_setup_fanout() {
    let gpu_procs: usize = std::env::var("REPRO_GPU_PROCS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let nongpu_procs: usize = std::env::var("REPRO_NONGPU_PROCS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);

    let tmp = tempfile::tempdir().unwrap();
    let fixtures_dst = stage_test_fixtures(tmp.path());

    let runtime = Runner::with_auto_build().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path { path: fixtures_dst, build: BuildPolicy::NeverBuild },
        )
        .expect("add_module test-fixtures cdylib");

    let mut outputs = Vec::new();
    // GPU processors: each builds a compute kernel in its lifecycle —
    // the concurrent fan-out + the device pre-warm is the suspect window.
    for i in 0..gpu_procs {
        let out = tmp.path().join(format!("gpu_{i}.txt"));
        runtime
            .add_processor(ProcessorSpec::new(
                schema_ident!("tatolab", "test-fixtures", "ComputeKernelTestProcessor", "1.0.0"),
                json!({ "output_path": out.to_string_lossy(), "element_count": 256 }),
            ))
            .expect("add ComputeKernelTestProcessor");
        outputs.push(out);
    }
    // Non-GPU processors widen the fan-out (drone-racer = 9 threads, 1 GPU).
    for i in 0..nongpu_procs {
        runtime
            .add_processor(ProcessorSpec::new(
                schema_ident!("tatolab", "test-fixtures", "TestConfiguredProcessor", "1.0.0"),
                json!({ "threshold": i as f64 }),
            ))
            .expect("add TestConfiguredProcessor");
    }

    eprintln!(
        "[repro] starting runtime: {gpu_procs} GPU + {nongpu_procs} non-GPU processors \
         ({} total fan-out threads)",
        gpu_procs + nongpu_procs
    );
    // If race #2 reproduces, the process SIGSEGVs inside start().
    runtime.start().expect("runtime.start()");
    eprintln!("[repro] start() returned without crashing");

    let deadline = Instant::now() + Duration::from_secs(15);
    while outputs.iter().any(|p| !p.exists()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    runtime.stop().ok();

    for out in &outputs {
        let contents = std::fs::read_to_string(out).unwrap_or_default();
        assert!(
            contents.starts_with("OK"),
            "compute-kernel processor output {} was not OK: {contents:?}",
            out.display()
        );
    }
}
