// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tracing` layer that captures events and pushes [`LogRecord`]s onto
//! the drain worker's bounded queue. Hot path: no fd writes, no
//! formatting beyond `Debug` on the message field, and at most one
//! allocation per captured field value. All fan-out work (JSON
//! serialization, file I/O, stdout write) happens on the drain worker
//! thread.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::Sender;
use crossbeam_queue::ArrayQueue;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::core::logging::event::{LogLevel, Source};
use crate::core::logging::record::LogRecord;
use crate::core::logging::worker::{now_ns, WorkerSignal};

pub(crate) struct JsonlSinkLayer {
    queue: Arc<ArrayQueue<LogRecord>>,
    doorbell: Sender<WorkerSignal>,
    dropped: Arc<AtomicU64>,
}

impl JsonlSinkLayer {
    pub(crate) fn new(
        queue: Arc<ArrayQueue<LogRecord>>,
        doorbell: Sender<WorkerSignal>,
        dropped: Arc<AtomicU64>,
    ) -> Self {
        Self {
            queue,
            doorbell,
            dropped,
        }
    }

    fn enqueue(&self, record: LogRecord) {
        // Drop-oldest when the queue is full. `force_push` returns the
        // evicted record (if any); we count that as one drop.
        if self.queue.force_push(record).is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        let _ = self.doorbell.try_send(WorkerSignal::Record);
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
        };

        self.enqueue(record);
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
                    "deno" => Some(Source::Deno),
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
            self.attrs
                .insert(name.to_string(), serde_json::Value::String(value.to_string()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.attrs
            .insert(field.name().to_string(), serde_json::Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.attrs
            .insert(field.name().to_string(), serde_json::Value::Number(value.into()));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.attrs
            .insert(field.name().to_string(), serde_json::Value::String(value.to_string()));
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.attrs
            .insert(field.name().to_string(), serde_json::Value::String(value.to_string()));
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
