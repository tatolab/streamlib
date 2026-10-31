// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Drain worker. Pops [`LogRecord`]s from a bounded MPMC queue, stamps each
//! with the `runtime_id`, `stream` and `action` its route entry names and its
//! `source`, and fans it out to an optional line-buffered pretty mirror on a
//! standard stream and to the JSONL log of the project whose stream or stream
//! action emitted it — for a record no project emitted, the runtime's own log
//! while a runtime keeps one.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{RecvTimeoutError, Sender, bounded};
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;
use streamlib_runtime_client_contract::runtime_log_event::{
    LogLevel, RuntimeLogEvent, SCHEMA_VERSION, Source,
};
use streamlib_runtime_client_contract::runtime_log_event_pretty_rendering::format_event_pretty;

use crate::core::logging::config::ResolvedTunables;
use crate::core::logging::project_log_route::{ProjectLogRoute, ProjectLogRouteEntry};
use crate::core::logging::record::LogRecord;

/// Signals sent to the drain worker over the doorbell channel.
#[derive(Debug)]
pub(crate) enum WorkerSignal {
    /// A new record is available (or several).
    Record,
    /// Best-effort flush (panic hook, external caller).
    Flush,
    /// Write every record queued before this signal, flush the project logs
    /// holding them, then answer on the sender.
    WriteEveryQueuedRecordThenAnswer(Sender<()>),
    /// Final shutdown — drain, flush, fsync (if clean), exit.
    Shutdown,
}

/// The bounded queue one drain worker pops records from, the doorbell that
/// wakes it, and its counts of records dropped to a full queue.
#[derive(Clone)]
pub(crate) struct DrainWorkerRecordQueue {
    pub queue: Arc<ArrayQueue<LogRecord>>,
    pub doorbell: Sender<WorkerSignal>,
    /// Records no project emitted that a full queue dropped.
    pub dropped: Arc<AtomicU64>,
    /// The emitters a full queue dropped records of since the worker last
    /// reported it; each counts its own.
    pub emitters_whose_records_were_dropped: Arc<Mutex<Vec<Arc<ProjectLogRouteEntry>>>>,
    /// The runtime's own log, written each record no project emitted while a
    /// runtime keeps one.
    pub runtime_own_log_entry: Arc<Mutex<Option<Arc<ProjectLogRouteEntry>>>>,
}

impl DrainWorkerRecordQueue {
    /// An empty queue holding `capacity` records, rung through `doorbell`.
    pub fn holding(capacity: usize, doorbell: Sender<WorkerSignal>) -> Self {
        Self {
            queue: Arc::new(ArrayQueue::new(capacity)),
            doorbell,
            dropped: Arc::new(AtomicU64::new(0)),
            emitters_whose_records_were_dropped: Arc::default(),
            runtime_own_log_entry: Arc::default(),
        }
    }

