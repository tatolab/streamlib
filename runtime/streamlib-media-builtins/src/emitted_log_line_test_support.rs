// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Counting what a built-in actually said, for the tests whose claim is about
//! how often it said it.
//!
//! Shared because more than one built-in makes that kind of claim: an output
//! nobody linked must not report every block, and a device that stopped must
//! be reported once rather than every turn of a loop. Both are assertions about
//! the count of lines, which no assertion about behaviour can stand in for.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// How many lines were emitted at each level a built-in uses, so a test can
/// hold the log rate itself rather than a predicate the log site is free to
/// ignore — and can tell the two failure paths apart.
#[derive(Default)]
pub(crate) struct EmittedLines {
    pub(crate) warnings: AtomicU64,
    pub(crate) errors: AtomicU64,
}

/// A subscriber that counts lines and keeps none, installed for the length of
/// one `tracing::subscriber::with_default` block.
pub(crate) struct CountingTracingSubscriber(pub(crate) Arc<EmittedLines>);

impl tracing::Subscriber for CountingTracingSubscriber {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let counter = match *event.metadata().level() {
            tracing::Level::ERROR => &self.0.errors,
            _ => &self.0.warnings,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
