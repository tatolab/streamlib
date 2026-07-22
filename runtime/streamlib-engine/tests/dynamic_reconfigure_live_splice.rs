// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Live add/remove reconfigure against a *started* runtime (#340).
//!
//! The `module_loader` unit tests exercise `add_module` / `remove_module`
//! around a module LOAD — never against a graph that has already been
//! `start()`ed and is actively running. This integration test closes that
//! gap: it starts a real runtime with one continuous processor, then, while
//! the compiler loop is live, adds a SECOND processor and removes it again,
//! asserting each stage through the processor's own file-based lifecycle
//! markers (the same cross-address-space proof `load_project_dylib_pause_resume`
//! uses — the markers are written by cdylib code, so they can't be faked by
//! the host test binary).
//!
//! What it locks:
//! - A live `add_processor` on a STARTED runtime causes the compiler to
//!   construct AND start the new instance (its `SETUP` + `PROCESS:` markers
//!   appear in a fresh file) — not merely enqueue a pending op that only
//!   materializes on the next `start()`.
//! - A live `remove_processor` on a STARTED runtime causes the compiler to
//!   stop AND tear the instance down (`TEARDOWN` marker appears).
//! - The processor that was NOT touched keeps running across the reconfigure
//!   (its `PROCESS:` count strictly increases while the other is spliced in
//!   and out) — the live splice does not stall the rest of the graph.
//!
//! Mental revert: gate `add_processor` / `remove_processor` to reject calls
//! while the runtime is running, or make the compiler skip pending ops once
//! started, and the corresponding marker never appears → the poll times out →
//! this test fails.
//!
//! Scope: this locks live `add_processor` / `remove_processor` on a started
//! runtime ONLY. It does NOT exercise `connect` / `disconnect` on a started
//! runtime — the probes are portless, so there is no cross-address-space
//! delivery marker to assert a rewire against. The live `connect` / `disconnect`
//! splice the `dynamic-reconfigure` example performs (camera → passthrough →
//! display and back) is verified visually via `/verify-live`, not here.
//!
//! Runs on the rig (GPU-backed `Runner`), headless (no window). Not part of
//! the `--lib` CI, which never builds `tests/` integration binaries.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use serial_test::serial;
use streamlib::sdk::RunnerAutoBuild;
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

/// Count marker lines in the probe's output file that satisfy `matches`.
/// A missing file reads as zero (the probe hasn't run its first hook yet).
fn count_lines<F: Fn(&str) -> bool>(output_path: &Path, matches: F) -> usize {
    std::fs::read_to_string(output_path)
        .map(|contents| contents.lines().filter(|l| matches(l)).count())
        .unwrap_or(0)
}

