// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tracing` layer that captures events, stamps each with the stream route of
//! the thread it was emitted on, and pushes [`LogRecord`]s onto the drain
//! worker's bounded queue. Hot path: no fd writes, no
//! formatting beyond `Debug` on the message field, and at most one
//! allocation per captured field value. All fan-out work (JSON
//! serialization, file I/O, stdout write) happens on the drain worker
//! thread.

use std::collections::BTreeMap;
use std::fmt;

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::core::logging::config::ResolvedTunables;
use crate::core::logging::loaded_stream_log_route::the_loaded_stream_log_route_of_this_thread;
use crate::core::logging::record::LogRecord;
use crate::core::logging::worker::{DrainWorkerRecordQueue, now_ns};
use streamlib_runtime_client_contract::runtime_log_event::{LogLevel, Source};

pub(crate) struct JsonlSinkLayer {
    record_queue: DrainWorkerRecordQueue,
    /// How the stream log files the queue's drain worker writes batch, sync
    /// and rotate; `None` for a queue no drain worker empties, such as a
    /// helper process's capture ring.
    stream_log_file_tunables: Option<ResolvedTunables>,
}

impl JsonlSinkLayer {
    /// A layer feeding a drain worker that writes each stream's log file as
    /// `stream_log_file_tunables` say.
    pub(crate) fn feeding_a_drain_worker_that_writes_stream_log_files(
        record_queue: DrainWorkerRecordQueue,
        stream_log_file_tunables: ResolvedTunables,
    ) -> Self {
        Self {
            record_queue,
            stream_log_file_tunables: Some(stream_log_file_tunables),
        }
    }

    /// A layer feeding a queue that something other than a drain worker
    /// empties, so no stream log file is opened through it.
    pub(crate) fn feeding_a_queue_that_writes_no_stream_log_file(
        record_queue: DrainWorkerRecordQueue,
    ) -> Self {
        Self {
            record_queue,
            stream_log_file_tunables: None,
        }
    }

    /// The queue a stream log file opened through this layer is written
    /// from, and how that file batches, syncs and rotates.
    pub(crate) fn record_queue_that_writes_stream_log_files(
        &self,
    ) -> Option<(DrainWorkerRecordQueue, ResolvedTunables)> {
        self.stream_log_file_tunables
            .map(|tunables| (self.record_queue.clone(), tunables))
    }

    /// Queue `record`, stamped with the route of the calling thread.
    pub(crate) fn enqueue_in_this_threads_route(&self, mut record: LogRecord) {
        record.loaded_stream_log_route = the_loaded_stream_log_route_of_this_thread();
        self.record_queue.enqueue(record);
    }
}

impl<S> Layer<S> for JsonlSinkLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level: LogLevel = (*metadata.level()).into();

        let mut visitor = Capture::default();
        event.record(&mut visitor);

        let record = LogRecord {
            host_ts: now_ns(),
            level,
            target: metadata.target().to_string(),
            message: visitor.message.unwrap_or_default(),
            pipeline_id: visitor.pipeline_id,
            processor_id: visitor.processor_id,
            rhi_op: visitor.rhi_op,
            intercepted: visitor.intercepted,
            channel: visitor.channel,
            attrs: visitor.attrs,
            // `source` is typically None for first-party Rust call-sites
            // (the worker stamps it as `Source::Rust` on serialize). Set
            // explicitly when a tracing event captures a subprocess pipe
            // (the Python stderr forwarder passes `source = "python"`).
            source: visitor.source,
            source_ts: None,
            source_seq: None,
            loaded_stream_log_route: None,
        };

        self.enqueue_in_this_threads_route(record);
    }
}

#[derive(Default)]
struct Capture {
    message: Option<String>,
    pipeline_id: Option<String>,
    processor_id: Option<String>,
    rhi_op: Option<String>,
    intercepted: bool,
    channel: Option<String>,
    source: Option<Source>,
    attrs: BTreeMap<String, serde_json::Value>,
}

impl Capture {
    fn set_well_known(&mut self, name: &str, value: String) -> bool {
        match name {
            "message" => {
                self.message = Some(value);
                true
            }
            "pipeline_id" => {
                self.pipeline_id = Some(value);
                true
            }
            "processor_id" => {
                self.processor_id = Some(value);
                true
            }
            "rhi_op" => {
                self.rhi_op = Some(value);
                true
            }
            "channel" => {
                self.channel = Some(value);
                true
            }
            "source" => {
                // Recognised only for the documented enum values; anything
                // else (stray attribute named `source`) falls back into
                // `attrs` so we don't silently drop it.
                self.source = match value.as_str() {
                    "rust" => Some(Source::Rust),
                    "python" => Some(Source::Python),
                    _ => None,
                };
                self.source.is_some()
            }
            _ => false,
        }
    }
}

impl Visit for Capture {
    fn record_str(&mut self, field: &Field, value: &str) {
        let name = field.name();
        if !self.set_well_known(name, value.to_string()) {
            self.attrs.insert(
                name.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.attrs.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.attrs.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.attrs.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.attrs.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "intercepted" {
            self.intercepted = value;
            return;
        }
        self.attrs
            .insert(field.name().to_string(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        let n = serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null);
        self.attrs.insert(field.name().to_string(), n);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        // `tracing`'s default macro path funnels the formatted message
        // through `record_debug`; extract it via `Debug` and strip the
        // enclosing `"..."` that `Debug` on `String`/`&str` would add.
        let rendered = format!("{:?}", value);
        let value = strip_debug_quotes(rendered);
        let name = field.name();
        if !self.set_well_known(name, value.clone()) {
            self.attrs
                .insert(name.to_string(), serde_json::Value::String(value));
        }
    }
}

fn strip_debug_quotes(s: String) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s.get(1..s.len() - 1).unwrap_or(&s).to_string()
    } else {
        s
    }
}
