// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tests for the escalate-IPC `{op:"log"}` variant (issue #442).
//!
//! These tests assert the full pipeline: wire parse → host dispatch →
//! polyglot sink → drain worker → JSONL file. Each test runs with
//! `#[serial]` and its own `TempDir`-scoped `XDG_STATE_HOME` so the
//! JSONL writer writes to a path we can read back.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;

use super::super::try_parse_escalate_request;
use super::log_record_from_wire;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestLog, EscalateRequestLogLevel, EscalateRequestLogSource,
};
use crate::core::logging::{
    LogLevel, RuntimeLogEvent, Source, StreamlibLoggingConfig, StreamlibLoggingGuard,
    init_for_tests, push_polyglot_record,
};
use crate::core::runtime::RuntimeUniqueId;

fn install_logging(runtime_tag: &str) -> (TempDir, StreamlibLoggingGuard) {
    let tmp = TempDir::new().unwrap();
    unsafe {
        std::env::set_var("XDG_STATE_HOME", tmp.path());
        // Capture debug+ so all the test levels surface.
        std::env::set_var("RUST_LOG", "debug");
        std::env::remove_var("STREAMLIB_QUIET");
    }
    let runtime_id = Arc::new(RuntimeUniqueId::from(runtime_tag));
    let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
    let guard = init_for_tests(config).unwrap();
    (tmp, guard)
}

fn read_jsonl(path: &std::path::Path) -> Vec<RuntimeLogEvent> {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<RuntimeLogEvent>(l).expect("valid JSONL"))
        .collect()
}

fn dispatch_log(log: EscalateRequestLog) {
    push_polyglot_record(log_record_from_wire(log));
}

fn sample_log(seq: &str, ts: &str, level: EscalateRequestLogLevel) -> EscalateRequestLog {
    EscalateRequestLog {
        source: EscalateRequestLogSource::Python,
        source_seq: seq.to_string(),
        source_ts: ts.to_string(),
        level,
        message: format!("record {seq}"),
        intercepted: false,
        channel: None,
        pipeline_id: Some("pl-1".into()),
        processor_id: Some("pr-1".into()),
        rhi_op: None,
        target: None,
        attrs: HashMap::new(),
    }
}

/// Every optional and required field on the wire round-trips
/// byte-for-byte through serde; the discriminator dispatches to
/// [`EscalateRequest::Log`] on decode.
#[test]
fn schema_round_trip() {
    let mut attrs = HashMap::new();
    attrs.insert("device".to_string(), Some(serde_json::json!("/dev/video0")));
    attrs.insert("count".to_string(), Some(serde_json::json!(3)));
    let original = EscalateRequestLog {
        source: EscalateRequestLogSource::Python,
        source_seq: "9001".into(),
        source_ts: "2026-04-23T14:00:00Z".into(),
        level: EscalateRequestLogLevel::Warn,
        message: "hello".into(),
        intercepted: true,
        channel: Some("fd1".into()),
        pipeline_id: Some("pl-42".into()),
        processor_id: Some("camera-1".into()),
        rhi_op: None,
        target: None,
        attrs: attrs.clone(),
    };
    let wrapped = EscalateRequest::Log(original.clone());
    let json = serde_json::to_value(&wrapped).expect("serializes");
    assert_eq!(json.get("op").and_then(|v| v.as_str()), Some("log"));

    let decoded: EscalateRequest = serde_json::from_value(json).expect("decodes");
    match decoded {
        EscalateRequest::Log(back) => {
            assert_eq!(back.source, original.source);
            assert_eq!(back.source_seq, original.source_seq);
            assert_eq!(back.source_ts, original.source_ts);
            assert_eq!(back.level, original.level);
            assert_eq!(back.message, original.message);
            assert_eq!(back.intercepted, original.intercepted);
            assert_eq!(back.channel, original.channel);
            assert_eq!(back.pipeline_id, original.pipeline_id);
            assert_eq!(back.processor_id, original.processor_id);
            assert_eq!(back.attrs, original.attrs);
        }
        other => panic!("expected Log variant, got {other:?}"),
    }
}

