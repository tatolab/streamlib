// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Forwarding a module's own `tracing` records to its node's log.
//!
//! A module's cdylib carries its own copy of `tracing`, which no subscriber in
//! a helper process listens to; this installs one that hands each record to the
//! node host's `log` verb.

use std::fmt::Write as _;
use std::sync::{Once, RwLock};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::module_host::{ModuleHost, ModuleHostLogLevel};

static MODULE_HOST_RECEIVING_THIS_MODULES_TRACING_RECORDS: RwLock<Option<ModuleHost>> =
    RwLock::new(None);

static THIS_MODULES_TRACING_SUBSCRIBER_INSTALLED: Once = Once::new();

/// Send every `tracing` record this module makes from here on to
/// `module_host`'s log, replacing the node host an earlier call named.
///
/// Installs this module's global subscriber on the first call; a module whose
/// `tracing` already has a global subscriber keeps it, and nothing is forwarded.
pub fn forward_this_modules_tracing_records_to(module_host: &ModuleHost) {
    if let Ok(mut receiving) = MODULE_HOST_RECEIVING_THIS_MODULES_TRACING_RECORDS.write() {
        *receiving = Some(module_host.clone());
    }
    THIS_MODULES_TRACING_SUBSCRIBER_INSTALLED.call_once(|| {
        let _ = tracing::subscriber::set_global_default(ModuleHostTracingSubscriber);
    });
}

/// Stop forwarding, releasing the node host the forwarding held.
pub fn stop_forwarding_this_modules_tracing_records() {
    if let Ok(mut receiving) = MODULE_HOST_RECEIVING_THIS_MODULES_TRACING_RECORDS.write() {
        *receiving = None;
    }
}

struct ModuleHostTracingSubscriber;

impl Subscriber for ModuleHostTracingSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::DEBUG
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::DEBUG)
    }

    fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let Ok(receiving) = MODULE_HOST_RECEIVING_THIS_MODULES_TRACING_RECORDS.read() else {
            return;
        };
        let Some(module_host) = receiving.as_ref() else {
            return;
        };
        let mut message = RecordMessageWithItsFields::default();
        event.record(&mut message);
        let metadata = event.metadata();
        module_host.log(
            module_host_log_level_of(*metadata.level()),
            metadata.target(),
            &message.text,
        );
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

fn module_host_log_level_of(level: Level) -> ModuleHostLogLevel {
    match level {
        Level::ERROR => ModuleHostLogLevel::Error,
        Level::WARN => ModuleHostLogLevel::Warn,
        Level::INFO => ModuleHostLogLevel::Info,
        Level::DEBUG => ModuleHostLogLevel::Debug,
        Level::TRACE => ModuleHostLogLevel::Trace,
    }
}

/// A record's `message` first, then each other field as ` name=value`.
#[derive(Default)]
struct RecordMessageWithItsFields {
    text: String,
    fields: String,
}

impl Visit for RecordMessageWithItsFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.text, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
        self.join_fields();
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.text.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
        self.join_fields();
    }
}

impl RecordMessageWithItsFields {
    fn join_fields(&mut self) {
        if !self.fields.is_empty() {
            let fields = std::mem::take(&mut self.fields);
            self.text.push_str(&fields);
        }
    }
}
