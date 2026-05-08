// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-runtime monotonic-clock parity test (#545).
//!
//! Validates that `streamlib.monotonic_now_ns()` (Python),
//! `monotonicNowNs()` (Deno), and `clock_gettime(CLOCK_MONOTONIC)` (host
//! Rust) all read from the same kernel-wide clock — i.e. their values
//! fall within a tight wall-time window when captured in lock-step.
//!
//! Pattern: each subprocess starts up, blocks on stdin, and on a single
//! signal byte captures its monotonic value and writes it to stdout. The
//! host samples its own value immediately before and after the signal.
//! All three values must fall within a bounded delta — the bound is
//! orchestration jitter (signal RTT through stdin/stdout), not the
//! clock itself.
//!
//! Skips cleanly when prerequisites are missing (no python3, no deno, no
//! cdylib built).
//!
//! Closes #545.

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Locate `libstreamlib_python_native.so` under the workspace target dir.
fn locate_python_native_lib() -> Option<PathBuf> {
    locate_native_lib("libstreamlib_python_native.so")
}

/// Locate `libstreamlib_deno_native.so` under the workspace target dir.
fn locate_deno_native_lib() -> Option<PathBuf> {
    locate_native_lib("libstreamlib_deno_native.so")
}

fn locate_native_lib(file_name: &str) -> Option<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let workspace = PathBuf::from(&manifest_dir).join("..").join("..");
    for profile in &["debug", "release"] {
        let candidate = workspace.join("target").join(profile).join(file_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
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

fn host_monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid stack slot; CLOCK_MONOTONIC is supported on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}

fn python_driver_source() -> &'static str {
    // Reads one byte from stdin (the signal); captures monotonic;
    // writes "<value>\n" to stdout; exits.
    r#"
import sys

# Add the SDK package path before import so we don't need it pip-installed.
sys.path.insert(0, sys.argv[1])

import streamlib

sys.stdin.buffer.read(1)
ns = streamlib.monotonic_now_ns()
sys.stdout.write(str(ns) + "\n")
sys.stdout.flush()
"#
}

fn deno_driver_source(deno_pkg_dir: &str, native_lib: &str) -> String {
    // Equivalent driver for Deno. Loads the cdylib and the SDK directly
    // from the workspace path; no jsr install required.
    format!(
        r#"
import {{ loadNativeLib }} from "{deno_pkg_dir}/native.ts";
import * as clock from "{deno_pkg_dir}/clock.ts";

const lib = loadNativeLib("{native_lib}");
clock.install(lib);

// Read one byte from stdin as the start signal.
const buf = new Uint8Array(1);
await Deno.stdin.read(buf);

const ns = clock.monotonicNowNs();
const text = new TextEncoder().encode(ns.toString() + "\n");
await Deno.stdout.write(text);
"#
    )
}