/// `level: "warn"` on the wire produces a JSONL record with
/// `level: "warn"`; required structured fields land in their
/// dedicated columns (not `attrs`) and `host_ts` is stamped
/// non-zero by the host.
#[test]
#[serial]
fn host_emits_jsonl_record_at_correct_level() {
    let (_tmp, guard) = install_logging("RlogOpLv");
    let path = guard.jsonl_path().unwrap().to_path_buf();

    dispatch_log(sample_log(
        "42",
        "2026-04-23T14:00:00Z",
        EscalateRequestLogLevel::Warn,
    ));

    drop(guard);

    let events = read_jsonl(&path);
    let record = events
        .iter()
        .find(|e| e.source == Source::Python && e.message == "record 42")
        .unwrap_or_else(|| panic!("no polyglot record; got {events:#?}"));
    assert_eq!(record.level, LogLevel::Warn);
    assert_eq!(record.source_seq, Some(42));
    assert_eq!(record.source_ts.as_deref(), Some("2026-04-23T14:00:00Z"));
    assert_eq!(record.pipeline_id.as_deref(), Some("pl-1"));
    assert_eq!(record.processor_id.as_deref(), Some("pr-1"));
    assert!(record.host_ts > 0, "host stamp must be non-zero");
}

/// An engine record a helper captured reaches the JSONL as the Rust
/// record it is — its own target and `rhi_op`, `source: "rust"` —
/// rather than as the helper's Python output, so `logs --target` finds
/// a call site in a child by the same name it has in the parent.
#[test]
#[serial]
fn a_captured_engine_record_lands_as_rust_with_its_own_target() {
    let (_tmp, guard) = install_logging("RlogOpRs");
    let path = guard.jsonl_path().unwrap().to_path_buf();

    dispatch_log(EscalateRequestLog {
        source: EscalateRequestLogSource::Rust,
        source_seq: "7".into(),
        source_ts: "2026-09-17T14:00:00Z".into(),
        level: EscalateRequestLogLevel::Warn,
        message: "InputMailboxes: bound local port has no mailbox".into(),
        intercepted: false,
        channel: None,
        pipeline_id: None,
        processor_id: Some("Pcamera".into()),
        rhi_op: Some("acquire_texture".into()),
        target: Some("streamlib_engine::iceoryx2::input".into()),
        attrs: HashMap::new(),
    });

    drop(guard);

    let events = read_jsonl(&path);
    let record = events
        .iter()
        .find(|e| e.message.starts_with("InputMailboxes:"))
        .unwrap_or_else(|| panic!("no captured engine record; got {events:#?}"));
    assert_eq!(record.source, Source::Rust);
    assert_eq!(record.target, "streamlib_engine::iceoryx2::input");
    assert_eq!(record.rhi_op.as_deref(), Some("acquire_texture"));
    assert_eq!(record.processor_id.as_deref(), Some("Pcamera"));
    assert_eq!(
        record.source_seq,
        Some(7),
        "a captured record shares the helper's sequence, so a gap in it still reads as loss"
    );
}

/// A record naming no target keeps the one its source implies — the
/// shape every `streamlib.log` call takes, and the only shape helpers
/// sent before engine records rode this op.
#[test]
fn a_record_naming_no_target_takes_its_sources_own() {
    let record = log_record_from_wire(sample_log(
        "1",
        "2026-09-17T14:00:00Z",
        EscalateRequestLogLevel::Info,
    ));

    assert_eq!(record.target, "streamlib::polyglot::python");
}

/// Two records with identical `source_ts` receive distinct
/// monotonically-increasing `host_ts` — subprocesses with broken
/// clocks can't collapse ordering by accident.
#[test]
#[serial]
fn host_stamps_host_ts() {
    let (_tmp, guard) = install_logging("RlogOpTs");
    let path = guard.jsonl_path().unwrap().to_path_buf();

    let ts = "2026-04-23T14:00:00Z";
    dispatch_log(sample_log("1", ts, EscalateRequestLogLevel::Info));
    std::thread::sleep(Duration::from_millis(2));
    dispatch_log(sample_log("2", ts, EscalateRequestLogLevel::Info));

    drop(guard);

    let events = read_jsonl(&path);
    let polyglot: Vec<_> = events
        .iter()
        .filter(|e| e.source == Source::Python)
        .collect();
    assert_eq!(polyglot.len(), 2, "expected exactly 2 polyglot records");
    assert_eq!(polyglot[0].source_ts, polyglot[1].source_ts);
    assert!(
        polyglot[1].host_ts > polyglot[0].host_ts,
        "host_ts must be monotonic: {} vs {}",
        polyglot[0].host_ts,
        polyglot[1].host_ts,
    );
}

