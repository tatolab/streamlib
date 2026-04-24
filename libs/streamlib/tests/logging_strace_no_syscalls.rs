// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! #447 performance-contract regression test: the unified logging
//! pathway must perform **zero `write()` syscalls on the emitting
//! thread** during a 1000-event burst of `tracing::info!` calls.
//!
//! Architecture:
//! - Child binary `log_emit_1000` (see `tests/bin/log_emit_1000.rs`)
//!   installs the pathway, writes a `BURST_BEGIN` sentinel to fd 1,
//!   emits exactly 1000 `tracing::info!` events, then writes
//!   `BURST_END`. It also prints `EMITTER_TID=<n>` on stdout so this
//!   test can pick out the per-thread strace file produced by
//!   `strace -ff`.
//! - The test invokes the child under `strace -ff -s 256 -e trace=write
//!   -o <prefix>`, waits for completion, opens the per-thread strace
//!   file for the emitter, locates the `write(` lines containing the
//!   `BURST_BEGIN` and `BURST_END` sentinels, and asserts that no
//!   `write(` line appears strictly between them.
//!
//! Skipped on non-Linux and when `strace` is not on `$PATH`.

#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::process::Command;

    use tempfile::TempDir;

    fn strace_available() -> bool {
        Command::new("strace")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Full line count of `write(` syscalls strictly between the
    /// `BURST_BEGIN` and `BURST_END` sentinels. Both sentinels are
    /// themselves `write(` lines and are NOT counted. Returns `Err` if
    /// either sentinel is missing.
    fn count_writes_in_burst(trace: &str) -> Result<usize, String> {
        let mut begin_idx = None;
        let mut end_idx = None;
        for (i, line) in trace.lines().enumerate() {
            if line.contains("write(") && line.contains("BURST_BEGIN") {
                begin_idx = Some(i);
            } else if line.contains("write(") && line.contains("BURST_END") {
                end_idx = Some(i);
                break;
            }
        }
        let Some(begin) = begin_idx else {
            return Err("BURST_BEGIN sentinel not found in emitter trace".to_string());
        };
        let Some(end) = end_idx else {
            return Err("BURST_END sentinel not found in emitter trace".to_string());
        };
        if end <= begin {
            return Err(format!(
                "BURST_END ({end}) did not appear after BURST_BEGIN ({begin})"
            ));
        }
        let count = trace
            .lines()
            .enumerate()
            .filter(|(i, line)| *i > begin && *i < end && line.contains("write("))
            .count();
        Ok(count)
    }

    #[test]
    #[allow(clippy::disallowed_macros)]
    fn emitter_thread_issues_zero_write_syscalls_during_burst() {
        if !strace_available() {
            eprintln!(
                "[SKIP] strace not found on $PATH — skipping #447 write()-syscall regression test"
            );
            return;
        }

        let child_bin = PathBuf::from(env!("CARGO_BIN_EXE_log_emit_1000"));
        assert!(
            child_bin.exists(),
            "child binary not found at {}",
            child_bin.display()
        );

        let tmp = TempDir::new().expect("tempdir");
        let strace_prefix = tmp.path().join("trace");
        let jsonl_dir = tmp.path().join("jsonl");
        std::fs::create_dir_all(&jsonl_dir).expect("mkdir jsonl");

        let output = Command::new("strace")
            .arg("-ff")
            .arg("-s")
            .arg("256")
            .arg("-e")
            .arg("trace=write")
            .arg("-o")
            .arg(&strace_prefix)
            .arg(&child_bin)
            .env("STREAMLIB_STRACE_JSONL", &jsonl_dir)
            .output()
            .expect("spawn strace");

        assert!(
            output.status.success(),
            "child under strace exited non-zero: status={:?}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let tid_line = stdout
            .lines()
            .find(|l| l.starts_with("EMITTER_TID="))
            .unwrap_or_else(|| {
                panic!("child did not emit EMITTER_TID= on stdout; full stdout:\n{stdout}")
            });
        let tid: i64 = tid_line
            .trim_start_matches("EMITTER_TID=")
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse EMITTER_TID line {tid_line:?}: {e}"));

        let emitter_trace_path = tmp.path().join(format!("trace.{tid}"));
        let trace = std::fs::read_to_string(&emitter_trace_path).unwrap_or_else(|e| {
            let dir_listing = std::fs::read_dir(tmp.path())
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            panic!(
                "failed to read emitter strace file {}: {}\nfiles in tmp: {}",
                emitter_trace_path.display(),
                e,
                dir_listing
            )
        });

        let writes = count_writes_in_burst(&trace).unwrap_or_else(|e| {
            panic!("burst window parse failed: {e}\nemitter trace:\n{trace}")
        });

        assert_eq!(
            writes, 0,
            "expected 0 write() syscalls on emitter tid {tid} between BURST_BEGIN and BURST_END, \
             got {writes}. Full emitter trace:\n{trace}"
        );
    }
}

/// Non-Linux hosts: the strace-backed test cannot run, but cargo's
/// test harness expects at least one test per integration file. This
/// stub is `#[ignore]` with a clear skip message so it shows up as
/// skipped rather than fails.
#[cfg(not(target_os = "linux"))]
#[test]
#[ignore = "strace test is Linux-only (#447)"]
fn emitter_thread_issues_zero_write_syscalls_during_burst() {}