/// Poll `count_lines(output_path, matches)` until it reaches `target` or the
/// timeout elapses; returns the final observed count.
fn wait_for_lines<F: Fn(&str) -> bool>(
    output_path: &Path,
    matches: F,
    target: usize,
    timeout: Duration,
) -> usize {
    let deadline = Instant::now() + timeout;
    let mut last = 0;
    while Instant::now() < deadline {
        last = count_lines(output_path, &matches);
        if last >= target {
            return last;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}

/// Build the `streamlib-test-fixtures` cdylib and stage it (plus its
/// `@tatolab/core` schema dep) into a temp package tree the runtime can
/// `add_module_with` under `BuildPolicy::NeverBuild`. Mirrors the staging
/// recipe every `load_project_dylib_*` test uses.
fn build_and_stage_test_fixtures() -> (tempfile::TempDir, std::path::PathBuf) {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "streamlib-test-fixtures"])
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
    let built_dylib = workspace_root
        .join("target")
        .join("debug")
        .join(&dylib_name);

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
fn live_add_and_remove_processor_on_a_started_runtime() {
    let (tmp, fixtures_dst) = build_and_stage_test_fixtures();

    // Two independent probe files: `surviving` belongs to the processor that
    // stays wired through the whole run; `spliced` belongs to the processor
    // added and removed live.
    let surviving_path = tmp.path().join("surviving_probe.txt");
    let spliced_path = tmp.path().join("spliced_probe.txt");

    let runtime = Runner::with_auto_build().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "test-fixtures"),
            Strategy::Path {
                path: fixtures_dst.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("add_module_with must succeed against a real test-fixtures cdylib");

    let probe_ident = || {
        schema_ident!(
            "tatolab",
            "test-fixtures",
            "LifecycleProbeProcessor",
            "1.0.0"
        )
    };

    // The surviving probe runs for the whole test; a high cap keeps it
    // appending PROCESS lines throughout so we can prove it never stalls.
    runtime
        .add_processor(ProcessorSpec::new(
            probe_ident(),
            json!({
                "output_path": surviving_path.to_string_lossy(),
                "max_iterations": 1_000_000u32,
            }),
        ))
        .expect("add surviving processor before start");

    runtime.start().expect("runtime.start");

    // The runtime is now STARTED and the surviving probe is running.
    let initial_surviving = wait_for_lines(
        &surviving_path,
        |l| l.starts_with("PROCESS:"),
        1,
        Duration::from_secs(10),
    );
    assert!(
        initial_surviving >= 1,
        "surviving probe must be PROCESSing before the live splice; got {initial_surviving}"
    );

    // ---- Live ADD against the started runtime --------------------------------
    // The spliced probe must not exist yet — this file is fresh.
    assert_eq!(
        count_lines(&spliced_path, |_| true),
        0,
        "spliced probe file must be empty before the live add"
    );

    let spliced_id = runtime
        .add_processor(ProcessorSpec::new(
            probe_ident(),
            json!({
                "output_path": spliced_path.to_string_lossy(),
                "max_iterations": 1_000_000u32,
            }),
        ))
        .expect("live add_processor must succeed on a STARTED runtime");

    // The compiler must CONSTRUCT (SETUP) and START (PROCESS) the new instance
    // live — not just enqueue a pending op that waits for another start().
    let spliced_setup = wait_for_lines(&spliced_path, |l| l == "SETUP", 1, Duration::from_secs(10));
    assert_eq!(
        spliced_setup, 1,
        "live-added processor must run SETUP once; the compiler did not construct \
         it against the started runtime"
    );
    let spliced_process = wait_for_lines(
        &spliced_path,
        |l| l.starts_with("PROCESS:"),
        1,
        Duration::from_secs(10),
    );
    assert!(
        spliced_process >= 1,
        "live-added processor must reach its PROCESS loop; the compiler constructed \
         but never started it against the running graph"
    );

    // The surviving probe must have kept running while the splice was added.
    let mid_surviving = count_lines(&surviving_path, |l| l.starts_with("PROCESS:"));
    assert!(
        mid_surviving > initial_surviving,
        "surviving probe stalled across the live add ({initial_surviving} → {mid_surviving})"
    );

    // ---- Live REMOVE against the started runtime -----------------------------
    runtime
        .remove_processor(&spliced_id)
        .expect("live remove_processor must succeed on a STARTED runtime");

    // The compiler must STOP + TEARDOWN the removed instance live.
    let spliced_teardown =
        wait_for_lines(&spliced_path, |l| l == "TEARDOWN", 1, Duration::from_secs(10));
    assert!(
        spliced_teardown >= 1,
        "live-removed processor must run TEARDOWN; the compiler did not destroy it \
         against the started runtime"
    );

    // The surviving probe must STILL be running after the removal — the live
    // splice-out did not disturb the rest of the graph.
    let after_remove = count_lines(&surviving_path, |l| l.starts_with("PROCESS:"));
    let resumed = wait_for_lines(
        &surviving_path,
        |l| l.starts_with("PROCESS:"),
        after_remove + 1,
        Duration::from_secs(10),
    );
    assert!(
        resumed > after_remove,
        "surviving probe stopped PROCESSing after the live remove ({after_remove} → {resumed}); \
         removing one processor must not stall the running graph"
    );

    runtime.stop().expect("runtime.stop");

    // A clean stop tears the surviving probe down.
    let surviving_teardown = wait_for_lines(
        &surviving_path,
        |l| l == "TEARDOWN",
        1,
        Duration::from_secs(10),
    );
    assert!(
        surviving_teardown >= 1,
        "surviving probe must TEARDOWN on runtime.stop()"
    );
}