/// `intercepted: true` + `channel: "fd1"` survive the wire → host
/// → JSONL hop untouched, landing in their dedicated columns.
#[test]
#[serial]
fn intercepted_flag_round_trip() {
    let (_tmp, guard) = install_logging("RlogOpInt");
    let path = guard.jsonl_path().unwrap().to_path_buf();

    let mut log = sample_log("7", "2026-04-23T14:00:00Z", EscalateRequestLogLevel::Error);
    log.intercepted = true;
    log.channel = Some("fd1".into());
    log.message = "fd1 capture".into();
    dispatch_log(log);

    drop(guard);

    let events = read_jsonl(&path);
    let record = events
        .iter()
        .find(|e| e.source == Source::Python && e.message == "fd1 capture")
        .unwrap_or_else(|| panic!("no polyglot record; got {events:#?}"));
    assert!(record.intercepted);
    assert_eq!(record.channel.as_deref(), Some("fd1"));
    assert_eq!(record.level, LogLevel::Error);
}

/// 1000 records with strictly increasing `source_seq` arrive at
/// the JSONL file in the same order. Proves the single-producer
/// path preserves FIFO without extra sequencing logic.
#[test]
#[serial]
fn within_source_fifo_preserved() {
    let (_tmp, guard) = install_logging("RlogOpFif");
    let path = guard.jsonl_path().unwrap().to_path_buf();

    for i in 0..1000 {
        dispatch_log(sample_log(
            &i.to_string(),
            "2026-04-23T14:00:00Z",
            EscalateRequestLogLevel::Debug,
        ));
    }

    drop(guard);

    let events = read_jsonl(&path);
    let seqs: Vec<u64> = events
        .iter()
        .filter(|e| e.source == Source::Python)
        .filter_map(|e| e.source_seq)
        .collect();
    assert_eq!(seqs.len(), 1000, "all records must land in JSONL");
    for (expected, got) in seqs.iter().enumerate() {
        assert_eq!(
            *got, expected as u64,
            "records out of order at index {expected}",
        );
    }
}

/// Rust and Python emit interleaved records into the unified
/// JSONL pathway. Verifies the architectural contract from #430:
/// `host_ts` is the authoritative sort key across the merged
/// stream (monotonically non-decreasing) and `source_seq` is
/// preserved within each subprocess source (monotonically
/// increasing). Rust records carry no `source_seq` because the
/// host-local tracing layer has no need for one — host receipt
/// IS the local order.
#[test]
#[serial]
fn cross_language_source_seq_monotonic_within_source() {
    let (_tmp, guard) = install_logging("RxLang");
    let path = guard.jsonl_path().unwrap().to_path_buf();

    // Round-robin emit Rust / Python. The subprocess
    // source carries a monotonic `source_seq`; Rust records do
    // not. A 50µs nap between emissions guarantees `host_ts`
    // strictly increases, which is the stronger property — the
    // contract only requires non-decreasing.
    const ROUNDS: u64 = 16;
    let mut py_seq = 0u64;
    for _ in 0..ROUNDS {
        tracing::info!(round = py_seq, "rust-merged");
        std::thread::sleep(Duration::from_micros(50));

        let py_log = EscalateRequestLog {
            source: EscalateRequestLogSource::Python,
            source_seq: py_seq.to_string(),
            source_ts: "2026-04-25T12:00:00Z".into(),
            level: EscalateRequestLogLevel::Info,
            message: format!("py-merged-{py_seq}"),
            intercepted: false,
            channel: None,
            pipeline_id: Some("pl-merge".into()),
            processor_id: Some("pr-merge".into()),
            rhi_op: None,
            target: None,
            attrs: HashMap::new(),
        };
        dispatch_log(py_log);
        py_seq += 1;
        std::thread::sleep(Duration::from_micros(50));
    }

    drop(guard);

    let events = read_jsonl(&path);

    let merged: Vec<&RuntimeLogEvent> = events
        .iter()
        .filter(|e| e.message.starts_with("rust-merged") || e.message.starts_with("py-merged-"))
        .collect();
    assert_eq!(
        merged.len(),
        (ROUNDS * 2) as usize,
        "expected {} merged-stream records, got {}: {merged:#?}",
        ROUNDS * 2,
        merged.len()
    );

    // host_ts is the authoritative cross-source order.
    for pair in merged.windows(2) {
        assert!(
            pair[1].host_ts >= pair[0].host_ts,
            "host_ts must be monotonic across merged stream: \
                     {} ({:?}) precedes {} ({:?})",
            pair[0].message,
            pair[0].host_ts,
            pair[1].message,
            pair[1].host_ts,
        );
    }

    // source_seq is monotonic within the subprocess source and
    // covers exactly [0, ROUNDS).
    let py_seqs: Vec<u64> = merged
        .iter()
        .filter(|e| e.source == Source::Python)
        .filter_map(|e| e.source_seq)
        .collect();
    assert_eq!(
        py_seqs,
        (0..ROUNDS).collect::<Vec<u64>>(),
        "python source_seq must be monotonic and contiguous"
    );
    // Rust records carry no source_seq — host-local tracing has
    // no use for one.
    let rust_records: Vec<&RuntimeLogEvent> = merged
        .iter()
        .copied()
        .filter(|e| e.source == Source::Rust)
        .collect();
    assert_eq!(rust_records.len(), ROUNDS as usize);
    for record in &rust_records {
        assert!(
            record.source_seq.is_none(),
            "rust records must not carry source_seq; got {:?}",
            record.source_seq,
        );
        assert_eq!(record.level, LogLevel::Info);
    }
}

