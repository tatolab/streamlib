// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot manual + continuous execution mode integration tests (#542).
//!
//! Drives the two reference example binaries
//! (`polyglot-manual-source-scenario` and
//! `polyglot-continuous-processor-scenario`) against both Python and
//! Deno polyglot SDKs end-to-end:
//!
//! - **Manual source**: spawns the binary, asserts exit-0 — which the
//!   binary returns only when it read back ≥ N frames written by a
//!   worker thread paced through `MonotonicTimer`. A regression that
//!   broke the worker-thread idiom (e.g. `start()` blocking the
//!   command loop) would leave the output file empty and the binary
//!   would exit non-zero.
//! - **Continuous processor**: spawns the binary, asserts exit-0 —
//!   which the binary returns only when the average inter-tick
//!   interval falls within `nominal ± 10ms` of the manifest's
//!   declared `interval_ms`. A regression to `time.sleep` /
//!   `setTimeout` semantics would still produce ticks but with
//!   different drift / no-drift behavior; this gate is the primary
//!   regression detector for the runner's monotonic-clock dispatch
//!   contract.
//!
//! Skips cleanly when prerequisites are missing (no python3, no deno,
//! no cdylibs built — same skip pattern as
//! `polyglot_linux_monotonic_clock_parity.rs`).

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    PathBuf::from(&manifest_dir)
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root canonicalize")
}

fn binary_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn locate_native_lib(file_name: &str) -> Option<PathBuf> {
    for profile in &["debug", "release"] {
        let candidate = workspace_root()
            .join("target")
            .join(profile)
            .join(file_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Locate the cargo-built example binary by name, falling back to a
/// `cargo build` if it's not on disk yet. Test runs do not assume the
/// caller pre-built the example.
fn locate_or_build_example(bin_name: &str, package: &str) -> Option<PathBuf> {
    for profile in &["debug", "release"] {
        let candidate = workspace_root()
            .join("target")
            .join(profile)
            .join(bin_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    // Fallback: build it.
    eprintln!("[polyglot_linux_execution_modes] building {package} ...");
    let status = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg(package)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let candidate = workspace_root()
        .join("target")
        .join("debug")
        .join(bin_name);
    candidate.exists().then_some(candidate)
}

/// Common precondition gate. Returns true if all polyglot pre-reqs are
/// present; emits a skip notice + returns false otherwise.
fn polyglot_prereqs_ok(test_name: &str) -> bool {
    if !binary_available("python3") {
        eprintln!("{test_name}: python3 not on PATH — skipping");
        return false;
    }
    if !binary_available("deno") {
        eprintln!("{test_name}: deno not on PATH — skipping");
        return false;
    }
    if locate_native_lib("libstreamlib_python_native.so").is_none() {
        eprintln!(
            "{test_name}: libstreamlib_python_native.so not built — skipping. \
             Run `cargo build -p streamlib-python-native`."
        );
        return false;
    }
    if locate_native_lib("libstreamlib_deno_native.so").is_none() {
        eprintln!(
            "{test_name}: libstreamlib_deno_native.so not built — skipping. \
             Run `cargo build -p streamlib-deno-native`."
        );
        return false;
    }
    true
}

fn run_scenario(bin: &Path, runtime: &str, test_name: &str) -> bool {
    eprintln!("{test_name}: running {} --runtime={runtime}", bin.display());
    let output = match Command::new(bin)
        .arg(format!("--runtime={runtime}"))
        // Force the binary's output dir under tempdir so concurrent test
        // runs don't collide on /tmp filenames.
        .env("CARGO_TARGET_TMPDIR", std::env::temp_dir())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{test_name}: failed to invoke {bin:?}: {e}");
            return false;
        }
    };
    if !output.status.success() {
        eprintln!(
            "{test_name}: scenario {runtime} FAILED (status={:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return false;
    }
    eprintln!(
        "{test_name}: scenario {runtime} PASSED — {}",
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .unwrap_or(""),
    );
    true
}

#[test]
fn polyglot_manual_source_python() {
    let test_name = "polyglot_manual_source_python";
    if !polyglot_prereqs_ok(test_name) {
        return;
    }
    let bin = match locate_or_build_example(
        "polyglot-manual-source-scenario",
        "polyglot-manual-source-scenario",
    ) {
        Some(p) => p,
        None => {
            eprintln!("{test_name}: could not locate or build example binary — skipping");
            return;
        }
    };
    assert!(
        run_scenario(&bin, "python", test_name),
        "manual source python scenario failed",
    );
}

#[test]
fn polyglot_manual_source_deno() {
    let test_name = "polyglot_manual_source_deno";
    if !polyglot_prereqs_ok(test_name) {
        return;
    }
    let bin = match locate_or_build_example(
        "polyglot-manual-source-scenario",
        "polyglot-manual-source-scenario",
    ) {
        Some(p) => p,
        None => {
            eprintln!("{test_name}: could not locate or build example binary — skipping");
            return;
        }
    };
    assert!(
        run_scenario(&bin, "deno", test_name),
        "manual source deno scenario failed",
    );
}

#[test]
fn polyglot_continuous_processor_python() {
    let test_name = "polyglot_continuous_processor_python";
    if !polyglot_prereqs_ok(test_name) {
        return;
    }
    let bin = match locate_or_build_example(
        "polyglot-continuous-processor-scenario",
        "polyglot-continuous-processor-scenario",
    ) {
        Some(p) => p,
        None => {
            eprintln!("{test_name}: could not locate or build example binary — skipping");
            return;
        }
    };
    assert!(
        run_scenario(&bin, "python", test_name),
        "continuous processor python scenario failed",
    );
}

#[test]
fn polyglot_continuous_processor_deno() {
    let test_name = "polyglot_continuous_processor_deno";
    if !polyglot_prereqs_ok(test_name) {
        return;
    }
    let bin = match locate_or_build_example(
        "polyglot-continuous-processor-scenario",
        "polyglot-continuous-processor-scenario",
    ) {
        Some(p) => p,
        None => {
            eprintln!("{test_name}: could not locate or build example binary — skipping");
            return;
        }
    };
    assert!(
        run_scenario(&bin, "deno", test_name),
        "continuous processor deno scenario failed",
    );
}

