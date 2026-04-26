// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end EPOLLHUP-watchdog integration test (#520).
//!
//! Wires the generic `SubprocessCrashHarness` against the real
//! per-runtime surface-share service. Spawns the
//! `surface_share_crash_helper` binary, waits for it to `check_in` a
//! memfd-backed surface, SIGKILLs it, and asserts the host-side
//! watchdog releases the surface within a short, bounded window —
//! plus that the surface backing is immediately reusable and that
//! `/proc/self/fd` returns to its pre-spawn baseline.

#![cfg(target_os = "linux")]

use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use streamlib::linux_surface_share::{SurfaceShareState, UnixSocketSurfaceService};
use streamlib_adapter_abi::testing::{CrashTiming, SubprocessCrashHarness};
use streamlib_surface_client::{connect_to_surface_share_socket, send_request_with_fds};

/// Locate the test helper binary built by `cargo test` under `target/<profile>/`.
fn locate_helper_binary() -> PathBuf {
    // CARGO_MANIFEST_DIR is libs/streamlib; the binary lands in
    // target/{debug,release}/surface_share_crash_helper. cargo sets
    // CARGO_BIN_EXE_<name> automatically for tests in the same package
    // — prefer that when present, fall back to a manual lookup.
    if let Some(p) = option_env!("CARGO_BIN_EXE_surface_share_crash_helper") {
        return PathBuf::from(p);
    }
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let workspace = PathBuf::from(&manifest_dir).join("..").join("..");
    for profile in &["debug", "release"] {
        let candidate = workspace
            .join("target")
            .join(profile)
            .join("surface_share_crash_helper");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!("surface_share_crash_helper binary not built");
}

fn tmp_socket_path(label: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!(
        "streamlib-surface-share-watchdog-{}-{}-{}.sock",
        label,
        std::process::id(),
        nanos
    ));
    p
}

/// Live fd count for the current process — `/proc/self/fd` entries.
/// The watchdog's correctness gate: any leak shows up as a baseline
/// drift across the spawn/cleanup window.
fn live_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|d| d.count())
        .unwrap_or(0)
}

fn make_memfd_with(contents: &[u8]) -> RawFd {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    let name = std::ffi::CString::new("watchdog-test-memfd").unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    assert!(fd >= 0, "memfd_create: {}", std::io::Error::last_os_error());
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(contents).expect("memfd write");
    file.seek(SeekFrom::Start(0)).expect("memfd rewind");
    file.into_raw_fd()
}

/// Subprocess crashes mid-flight after `check_in` — host watchdog must
/// release the surface, and the runtime must remain healthy enough to
/// accept a fresh registration immediately afterwards.
#[test]
fn watchdog_cleans_up_surface_after_subprocess_sigkill() {
    let helper = locate_helper_binary();
    assert!(helper.exists(), "helper binary missing: {:?}", helper);

    let state = SurfaceShareState::new();
    let socket_path = tmp_socket_path("crash");
    let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
    service.start().expect("service start");
    // Give the listener thread a tick to bind before any subprocess connects.
    std::thread::sleep(Duration::from_millis(50));

    let runtime_id = format!("watchdog-runtime-{}", std::process::id());
    let baseline_fds = live_fd_count();

    // Configure the harness's command — we don't pipe stdin/out/err because
    // the test reads state directly. The helper's connection survives the
    // post_spawn hook returning because the helper holds the OwnedFd, not
    // the parent.
    let mut command = Command::new(&helper);
    command
        .env("STREAMLIB_SURFACE_SOCKET", &socket_path)
        .env("STREAMLIB_RUNTIME_ID", &runtime_id)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let runtime_id_for_observer = runtime_id.clone();
    let state_for_observer = state.clone();
    let runtime_id_for_post = runtime_id.clone();
    let state_for_post = state.clone();

    let outcome = SubprocessCrashHarness::new(command)
        // 50 ms is enough for the helper to connect, send `check_in`, and
        // print SURFACE_ID; we still verify it landed before kill via the
        // post_spawn hook below, so a slow CI host won't false-pass.
        .with_timing(CrashTiming::AfterDelay(Duration::from_millis(50)))
        .with_cleanup_timeout(Duration::from_secs(2))
        .with_post_spawn(move |_child| {
            // Wait until the helper has actually registered its surface
            // with the host. Without this, a sluggish spawn could see kill
            // happen before check_in lands and the test would silently pass
            // with nothing to clean up.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let n = state_for_post.surface_ids_by_runtime(&runtime_id_for_post).len();
                if n >= 1 {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "helper did not register a surface before kill window",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
        .run(move || {
            if state_for_observer
                .surface_ids_by_runtime(&runtime_id_for_observer)
                .is_empty()
            {
                Ok(())
            } else {
                Err("watchdog has not yet released subprocess surfaces")
            }
        })
        .expect("harness run");

    // Per the issue's exit criteria: cleanup must complete within 1 s of
    // SIGKILL. The harness reports cleanup_latency, which is wall-clock
    // between kill and the observe_cleanup closure returning Ok.
    assert!(
        outcome.cleanup_latency < Duration::from_secs(1),
        "watchdog cleanup_latency {:?} exceeds 1s budget",
        outcome.cleanup_latency
    );

    // Surface really gone from the global table.
    assert!(
        state.surface_ids_by_runtime(&runtime_id).is_empty(),
        "surfaces by runtime should be empty after watchdog cleanup",
    );

    // Backing immediately reusable: a fresh check_in under any runtime_id
    // works (no leftover wedge in the service / state).
    let stream = connect_to_surface_share_socket(&socket_path).expect("post-cleanup connect");
    let send_fd = make_memfd_with(b"post-cleanup-fixture");
    let (resp, _) = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "check_in",
            "runtime_id": "post-cleanup",
            "width": 16,
            "height": 16,
            "format": "Bgra32",
            "resource_type": "pixel_buffer",
        }),
        &[send_fd],
        0,
    )
    .expect("post-cleanup check_in");
    unsafe { libc::close(send_fd) };
    let new_surface_id = resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("post-cleanup surface_id")
        .to_string();
    assert!(!new_surface_id.is_empty());
    let _ = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "release",
            "surface_id": new_surface_id,
            "runtime_id": "post-cleanup",
        }),
        &[],
        0,
    )
    .expect("post-cleanup release");
    drop(stream);

    service.stop();

    // FD baseline: anything left allocated by the helper (the connection,
    // the dup'd DMA-BUF fds the host kept) must be reaped. The service has
    // closed its listener and its registered fds; the helper exited; the
    // post-cleanup connection above was a roundtrip that closed cleanly.
    // Allow a small slack — the test runner spawns its own threads and
    // tracing layers can momentarily hold pipe fds.
    let after_fds = live_fd_count();
    assert!(
        after_fds <= baseline_fds + 2,
        "fd baseline drift after watchdog cleanup: baseline={}, after={}",
        baseline_fds,
        after_fds,
    );
}