/// End-to-end tests that spawn a real Python 3 subprocess, have it
/// call `streamlib.log.*`, read the framed escalate-IPC traffic off
/// its stdout, dispatch each frame through the host handler, and
/// assert the records land in the unified JSONL.
///
/// These sit above the wire-format unit tests in `log_op` and the
/// Python-side pytest suite — together they pin the whole loop from
/// `streamlib.log.info("msg")` in Python to a JSONL line on disk.
///
/// Skipped when `python3` is not on PATH (minimal sandboxes).
mod python_subprocess {
    use std::io::{BufReader, Read};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::Duration;

    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;
    use crate::core::compiler::compiler_ops::subprocess_bridge::{
        EscalateTransport, spawn_fd_line_reader,
    };
    use crate::core::logging::{
        LogLevel, RuntimeLogEvent, Source, StreamlibLoggingConfig, StreamlibLoggingGuard,
        init_for_tests,
    };
    use crate::core::runtime::RuntimeUniqueId;

    fn python3() -> Option<PathBuf> {
        let path_env = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_env) {
            let candidate = dir.join("python3");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn streamlib_python_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("streamlib-python")
            .join("python")
    }

    fn install_logging(tag: &str) -> (TempDir, StreamlibLoggingGuard) {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", tmp.path());
            std::env::set_var("RUST_LOG", "debug");
            std::env::remove_var("STREAMLIB_QUIET");
        }
        let runtime_id = Arc::new(RuntimeUniqueId::from(tag));
        let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
        let guard = init_for_tests(config).unwrap();
        (tmp, guard)
    }

    fn read_jsonl(path: &std::path::Path) -> Vec<RuntimeLogEvent> {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        contents
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<RuntimeLogEvent>(l).expect("valid JSONL"))
            .collect()
    }

    /// Run the given Python snippet with streamlib-python on PYTHONPATH.
    /// Returns `None` when `python3` is missing.
    ///
    /// Reads length-prefixed JSON frames from the subprocess stdout
    /// and feeds each through `try_parse_escalate_request` →
    /// `handle_escalate_op`, mirroring what the bridge's escalate worker
    /// does on a live host.
    fn run_and_drain(snippet: &str) -> Option<usize> {
        let py = python3()?;
        let lib = streamlib_python_path();
        if !lib.exists() {
            return None;
        }
        let mut child = Command::new(py)
            .arg("-c")
            .arg(snippet)
            .env("PYTHONPATH", &lib)
            .env_remove("PYTHONHOME")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn python3");

        let stdout = child.stdout.take().expect("child stdout");
        let mut reader = BufReader::new(stdout);
        let mut frame_count = 0usize;

        // The `process_bridge_message` pipeline expects a
        // `GpuContextLimitedAccess` for resource ops; log ops never
        // touch it. We build a parse → dispatch loop that handles
        // `log` directly via `handle_escalate_op` with a sandbox
        // that is never read on the log path. This keeps the test
        // independent of GPU availability.
        loop {
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("bridge read failed: {e}"),
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).expect("read frame body");
            let value: serde_json::Value = serde_json::from_slice(&buf).expect("valid JSON frame");
            let parsed = match try_parse_escalate_request(&value) {
                Some(Ok(op)) => op,
                Some(Err(e)) => panic!("escalate decode failed: {}", e.message),
                None => panic!("python subprocess only sends escalate traffic; got {value}"),
            };
            // For log ops we only need to drive the wire-decode →
            // sink path. Non-log ops are not expected from the
            // helper snippet.
            if let EscalateRequest::Log(log_op) = parsed {
                push_polyglot_record(log_record_from_wire(log_op));
                frame_count += 1;
            } else {
                panic!("unexpected escalate op from helper snippet");
            }
        }

        // Drain stderr for diagnostics.
        if let Some(mut stderr) = child.stderr.take() {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            if !s.is_empty() {
                eprintln!("python subprocess stderr:\n{s}");
            }
        }

        let _ = child.wait();
        Some(frame_count)
    }

    // Post-#604 the EscalateChannel takes a single writer; the
    // bridge reader thread (started by subprocess_runner.main)
    // owns the read side. These log-only tests don't need a
    // reader thread — they just enqueue records that the writer
    // thread frames onto stdout.
    const HELPER_PREAMBLE: &str = r#"