    /// Queue `record`, dropping the oldest when the queue is full.
    pub fn enqueue(&self, record: LogRecord) {
        // `force_push` returns the evicted record (if any); that is one drop,
        // counted against the emitter that emitted it.
        if let Some(evicted) = self.queue.force_push(record) {
            match evicted.project_log_route_entry {
                Some(entry) => {
                    if entry.note_a_record_dropped_from_a_full_queue() {
                        self.emitters_whose_records_were_dropped.lock().push(entry);
                    }
                }
                None => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let _ = self.doorbell.try_send(WorkerSignal::Record);
    }

    /// Send a best-effort flush request to the worker. Does NOT wait.
    pub fn request_flush(&self) {
        let _ = self.doorbell.try_send(WorkerSignal::Flush);
    }

    /// Block until the worker has written every record queued before this
    /// call, at most `budget`; `false` when it did not answer in time or is
    /// gone.
    pub fn wait_until_every_queued_record_is_written(&self, budget: Duration) -> bool {
        let (answer, answered) = bounded(1);
        if self
            .doorbell
            .send_timeout(
                WorkerSignal::WriteEveryQueuedRecordThenAnswer(answer),
                budget,
            )
            .is_err()
        {
            return false;
        }
        answered.recv_timeout(budget).is_ok()
    }
}

pub(crate) struct WorkerHandle {
    pub record_queue: DrainWorkerRecordQueue,
    pub join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Send the final shutdown signal and join the worker thread. On
    /// return, every queued record is written and every project log it
    /// wrote to is `fdatasync`'d.
    pub fn shutdown_and_join(&mut self) {
        let _ = self.record_queue.doorbell.send(WorkerSignal::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub(crate) struct WorkerConfig {
    pub source: Source,
    pub tunables: ResolvedTunables,
    /// Pretty-mirror sink. `None` disables the mirror entirely. When
    /// the fd-level interceptor is active this MUST point at the
    /// dup'd real standard stream, not at `std::io::stdout()` or
    /// `std::io::stderr()`, otherwise mirror output re-enters the pipe and
    /// recurses.
    pub pretty_log_mirror_sink: Option<Box<dyn std::io::Write + Send>>,
}

/// Spawn the drain worker and return its handle. The returned queue and
/// doorbell are used by tracing layers to enqueue records.
pub(crate) fn spawn(config: WorkerConfig) -> WorkerHandle {
    let (doorbell_tx, doorbell_rx) = bounded(256);
    let record_queue =
        DrainWorkerRecordQueue::holding(config.tunables.channel_capacity, doorbell_tx);

    let record_queue_of_the_worker = record_queue.clone();
    let mut pretty_log_mirror_sink = config.pretty_log_mirror_sink;
    let tunables = config.tunables;
    let source = config.source;

    let join = std::thread::Builder::new()
        .name("streamlib-logging-drain".into())
        .spawn(move || {
            run_worker(
                record_queue_of_the_worker,
                doorbell_rx,
                source,
                tunables,
                &mut pretty_log_mirror_sink,
            );
            drop(pretty_log_mirror_sink);
        })
        .expect("spawn drain worker thread");

    WorkerHandle {
        record_queue,
        join: Some(join),
    }
}

/// The project logs written to since their last flush.
#[derive(Default)]
struct ProjectLogFilesWrittenSinceTheirLastFlush(Vec<Arc<ProjectLogRoute>>);

impl ProjectLogFilesWrittenSinceTheirLastFlush {
    fn note(&mut self, route: &Arc<ProjectLogRoute>) {
        if !self.0.iter().any(|noted| Arc::ptr_eq(noted, route)) {
            self.0.push(Arc::clone(route));
        }
    }

    fn flush_each(&mut self) {
        for route in self.0.drain(..) {
            route.flush_the_records_pending_in_the_jsonl_log_file();
        }
    }

    fn flush_and_fsync_each(&mut self) {
        for route in self.0.drain(..) {
            route.flush_and_fsync_the_jsonl_log_file();
        }
    }
}

/// What a drain writes each record with.
struct DrainWorkerOutputs<'worker> {
    worker_source: Source,
    pretty_log_mirror_sink: &'worker mut Option<Box<dyn std::io::Write + Send>>,
    project_log_files_written: ProjectLogFilesWrittenSinceTheirLastFlush,
    serialize_buf: Vec<u8>,
    pretty_buf: String,
    /// The runtime's own log as the queue named it when this drain began.
    runtime_own_log_entry: Option<Arc<ProjectLogRouteEntry>>,
}

fn run_worker(
    record_queue: DrainWorkerRecordQueue,
    doorbell: crossbeam_channel::Receiver<WorkerSignal>,
    source: Source,
    tunables: ResolvedTunables,
    pretty_log_mirror_sink: &mut Option<Box<dyn std::io::Write + Send>>,
) {
    let queue = &record_queue.queue;
    let mut last_flush = Instant::now();
    let mut last_dropped_emit = Instant::now();
    let mut last_dropped_seen: u64 = 0;

    let mut outputs = DrainWorkerOutputs {
        worker_source: source,
        pretty_log_mirror_sink,
        project_log_files_written: ProjectLogFilesWrittenSinceTheirLastFlush::default(),
        serialize_buf: Vec::with_capacity(1024),
        pretty_buf: String::with_capacity(256),
        runtime_own_log_entry: None,
    };

    loop {
        // Remaining time until the next time-triggered flush. Floor at 1ms
        // so we always wake to service the queue.
        let timeout = tunables
            .batch_interval
            .saturating_sub(last_flush.elapsed())
            .max(Duration::from_millis(1));

        let signal = doorbell.recv_timeout(timeout);

        outputs
            .runtime_own_log_entry
            .clone_from(&record_queue.runtime_own_log_entry.lock());
        // Drain everything currently available.
        drain_queue(queue, &mut outputs);

        // Emit a synthetic `dropped=N` record if new drops have
        // accumulated since the last emission, rate-limited to once per
        // second or per 1000 drops (whichever fires first): one for the
        // records no project emitted, mirrored only, and one into each
        // emitter's project log for its own.
        let current_dropped = record_queue.dropped.load(Ordering::Relaxed);
        let new_drops = current_dropped - last_dropped_seen;
        let since_last_emit = last_dropped_emit.elapsed();
        if new_drops > 0 && (since_last_emit >= Duration::from_secs(1) || new_drops >= 1000) {
            write_one(
                &a_record_saying_records_were_dropped(new_drops, source, None),
                &mut outputs,
            );
            last_dropped_seen = current_dropped;
            last_dropped_emit = Instant::now();
        }
        if since_last_emit >= Duration::from_secs(1) {
            let emitters_whose_records_were_dropped =
                std::mem::take(&mut *record_queue.emitters_whose_records_were_dropped.lock());
            for entry in emitters_whose_records_were_dropped {
                let dropped_of_this_emitter = entry.take_the_count_of_records_dropped();
                if dropped_of_this_emitter > 0 {
                    write_one(
                        &a_record_saying_records_were_dropped(
                            dropped_of_this_emitter,
                            source,
                            Some(entry),
                        ),
                        &mut outputs,
                    );
                    last_dropped_emit = Instant::now();
                }
            }
        }

        // Periodic flush.
        if last_flush.elapsed() >= tunables.batch_interval {
            outputs.project_log_files_written.flush_each();
            last_flush = Instant::now();
        }

        match signal {
            Ok(WorkerSignal::Record) | Err(RecvTimeoutError::Timeout) => {}
            Ok(WorkerSignal::Flush) => {
                outputs.project_log_files_written.flush_each();
                last_flush = Instant::now();
            }
            Ok(WorkerSignal::WriteEveryQueuedRecordThenAnswer(answer)) => {
                // Pushed before the signal was sent, so the drain above took
                // every record the asker queued; this takes any since.
                drain_queue(queue, &mut outputs);
                outputs.project_log_files_written.flush_each();
                last_flush = Instant::now();
                let _ = answer.send(());
            }
            Ok(WorkerSignal::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                // Drain any remaining records that raced the shutdown.
                drain_queue(queue, &mut outputs);
                outputs.project_log_files_written.flush_and_fsync_each();
                break;
            }
        }
    }
}

/// The warning that `dropped_record_count` records of `entry`'s emitter — or
/// of no project — were dropped to a full queue.
fn a_record_saying_records_were_dropped(
    dropped_record_count: u64,
    source: Source,
    entry: Option<Arc<ProjectLogRouteEntry>>,
) -> LogRecord {
    LogRecord {
        host_ts: now_ns(),
        level: LogLevel::Warn,
        target: "streamlib::logging".into(),
        message: format!("dropped {dropped_record_count} log records"),
        pipeline_id: None,
        processor_id: None,
        rhi_op: None,
        intercepted: false,
        channel: None,
        attrs: std::collections::BTreeMap::from([
            (
                "dropped".to_string(),
                serde_json::Value::Number(dropped_record_count.into()),
            ),
            (
                "source".to_string(),
                serde_json::Value::String(source.as_str().into()),
            ),
        ]),
        source: None,
        source_ts: None,
        source_seq: None,
        project_log_route_entry: entry,
    }
}

fn drain_queue(queue: &ArrayQueue<LogRecord>, outputs: &mut DrainWorkerOutputs<'_>) {
    while let Some(record) = queue.pop() {
        write_one(&record, outputs);
    }
}

fn write_one(record: &LogRecord, outputs: &mut DrainWorkerOutputs<'_>) {
    // A record carries its own `source` only when it originated outside the
    // worker's runtime (polyglot subprocess records). Tracing-sourced
    // records leave it `None` and inherit the worker's configured source.
    let source = record.source.unwrap_or(outputs.worker_source);
    let entry = record
        .project_log_route_entry
        .as_ref()
        .or(outputs.runtime_own_log_entry.as_ref());

    // Build a full JSONL event and serialize once.
    let event = RuntimeLogEvent {
        schema_version: SCHEMA_VERSION,
        host_ts: record.host_ts,
        runtime_id: entry
            .map(|entry| entry.runtime_id().to_string())
            .unwrap_or_default(),
        stream: entry
            .and_then(|entry| entry.stream_id())
            .map(str::to_string),
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

    if let Some(entry) = entry {
        outputs.serialize_buf.clear();
        if serde_json::to_writer(&mut outputs.serialize_buf, &event).is_ok()
            && entry
                .route()
                .append_serialized_record(&outputs.serialize_buf)
        {
            outputs.project_log_files_written.note(entry.route());
        }
    }

    if let Some(sink) = outputs.pretty_log_mirror_sink.as_mut() {
        outputs.pretty_buf.clear();
        format_event_pretty(&event, &mut outputs.pretty_buf);
        let _ = sink.write_all(outputs.pretty_buf.as_bytes());
        // Line-buffered: one flush per record so humans tail it live.
        let _ = sink.flush();
    }
}

pub(crate) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
