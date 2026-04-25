// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Drain worker. Pops [`LogRecord`]s from a bounded MPMC queue, enriches
//! them with `host_ts` / `runtime_id` / `source`, serializes to JSONL, and
//! fans out to an optional line-buffered pretty stdout mirror and an
//! optional batched JSONL writer.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{bounded, RecvTimeoutError, Sender};
use crossbeam_queue::ArrayQueue;

use crate::core::logging::config::ResolvedTunables;
use crate::core::logging::event::{LogLevel, RuntimeLogEvent, Source, SCHEMA_VERSION};
use crate::core::logging::record::LogRecord;
use crate::core::logging::writer::JsonlBatchedWriter;
use crate::core::runtime::RuntimeUniqueId;

/// Signals sent to the drain worker over the doorbell channel.
#[derive(Debug, Clone, Copy)]
pub(crate) enum WorkerSignal {
    /// A new record is available (or several).
    Record,
    /// Best-effort flush (panic hook, external caller).
    Flush,
    /// Final shutdown — drain, flush, fsync (if clean), exit.
    Shutdown,
}

pub(crate) struct WorkerHandle {
    pub queue: Arc<ArrayQueue<LogRecord>>,
    pub doorbell: Sender<WorkerSignal>,
    pub dropped: Arc<AtomicU64>,
    pub join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Send a best-effort flush request to the worker. Does NOT wait.
    pub fn request_flush(&self) {
        let _ = self.doorbell.try_send(WorkerSignal::Flush);
    }

