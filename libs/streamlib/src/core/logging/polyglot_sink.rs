// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Direct-enqueue sink for polyglot (Python / Deno subprocess) log records.
//!
//! # Why this bypasses `tracing::*!()`
//!
//! Polyglot log records arrive on the host as already-deserialized data
//! relayed from a subprocess via escalate IPC. They are *not* events in
//! the host's call graph. Two concrete reasons for the split:
//!
//! 1. **Semantic honesty.** `tracing::Event` represents something that
//!    happened inside this process's call graph. Any `tracing::span!`
//!    context on the thread receiving the escalate IPC would falsely
//!    decorate the polyglot record as if the subprocess work had
//!    happened inside that span. Treating polyglot records as data
//!    rather than events avoids that class of bug.
//!
//! 2. **Fidelity of `source` / `source_ts` / `source_seq`.** The JSONL
//!    schema ([`RuntimeLogEvent`]) has these fields as top-level
//!    columns, but [`JsonlSinkLayer`] captures tracing events into a
//!    [`LogRecord`] that carries `source: None` and funnels everything
//!    through the worker's configured `Source`. Routing polyglot
//!    records through `tracing::*!()` would stamp them as
//!    `source: "rust"` and drop `source_ts` / `source_seq` into
//!    `attrs` rather than their proper columns.
//!
//! Both producers (tracing layer + this sink) converge on the same
//! [`LogRecord`] queue; the worker handles drain, serialization, and
//! fan-out identically. Only the producer boundary differs.
//!
//! Design decision recorded in issue #442, PR that landed the
//! escalate-IPC `log` op.
//!
//! [`RuntimeLogEvent`]: crate::core::logging::event::RuntimeLogEvent
//! [`JsonlSinkLayer`]: crate::core::logging::layer::JsonlSinkLayer

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crossbeam_channel::Sender;
use crossbeam_queue::ArrayQueue;

use crate::core::logging::record::LogRecord;
use crate::core::logging::worker::{WorkerHandle, WorkerSignal};

/// Handle onto the drain worker's queue. Clone-friendly; pushing is
/// lock-free up to the bounded-queue capacity (drop-oldest beyond that).
pub(crate) struct PolyglotLogSink {
    queue: Arc<ArrayQueue<LogRecord>>,
    doorbell: Sender<WorkerSignal>,
    dropped: Arc<AtomicU64>,
}

impl PolyglotLogSink {
    pub(crate) fn from_worker(handle: &WorkerHandle) -> Self {
        Self {
            queue: Arc::clone(&handle.queue),
            doorbell: handle.doorbell.clone(),
            dropped: Arc::clone(&handle.dropped),
        }
    }

    /// Enqueue a polyglot-origin record. Drop-oldest when the queue is
    /// full; the lost record is counted into the shared dropped counter
    /// so the worker can surface a synthetic `dropped=N` record.
    pub(crate) fn push(&self, record: LogRecord) {
        if self.queue.force_push(record).is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        let _ = self.doorbell.try_send(WorkerSignal::Record);
    }
}

/// Process-wide sink. Installed by [`crate::core::logging::init`] when it
/// spawns the drain worker, cleared when the guard drops. Tests that use
/// `init_for_tests` reinstall a fresh sink per test (guarded by
/// `#[serial]`).
static GLOBAL: RwLock<Option<Arc<PolyglotLogSink>>> = RwLock::new(None);

pub(crate) fn install(sink: Arc<PolyglotLogSink>) {
    *GLOBAL.write().expect("polyglot sink lock poisoned") = Some(sink);
}

pub(crate) fn uninstall() {
    *GLOBAL.write().expect("polyglot sink lock poisoned") = None;
}

/// Enqueue a polyglot-origin record into the unified JSONL pipeline.
///
/// Silently no-ops when no logging runtime is installed — matches the
/// behaviour of `tracing::*!()` calls made before `init()` runs.
pub(crate) fn push_polyglot_record(record: LogRecord) {
    let guard = GLOBAL.read().expect("polyglot sink lock poisoned");
    if let Some(sink) = guard.as_ref() {
        sink.push(record);
    }
}