#[test]
fn python_and_deno_monotonic_clocks_share_epoch_with_host() {
    let py_native_lib = match locate_python_native_lib() {
        Some(p) => p,
        None => {
            eprintln!(
                "polyglot_linux_monotonic_clock_parity: libstreamlib_python_native.so not \
                 built; run `cargo build -p streamlib-python-native` first — skipping"
            );
            return;
        }
    };
    let deno_native_lib = match locate_deno_native_lib() {
        Some(p) => p,
        None => {
            eprintln!(
                "polyglot_linux_monotonic_clock_parity: libstreamlib_deno_native.so not \
                 built; run `cargo build -p streamlib-deno-native` first — skipping"
            );
            return;
        }
    };
    if !binary_available("python3") {
        eprintln!("polyglot_linux_monotonic_clock_parity: python3 not on PATH — skipping");
        return;
    }
    if !binary_available("deno") {
        eprintln!("polyglot_linux_monotonic_clock_parity: deno not on PATH — skipping");
        return;
    }

    // Resolve SDK source paths at the workspace.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace = PathBuf::from(&manifest_dir).join("..").join("..");
    let python_pkg = workspace
        .join("libs/streamlib-python/python")
        .canonicalize()
        .expect("python pkg path");
    let deno_pkg = workspace
        .join("libs/streamlib-deno")
        .canonicalize()
        .expect("deno pkg path");

    // Write the deno driver to a temp file (Deno doesn't accept inline
    // module sources via -e + bare import specifiers).
    let deno_driver = deno_driver_source(
        deno_pkg.to_str().unwrap(),
        deno_native_lib.to_str().unwrap(),
    );
    let tmp_dir = std::env::temp_dir().join("streamlib-clock-parity");
    std::fs::create_dir_all(&tmp_dir).expect("create tmp dir");
    let deno_driver_path = tmp_dir.join("clock_parity.ts");
    std::fs::write(&deno_driver_path, &deno_driver).expect("write deno driver");

    // Spawn python.
    let mut python = Command::new("python3")
        .arg("-c")
        .arg(python_driver_source())
        .arg(python_pkg.to_str().unwrap())
        .env("STREAMLIB_PYTHON_NATIVE", py_native_lib.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn python3");

    // Spawn deno. Permit-flags scoped to what the driver actually uses.
    let mut deno = Command::new("deno")
        .arg("run")
        .arg("--quiet")
        .arg("--allow-ffi")
        .arg("--allow-read")
        .arg(deno_driver_path.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn deno");

    // Both subprocesses block on stdin. Send the signal as close together
    // in wall-time as possible, bracketing with host monotonic samples.
    let host_before = host_monotonic_ns();
    python
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"\x01")
        .expect("signal python");
    deno.stdin
        .as_mut()
        .unwrap()
        .write_all(b"\x01")
        .expect("signal deno");
    // Drop stdins to flush + close so the child reads return immediately.
    drop(python.stdin.take());
    drop(deno.stdin.take());

    // Drain outputs. Each writes a single decimal nanosecond value + newline.
    let mut py_out = String::new();
    python
        .stdout
        .as_mut()
        .unwrap()
        .read_to_string(&mut py_out)
        .expect("read python stdout");
    let mut deno_out = String::new();
    deno.stdout
        .as_mut()
        .unwrap()
        .read_to_string(&mut deno_out)
        .expect("read deno stdout");
    let host_after = host_monotonic_ns();

    let py_status = python.wait().expect("python wait");
    let deno_status = deno.wait().expect("deno wait");

    if !py_status.success() {
        let mut stderr = String::new();
        python
            .stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut stderr)
            .ok();
        panic!("python driver failed: status={py_status:?} stderr={stderr}");
    }
    if !deno_status.success() {
        let mut stderr = String::new();
        deno.stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut stderr)
            .ok();
        panic!("deno driver failed: status={deno_status:?} stderr={stderr}");
    }

    let py_ns: u64 = py_out
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("python value not a u64 (got {py_out:?}): {e}"));
    let deno_ns: u64 = deno_out
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("deno value not a u64 (got {deno_out:?}): {e}"));

    // Pin the same-clock-domain invariant. Each subprocess captured its
    // value strictly between `host_before` and `host_after`, modulo
    // signal RTT and stdin-buffering jitter. Because the host samples
    // and the subprocess samples both call `clock_gettime(CLOCK_MONOTONIC)`,
    // values are directly comparable and cannot disagree by more than
    // the orchestration window.
    //
    // Window bound: 50 ms. Generous enough to absorb stdin-buffering /
    // process scheduling on a CI host, tight enough that a wrong-clock
    // bug (e.g. CLOCK_REALTIME drifting NTP-corrected, or a per-process
    // origin-relative timestamp) would blow it out by orders of magnitude.
    const WINDOW_NS: u64 = 50_000_000;

    let window = host_after.saturating_sub(host_before);
    assert!(
        py_ns >= host_before.saturating_sub(WINDOW_NS)
            && py_ns <= host_after.saturating_add(WINDOW_NS),
        "python clock outside host window: host_before={host_before} host_after={host_after} \
         py={py_ns} window={window}",
    );
    assert!(
        deno_ns >= host_before.saturating_sub(WINDOW_NS)
            && deno_ns <= host_after.saturating_add(WINDOW_NS),
        "deno clock outside host window: host_before={host_before} host_after={host_after} \
         deno={deno_ns} window={window}",
    );

    let py_to_deno_delta = py_ns.abs_diff(deno_ns);
    assert!(
        py_to_deno_delta <= WINDOW_NS,
        "python and deno clocks disagree beyond orchestration window: \
         py={py_ns} deno={deno_ns} delta={py_to_deno_delta}",
    );

    eprintln!(
        "polyglot_linux_monotonic_clock_parity: pass — host[{host_before}..{host_after}] \
         py={py_ns} deno={deno_ns} (delta_py_deno={py_to_deno_delta} ns)"
    );
}