import sys
from streamlib import log
from streamlib.escalate import EscalateChannel
channel = EscalateChannel(sys.stdout.buffer)
log.set_processor_id("pr-test")
log.set_pipeline_id("pl-test")
log.install(channel, install_interceptors=False)
"#;

    /// `streamlib.log.info("hi", ...)` from Python surfaces in the
    /// host JSONL with `source=python`, correct message, level, and
    /// context fields.
    #[test]
    #[serial]
    fn python_log_surfaces_in_host_jsonl() {
        let (_tmp, guard) = install_logging("PyLogSurf");
        let path = guard.jsonl_path().unwrap().to_path_buf();

        let body = r#"
log.info("hi from python", count=7)
log.shutdown()
"#;
        let snippet = format!("{HELPER_PREAMBLE}{body}");
        let frames = match run_and_drain(&snippet) {
            Some(n) => n,
            None => {
                println!("python3 or streamlib-python source missing — skipping");
                return;
            }
        };
        assert!(frames >= 1, "expected at least one frame, got {frames}");

        drop(guard);

        let events = read_jsonl(&path);
        let record = events
            .iter()
            .find(|e| e.source == Source::Python && e.message == "hi from python")
            .unwrap_or_else(|| panic!("no python record; got {events:#?}"));
        assert_eq!(record.level, LogLevel::Info);
        assert_eq!(record.pipeline_id.as_deref(), Some("pl-test"));
        assert_eq!(record.processor_id.as_deref(), Some("pr-test"));
        assert_eq!(record.attrs.get("count").and_then(|v| v.as_i64()), Some(7));
        assert!(record.host_ts > 0);
    }

    /// A burst of 20 records arrives fully ordered and distinct — FIFO
    /// holds across the real `queue.Queue` + writer-thread →
    /// length-prefixed-frame → wire path.
    #[test]
    #[serial]
    fn python_log_burst_preserves_order() {
        let (_tmp, guard) = install_logging("PyLogBurst");
        let path = guard.jsonl_path().unwrap().to_path_buf();

        let body = r#"
for i in range(20):
    log.info("burst", index=i)
log.shutdown()
"#;
        let snippet = format!("{HELPER_PREAMBLE}{body}");
        let frames = match run_and_drain(&snippet) {
            Some(n) => n,
            None => {
                println!("python3 missing — skipping");
                return;
            }
        };
        assert_eq!(frames, 20, "subprocess should emit all 20 frames");

        drop(guard);

        let events = read_jsonl(&path);
        let indices: Vec<i64> = events
            .iter()
            .filter(|e| e.source == Source::Python && e.message == "burst")
            .filter_map(|e| e.attrs.get("index").and_then(|v| v.as_i64()))
            .collect();
        assert_eq!(indices.len(), 20, "all 20 records should land");
        assert_eq!(
            indices,
            (0..20).collect::<Vec<i64>>(),
            "order must match emission order"
        );
    }

    /// Spawn `python3` with the host's escalate-transport + fd1/fd2
    /// line readers installed exactly like the real spawn path does,
    /// then run a caller-supplied snippet. Parent closes its end of
    /// the escalate socketpair immediately so the child is free to
    /// exit once the snippet finishes. Returns the child handle +
    /// the kept-alive parent-side socket half (dropped by the
    /// caller after it's done with the run). `None` when `python3`
    /// isn't available.
    fn spawn_python_with_host_fd_readers(
        snippet: &str,
        processor_id: &str,
    ) -> Option<(std::process::Child, std::os::unix::net::UnixStream)> {
        let py = python3()?;
        let lib = streamlib_python_path();
        if !lib.exists() {
            return None;
        }
        let mut command = Command::new(py);
        command
            .arg("-c")
            .arg(snippet)
            .env("PYTHONPATH", &lib)
            .env_remove("PYTHONHOME")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut transport = EscalateTransport::attach(&mut command).expect("attach transport");

        let mut child = command.spawn().expect("spawn python3");
        transport.release_child_end();

        if let Some(stdout) = child.stdout.take() {
            spawn_fd_line_reader(stdout, "py-stdout", "fd1", processor_id);
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_fd_line_reader(stderr, "py-stderr", "fd2", processor_id);
        }

        let parent_socket = transport.into_parent_stream();
        Some((child, parent_socket))
    }

    /// A raw `os.write(1, …)` from a Python subprocess — the
    /// canonical case a C-extension or `printf` from a loaded C
    /// library would hit — must now surface in the host JSONL as
    /// `intercepted=true, channel="fd1", source="python"`. Deferred
    /// from #443; unlocked by moving escalate IPC onto the
    /// dedicated socketpair so fd1 is free to capture raw writes.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn python_os_write_fd1_intercepted() {
        let (_tmp, guard) = install_logging("PyFd1Intercept");
        let path = guard.jsonl_path().unwrap().to_path_buf();

        let snippet = r#"