    /// Send the final shutdown signal and join the worker thread. On
    /// return, all buffered records are on disk and (if applicable)
    /// `fdatasync`'d.
    pub fn shutdown_and_join(&mut self) {
        let _ = self.doorbell.send(WorkerSignal::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub(crate) struct WorkerConfig {
    pub runtime_id: Option<Arc<RuntimeUniqueId>>,
    pub source: Source,
    pub tunables: ResolvedTunables,
    /// Pretty-mirror sink. `None` disables the mirror entirely. When
    /// the fd-level interceptor is active this MUST point at the
    /// dup'd real-stdout handle, not at `std::io::stdout()`, otherwise
    /// mirror output re-enters the pipe and recurses.
    pub stdout_sink: Option<Box<dyn std::io::Write + Send>>,
    pub writer: Option<JsonlBatchedWriter>,
}

/// Spawn the drain worker and return its handle. The returned queue and
/// doorbell are used by tracing layers to enqueue records.
pub(crate) fn spawn(config: WorkerConfig) -> WorkerHandle {
    let queue = Arc::new(ArrayQueue::new(config.tunables.channel_capacity));
    let dropped = Arc::new(AtomicU64::new(0));
    let (doorbell_tx, doorbell_rx) = bounded(256);

    let runtime_id_str = config
        .runtime_id
        .as_ref()
        .map(|id| id.as_str().to_string())
        .unwrap_or_default();

    let queue_worker = Arc::clone(&queue);
    let dropped_worker = Arc::clone(&dropped);
    let mut stdout_sink = config.stdout_sink;
    let tunables = config.tunables;
    let source = config.source;
    let mut writer = config.writer;

    let join = std::thread::Builder::new()
        .name("streamlib-logging-drain".into())
        .spawn(move || {
            run_worker(
                queue_worker,
                doorbell_rx,
                dropped_worker,
                runtime_id_str,
                source,
                tunables,
                &mut stdout_sink,
                &mut writer,
            );
            // Final fsync happens inside run_worker on clean shutdown.
            drop(writer);
            drop(stdout_sink);
        })
        .expect("spawn drain worker thread");

    WorkerHandle {
        queue,
        doorbell: doorbell_tx,
        dropped,
        join: Some(join),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    queue: Arc<ArrayQueue<LogRecord>>,
    doorbell: crossbeam_channel::Receiver<WorkerSignal>,
    dropped: Arc<AtomicU64>,
    runtime_id: String,
    source: Source,
    tunables: ResolvedTunables,
    stdout_sink: &mut Option<Box<dyn std::io::Write + Send>>,
    writer: &mut Option<JsonlBatchedWriter>,
) {
    let mut last_flush = Instant::now();
    let mut last_dropped_emit = Instant::now();
    let mut last_dropped_seen: u64 = 0;

    let mut serialize_buf = Vec::with_capacity(1024);
    let mut pretty_buf = String::with_capacity(256);

    loop {
        // Remaining time until the next time-triggered flush. Floor at 1ms
        // so we always wake to service the queue.
        let timeout = tunables
            .batch_interval
            .saturating_sub(last_flush.elapsed())
            .max(Duration::from_millis(1));

        let signal = doorbell.recv_timeout(timeout);

        // Drain everything currently available.
        drain_queue(
            &queue,
            &runtime_id,
            source,
            writer,
            stdout_sink,
            &mut serialize_buf,
            &mut pretty_buf,
        );

        // Emit a synthetic `dropped=N` record if new drops have
        // accumulated since the last emission, rate-limited to once per
        // second or per 1000 drops (whichever fires first).
        let current_dropped = dropped.load(Ordering::Relaxed);
        let new_drops = current_dropped - last_dropped_seen;
        let since_last_emit = last_dropped_emit.elapsed();
        let emit_dropped =
            new_drops > 0 && (since_last_emit >= Duration::from_secs(1) || new_drops >= 1000);
        if emit_dropped {
            let synthetic = LogRecord {
                host_ts: now_ns(),
                level: LogLevel::Warn,
                target: "streamlib::logging".into(),
                message: format!("dropped {} log records", new_drops),
                pipeline_id: None,
                processor_id: None,
                rhi_op: None,
                intercepted: false,
                channel: None,
                attrs: {
                    let mut m = std::collections::BTreeMap::new();
                    m.insert(
                        "dropped".into(),
                        serde_json::Value::Number(new_drops.into()),
                    );
                    m.insert(
                        "source".into(),
                        serde_json::Value::String(source.as_str().into()),
                    );
                    m
                },
                source: None,
                source_ts: None,
                source_seq: None,
            };
            write_one(
                &synthetic,
                &runtime_id,
                source,
                writer,
                stdout_sink,
                &mut serialize_buf,
                &mut pretty_buf,
            );
            last_dropped_seen = current_dropped;
            last_dropped_emit = Instant::now();
        }

        // Periodic flush.
        if last_flush.elapsed() >= tunables.batch_interval {
            if let Some(w) = writer.as_mut() {
                let _ = w.flush_if_pending();
            }
            last_flush = Instant::now();
        }

        match signal {
            Ok(WorkerSignal::Record) | Err(RecvTimeoutError::Timeout) => {}
            Ok(WorkerSignal::Flush) => {
                if let Some(w) = writer.as_mut() {
                    let _ = w.flush_if_pending();
                }
                last_flush = Instant::now();
            }
            Ok(WorkerSignal::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                // Drain any remaining records that raced the shutdown.
                drain_queue(
                    &queue,
                    &runtime_id,
                    source,
                    writer,
                    stdout_sink,
                    &mut serialize_buf,
                    &mut pretty_buf,
                );
                if let Some(w) = writer.as_mut() {
                    let _ = w.flush_and_fsync();
                }
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drain_queue(
    queue: &ArrayQueue<LogRecord>,
    runtime_id: &str,
    source: Source,
    writer: &mut Option<JsonlBatchedWriter>,
    stdout_sink: &mut Option<Box<dyn std::io::Write + Send>>,
    serialize_buf: &mut Vec<u8>,
    pretty_buf: &mut String,
) {
    while let Some(record) = queue.pop() {
        write_one(
            &record,
            runtime_id,
            source,
            writer,
            stdout_sink,
            serialize_buf,
            pretty_buf,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn write_one(
    record: &LogRecord,
    runtime_id: &str,
    worker_source: Source,
    writer: &mut Option<JsonlBatchedWriter>,
    stdout_sink: &mut Option<Box<dyn std::io::Write + Send>>,
    serialize_buf: &mut Vec<u8>,
    pretty_buf: &mut String,
) {
    // A record carries its own `source` only when it originated outside the
    // worker's runtime (polyglot subprocess records). Tracing-sourced
    // records leave it `None` and inherit the worker's configured source.
    let source = record.source.unwrap_or(worker_source);

    // Build a full JSONL event and serialize once.
    let event = RuntimeLogEvent {
        schema_version: SCHEMA_VERSION,
        host_ts: record.host_ts,
        runtime_id: runtime_id.to_string(),
        source,
        level: record.level,
        message: record.message.clone(),
        target: record.target.clone(),
        pipeline_id: record.pipeline_id.clone(),
        processor_id: record.processor_id.clone(),
        rhi_op: record.rhi_op.clone(),
        source_ts: record.source_ts.clone(),
        source_seq: record.source_seq,
        intercepted: record.intercepted,
        channel: record.channel.clone(),
        attrs: record.attrs.clone(),
    };

    serialize_buf.clear();
    if serde_json::to_writer(&mut *serialize_buf, &event).is_err() {
        return;
    }

    if let Some(w) = writer.as_mut() {
        let _ = w.append_record(serialize_buf);
    }

    if let Some(sink) = stdout_sink.as_mut() {
        pretty_buf.clear();
        format_event_pretty(&event, pretty_buf);
        let _ = sink.write_all(pretty_buf.as_bytes());
        // Line-buffered: one flush per record so humans tail it live.
        let _ = sink.flush();
    }
}

/// Format one [`RuntimeLogEvent`] in the human-readable layout used by the
/// runtime's stdout mirror. `streamlib-cli logs` reuses this so the
/// replayed JSONL output is byte-for-byte identical to the live tail.
pub fn format_event_pretty(event: &RuntimeLogEvent, out: &mut String) {
    use std::fmt::Write;
    let level = match event.level {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => " INFO",
        LogLevel::Warn => " WARN",
        LogLevel::Error => "ERROR",
    };
    let _ = write!(
        out,
        "{} [{:>5}] [{}/{}] {} — {}",
        format_ns_timestamp(event.host_ts),
        level,
        event.runtime_id,
        event.source.as_str(),
        event.target,
        event.message,
    );
    if let Some(p) = &event.pipeline_id {
        let _ = write!(out, " pipeline_id={}", p);
    }
    if let Some(p) = &event.processor_id {
        let _ = write!(out, " processor_id={}", p);
    }
    if let Some(r) = &event.rhi_op {
        let _ = write!(out, " rhi_op={}", r);
    }
    for (k, v) in &event.attrs {
        let _ = write!(out, " {}={}", k, v);
    }
    out.push('\n');
}

fn format_ns_timestamp(ns: u64) -> String {
    // Compact `HH:MM:SS.mmm` — enough for humans tailing logs. Full
    // authoritative timestamp remains in the JSONL as `host_ts`.
    let secs_total = ns / 1_000_000_000;
    let ms = (ns % 1_000_000_000) / 1_000_000;
    let hh = (secs_total / 3600) % 24;
    let mm = (secs_total / 60) % 60;
    let ss = secs_total % 60;
    format!("{:02}:{:02}:{:02}.{:03}", hh, mm, ss, ms)
}

pub(crate) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
