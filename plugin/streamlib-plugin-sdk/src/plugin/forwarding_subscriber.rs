// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-side `tracing::Subscriber` that forwards every event to the
//! host via [`crate::plugin::HostCallbacks::tracing_emit`].
//!
//! Rather than passing the host's `Dispatch` across the plugin ABI
//! (which doesn't reach the host's subscriber chain), the cdylib installs
//! a thin `Subscriber` impl whose `event` method serializes the event's
//! target / level / message / fields into primitive payloads and calls
//! the host's `tracing_emit` fn pointer. The host's installed subscriber
//! chain sees the event as a normal in-process emit and filters /
//! records / fan-outs accordingly.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::field::{Field, Visit};
use tracing::span;
use tracing::{Event, Metadata, Subscriber};

use super::{host_callbacks, host_interest_to_tracing, tracing_level_to_host};

/// Forwarding subscriber installed in cdylib-linked plugins at
/// `install_host_services` time.
pub struct ForwardingSubscriber {
    /// Monotonic span-id source. Spans aren't bridged today; the id just
    /// needs to be non-zero per tracing-core's contract.
    next_span_id: AtomicU64,
}

impl ForwardingSubscriber {
    fn new() -> Self {
        Self {
            next_span_id: AtomicU64::new(1),
        }
    }
}

impl Subscriber for ForwardingSubscriber {
    fn register_callsite(
        &self,
        metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        let Some(cbs) = host_callbacks() else {
            return tracing::subscriber::Interest::never();
        };
        let target = metadata.target();
        let level = tracing_level_to_host(*metadata.level());
        let host_interest = unsafe {
            (cbs.tracing_register_callsite)(cbs.host, target.as_ptr(), target.len(), level)
        };
        host_interest_to_tracing(host_interest)
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let Some(cbs) = host_callbacks() else {
            return false;
        };
        let target = metadata.target();
        let level = tracing_level_to_host(*metadata.level());
        unsafe { (cbs.tracing_enabled)(cbs.host, target.as_ptr(), target.len(), level) }
    }

    fn new_span(&self, _attrs: &span::Attributes<'_>) -> span::Id {
        let id = self.next_span_id.fetch_add(1, Ordering::Relaxed);
        // span::Id requires nonzero u64.
        span::Id::from_u64(if id == 0 { 1 } else { id })
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let Some(cbs) = host_callbacks() else { return };
        let metadata = event.metadata();
        let target = metadata.target();
        let level = tracing_level_to_host(*metadata.level());

        // Walk the event's value set: pull `message` out as the primary
        // string payload; everything else folds into a BTreeMap that we
        // msgpack-encode and ship in the same call.
        let mut visitor = ForwardingVisitor::default();
        event.record(&mut visitor);

        let message = visitor.message.unwrap_or_default();
        let fields_bytes = if visitor.fields.is_empty() {
            Vec::new()
        } else {
            rmp_serde::to_vec_named(&visitor.fields).unwrap_or_default()
        };

        unsafe {
            (cbs.tracing_emit)(
                cbs.host,
                target.as_ptr(),
                target.len(),
                level,
                message.as_ptr(),
                message.len(),
                fields_bytes.as_ptr(),
                fields_bytes.len(),
            );
        }
    }

    fn enter(&self, _span: &span::Id) {}
    fn exit(&self, _span: &span::Id) {}
}

#[derive(Default)]
struct ForwardingVisitor {
    message: Option<String>,
    fields: BTreeMap<String, serde_json::Value>,
}

impl Visit for ForwardingVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        let name = field.name();
        if name == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields
                .insert(name.to_string(), serde_json::Value::String(value.to_string()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::Number(value.into()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        let n = serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null);
        self.fields.insert(field.name().to_string(), n);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{:?}", value);
        let stripped = strip_debug_quotes(rendered);
        let name = field.name();
        if name == "message" {
            self.message = Some(stripped);
        } else {
            self.fields
                .insert(name.to_string(), serde_json::Value::String(stripped));
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

/// Install the forwarding subscriber as this plugin's global tracing
/// dispatcher. Called by `install_host_services` after the callback table
/// is cached. The cdylib's `set_global_default` succeeds on first call;
/// subsequent calls are silent no-ops.
pub fn install_for_self() {
    let subscriber = ForwardingSubscriber::new();
    let _ = tracing::dispatcher::set_global_default(tracing::Dispatch::new(subscriber));
}