import os
os.write(1, b"hi from c\n")
"#;
        let (mut child, _sock) = match spawn_python_with_host_fd_readers(snippet, "pr-fd1") {
            Some(v) => v,
            None => {
                println!("python3 missing — skipping");
                return;
            }
        };

        // Wait for child to exit and for the fd1 reader thread to
        // flush the final line into the JSONL worker queue.
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(200));

        drop(guard);

        let events = read_jsonl(&path);
        let record = events
            .iter()
            .find(|e| {
                e.intercepted
                    && e.channel.as_deref() == Some("fd1")
                    && e.source == Source::Python
                    && e.message == "hi from c"
            })
            .unwrap_or_else(|| panic!("no fd1-intercepted record for python; got {events:#?}"));
        assert_eq!(record.level, LogLevel::Warn);
        assert_eq!(record.processor_id.as_deref(), Some("pr-fd1"));
    }

    /// Sanity: fd2 capture survives the transport move. Confirms
    /// the existing fd2 path from #443 still works after #451
    /// promoted fd1 to a captured log pipe.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn python_stderr_fd2_intercepted_on_dedicated_fd_transport() {
        let (_tmp, guard) = install_logging("PyFd2Intercept");
        let path = guard.jsonl_path().unwrap().to_path_buf();

        let snippet = r#"
import os
os.write(2, b"stderr after transport move\n")
"#;
        let (mut child, _sock) = match spawn_python_with_host_fd_readers(snippet, "pr-fd2") {
            Some(v) => v,
            None => {
                println!("python3 missing — skipping");
                return;
            }
        };

        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(200));

        drop(guard);

        let events = read_jsonl(&path);
        let record = events
            .iter()
            .find(|e| {
                e.intercepted
                    && e.channel.as_deref() == Some("fd2")
                    && e.source == Source::Python
                    && e.message == "stderr after transport move"
            })
            .unwrap_or_else(|| panic!("no fd2-intercepted record for python; got {events:#?}"));
        assert_eq!(record.level, LogLevel::Warn);
        assert_eq!(record.processor_id.as_deref(), Some("pr-fd2"));
    }
}
