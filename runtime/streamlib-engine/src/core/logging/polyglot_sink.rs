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
//! [`LogRecord`] queue, each record stamped with the stream route of the
//! thread that produced it; the worker handles drain, serialization, and
//! fan-out identically. Only the producer boundary differs.
//!
//! Design decision recorded in issue #442, PR that landed the
//! escalate-IPC `log` op.
//!
//! [`RuntimeLogEvent`]: streamlib_runtime_client_contract::runtime_log_event::RuntimeLogEvent
//! [`JsonlSinkLayer`]: crate::core::logging::layer::JsonlSinkLayer

use crate::core::logging::layer::JsonlSinkLayer;
use crate::core::logging::record::LogRecord;

/// Enqueue a polyglot-origin record into the unified pathway, stamped with
/// the stream route of the calling thread — the bridge thread of the helper
/// that sent it.
///
/// Reaches the pathway the calling thread's dispatcher runs; silently
/// no-ops when that is none of the engine's — matching `tracing::*!()`
/// calls made before the pathway is installed.
pub(crate) fn push_polyglot_record(record: LogRecord) {
    let mut record_to_push = Some(record);
    tracing::dispatcher::get_default(|dispatch| {
        if let Some(layer) = dispatch.downcast_ref::<JsonlSinkLayer>()
            && let Some(record) = record_to_push.take()
        {
            layer.enqueue_in_this_threads_route(record);
        }
    });
}
