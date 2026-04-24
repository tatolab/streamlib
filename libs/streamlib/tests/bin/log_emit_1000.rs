// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Strace target for the `logging_strace_no_syscalls` integration test.
//! Installs the unified logging pathway and emits exactly 1000
//! `tracing::info!` events on the main thread. Writes `BURST_BEGIN` /
//! `BURST_END` sentinels directly to fd 1 via `libc::write` so the test
//! can locate the burst window in the strace output and count
//! intermediate writes (expected: zero).
//!
//! Also prints `EMITTER_TID=<n>` to stdout so the test can pick the
//! emitting thread's per-thread strace file produced by `strace -ff`.
//! This binary necessarily uses raw stdout/stderr (not tracing!) to
//! communicate with the parent test harness — routing through tracing
//! would land bytes in the pipeline under measurement.
#![allow(clippy::disallowed_macros)]

use std::sync::Arc;

use streamlib::core::logging::{init_for_tests, LoggingTunables, StreamlibLoggingConfig};
use streamlib::core::runtime::RuntimeUniqueId;

fn raw_write_stdout(bytes: &[u8]) {
    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            bytes.as_ptr() as *const libc::c_void,
            bytes.len(),
        );
    }
}

fn main() {
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i64;
    // Plain println! is fine here — this prefix is consumed by the
    // parent test reading the child's stdout, and the `write(1, ...)`
    // syscall it produces happens BEFORE the BURST_BEGIN sentinel so
    // the test doesn't count it.
    println!("EMITTER_TID={tid}");
    // Flush so the EMITTER_TID line reaches the parent before the
    // child proceeds into the measured burst window.
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    let jsonl_path = std::env::var_os("STREAMLIB_STRACE_JSONL")
        .map(std::path::PathBuf::from)
        .expect("STREAMLIB_STRACE_JSONL must point at a temp dir");

    unsafe {
        std::env::set_var("XDG_STATE_HOME", &jsonl_path);
        std::env::set_var("STREAMLIB_QUIET", "1");
        std::env::set_var("RUST_LOG", "info");
    }

    let runtime_id = Arc::new(RuntimeUniqueId::from("RstraceEmit"));
    let config = StreamlibLoggingConfig {
        service_name: "log_emit_1000".into(),
        runtime_id: Some(runtime_id),
        stdout: false,
        jsonl: true,
        intercept_stdio: false,
        tunables: LoggingTunables {
            batch_ms: Some(100),
            batch_bytes: Some(64 * 1024),
            channel_capacity: Some(65_536),
            fsync_on_every_batch: None,
        },
    };
    let guard = init_for_tests(config).expect("install logging pathway");

    // Unbuffered sentinels straddle the 1000-event burst. Between these
    // two `write(1, ...)` calls on this tid, the test expects zero
    // further `write()` syscalls — any other write would indicate the
    // hot path is hitting I/O.
    raw_write_stdout(b"BURST_BEGIN\n");

    for i in 0u32..1000 {
        tracing::info!(
            pipeline_id = "strace-test",
            processor_id = "emitter",
            rhi_op = "tick",
            i,
            "strace-no-syscalls"
        );
    }

    raw_write_stdout(b"BURST_END\n");

    drop(guard);
}
