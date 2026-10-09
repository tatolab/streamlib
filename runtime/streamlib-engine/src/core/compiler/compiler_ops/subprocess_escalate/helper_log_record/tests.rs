// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tests for the escalate-IPC `{op:"log"}` variant (issue #442).
//!
//! These tests assert the full pipeline: wire parse → host dispatch →
//! polyglot sink → drain worker → the JSONL file of the stream whose route
//! the pushing thread carries. Each test runs with `#[serial]` and its own
//! `TempDir` project so the file is one we can read back.

use std::collections::HashMap;
use std::time::Duration;

use serial_test::serial;
use streamlib_runtime_client_contract::runtime_log_event::{LogLevel, RuntimeLogEvent, Source};
use tempfile::TempDir;

use super::log_record_from_wire;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestLog, EscalateRequestLogLevel, EscalateRequestLogSource,
};
use crate::core::logging::one_stream_log_file_written_on_a_test_thread::{
    OneStreamLogFileWrittenOnThisTestThread, read_every_record_of_a_jsonl_log,
};
use crate::core::logging::{StreamlibLoggingConfig, push_polyglot_record};

/// A test pathway routing this thread's records — the relayed ones it pushes
/// included — into one stream's log file under a temporary project.
fn install_logging(runtime_id: &str) -> (TempDir, OneStreamLogFileWrittenOnThisTestThread) {
    let tmp = crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    unsafe {
        // Capture debug+ so all the test levels surface.
        std::env::set_var("RUST_LOG", "debug");
        std::env::remove_var("STREAMLIB_QUIET");
    }
    let config = StreamlibLoggingConfig::for_runtime("test");
    let logging = OneStreamLogFileWrittenOnThisTestThread::install(config, runtime_id, tmp.path());
    (tmp, logging)
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
    let (_tmp, logging) = install_logging("RlogOpLv");
    let path = logging.jsonl_log_path();

    dispatch_log(sample_log(
        "42",
        "2026-04-23T14:00:00Z",
        EscalateRequestLogLevel::Warn,
    ));

    logging.finish();

    let events = read_every_record_of_a_jsonl_log(&path);
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
    let (_tmp, logging) = install_logging("RlogOpRs");
    let path = logging.jsonl_log_path();

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

    logging.finish();

    let events = read_every_record_of_a_jsonl_log(&path);
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
/// shape every `tatolab.stream.log` call takes, and the only shape helpers
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
    let (_tmp, logging) = install_logging("RlogOpTs");
    let path = logging.jsonl_log_path();

    let ts = "2026-04-23T14:00:00Z";
    dispatch_log(sample_log("1", ts, EscalateRequestLogLevel::Info));
    std::thread::sleep(Duration::from_millis(2));
    dispatch_log(sample_log("2", ts, EscalateRequestLogLevel::Info));

    logging.finish();

    let events = read_every_record_of_a_jsonl_log(&path);
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
    let (_tmp, logging) = install_logging("RlogOpInt");
    let path = logging.jsonl_log_path();

    let mut log = sample_log("7", "2026-04-23T14:00:00Z", EscalateRequestLogLevel::Error);
    log.intercepted = true;
    log.channel = Some("fd1".into());
    log.message = "fd1 capture".into();
    dispatch_log(log);

    logging.finish();

    let events = read_every_record_of_a_jsonl_log(&path);
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
    let (_tmp, logging) = install_logging("RlogOpFif");
    let path = logging.jsonl_log_path();

    for i in 0..1000 {
        dispatch_log(sample_log(
            &i.to_string(),
            "2026-04-23T14:00:00Z",
            EscalateRequestLogLevel::Debug,
        ));
    }

    logging.finish();

    let events = read_every_record_of_a_jsonl_log(&path);
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
    let (_tmp, logging) = install_logging("RxLang");
    let path = logging.jsonl_log_path();

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

    logging.finish();

    let events = read_every_record_of_a_jsonl_log(&path);

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
