// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Capture the engine's own `tracing` records inside a helper process, for
//! the helper to hand its parent over the escalate log op.
//!
//! A helper hosts no engine and writes no JSONL of its own: the runtime's log
//! lives in the app process. So the records the engine emits in a child — an
//! input receive failure, a port with no mailbox, anything iceoryx2 says —
//! ride the channel the helper already has to its parent, which stamps and
//! enqueues them into the one unified pipeline.
//!
//! The subscriber installed here feeds a bounded ring and nothing else. A
//! `tracing` event can land on any thread, a garbage-collector finalizer and
//! a `Drop` path included, so the capture side never touches Python: it
//! pushes onto the ring lock-free and a Python thread drains it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};
use crossbeam_queue::ArrayQueue;
use tracing::Dispatch;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;

use crate::core::logging::event::LogLevel;
use crate::core::logging::iceoryx2_log_bridge::install_iceoryx2_log_bridge_at_the_engines_configured_level;
use crate::core::logging::init::the_engines_configured_tracing_filter;
use crate::core::logging::layer::JsonlSinkLayer;
use crate::core::logging::record::LogRecord;
use crate::core::logging::worker::WorkerSignal;

/// How many engine records a helper holds for its parent before the oldest
/// are dropped.
///
/// A helper's records are a diagnostic trickle beside its data plane, and the
/// ring is preallocated per helper process, so this is sized to survive a
/// burst — a storm of receive failures — rather than a sustained flood.
const ENGINE_LOG_RECORDS_A_HELPER_PROCESS_HOLDS_FOR_ITS_PARENT: usize = 4_096;

/// One engine `tracing` record a helper process owes its parent.
///
/// The parent stamps receipt time and owns the runtime id, so neither travels
/// from here; `emitted_at_wall_clock_nanoseconds` is what the child observed
/// when the record was made, and is advisory exactly as the log op's
/// `source_ts` is.
#[derive(Debug, Clone)]
pub struct EngineLogRecordForTheParentProcess {
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    pub pipeline_id: Option<String>,
    pub processor_id: Option<String>,
    pub rhi_op: Option<String>,
    pub attrs: std::collections::BTreeMap<String, serde_json::Value>,
    pub emitted_at_wall_clock_nanoseconds: u64,
}

/// What one drain of the ring found: the records in the order they were
/// emitted, and how many the ring dropped since the drain before it.
#[derive(Debug)]
pub struct EngineLogRecordsDrainedForTheParentProcess {
    pub records: Vec<EngineLogRecordForTheParentProcess>,
    pub records_dropped_since_the_last_drain: u64,
}

/// The bounded ring this helper process's engine records queue in, drained by
/// the thread that forwards them to the parent.
///
/// Drop-oldest: a full ring evicts the record that has waited longest and
/// counts it, so a burst costs the oldest diagnostics rather than blocking
/// the thread that emitted one.
pub struct HelperProcessEngineLogRecordRing {
    queue: Arc<ArrayQueue<LogRecord>>,
    doorbell: Receiver<WorkerSignal>,
    dropped: Arc<AtomicU64>,
    dropped_already_reported: AtomicU64,
}

impl HelperProcessEngineLogRecordRing {
    fn with_capacity(capacity: usize) -> (Self, JsonlSinkLayer) {
        let queue = Arc::new(ArrayQueue::new(capacity));
        let dropped = Arc::new(AtomicU64::new(0));
        let (doorbell_sender, doorbell): (Sender<WorkerSignal>, Receiver<WorkerSignal>) =
            bounded(256);
        let layer = JsonlSinkLayer::new(Arc::clone(&queue), doorbell_sender, Arc::clone(&dropped));
        (
            Self {
                queue,
                doorbell,
                dropped,
                dropped_already_reported: AtomicU64::new(0),
            },
            layer,
        )
    }

    /// Take every record the ring holds, waiting up to `wait` for the first
    /// one.
    ///
    /// Returns as soon as a record arrives; an empty answer means the wait
    /// elapsed with the ring empty, which is what a quiet helper looks like.
    pub fn drain_waiting_at_most(
        &self,
        wait: Duration,
    ) -> EngineLogRecordsDrainedForTheParentProcess {
        let mut records = self.take_every_record_the_ring_holds();
        if records.is_empty() {
            // A doorbell ring can outlive the record that sent it — the
            // drain before this one may have taken it — so the pop after the
            // wait is what decides, not the signal.
            let _ = self.doorbell.recv_timeout(wait);
            records = self.take_every_record_the_ring_holds();
        }
        let dropped = self.dropped.load(Ordering::Relaxed);
        let records_dropped_since_the_last_drain = dropped.saturating_sub(
            self.dropped_already_reported
                .swap(dropped, Ordering::Relaxed),
        );
        EngineLogRecordsDrainedForTheParentProcess {
            records,
            records_dropped_since_the_last_drain,
        }
    }

    fn take_every_record_the_ring_holds(&self) -> Vec<EngineLogRecordForTheParentProcess> {
        let mut records = Vec::new();
        while let Some(record) = self.queue.pop() {
            records.push(EngineLogRecordForTheParentProcess {
                level: record.level,
                target: record.target,
                message: record.message,
                pipeline_id: record.pipeline_id,
                processor_id: record.processor_id,
                rhi_op: record.rhi_op,
                attrs: record.attrs,
                emitted_at_wall_clock_nanoseconds: record.host_ts,
            });
        }
        records
    }
}

/// Install this helper process's engine log capture: a `tracing` subscriber
/// at the engine's configured level feeding a bounded ring, and iceoryx2's
/// log bridge at that same level.
///
/// The caller drains the ring and forwards what it holds; nothing here writes
/// a file, mirrors to stdout, or reaches the parent on its own.
pub fn capture_this_helper_processes_engine_log_records() -> Result<HelperProcessEngineLogRecordRing>
{
    let (ring, layer) = HelperProcessEngineLogRecordRing::with_capacity(
        ENGINE_LOG_RECORDS_A_HELPER_PROCESS_HOLDS_FOR_ITS_PARENT,
    );
    let subscriber = Registry::default()
        .with(the_engines_configured_tracing_filter())
        .with(layer);
    tracing::dispatcher::set_global_default(Dispatch::new(subscriber)).map_err(|already| {
        anyhow::anyhow!(
            "this process already has a tracing subscriber, so the engine's records cannot be \
             captured for its parent: {already}"
        )
    })?;
    // After the subscriber, never before: the level iceoryx2 is set to is the
    // level that subscriber admits.
    install_iceoryx2_log_bridge_at_the_engines_configured_level();
    Ok(ring)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use crate::core::logging::worker::now_ns;

    fn a_record_saying(message: &str) -> LogRecord {
        LogRecord {
            host_ts: now_ns(),
            level: LogLevel::Warn,
            target: "iceoryx2".to_string(),
            message: message.to_string(),
            pipeline_id: None,
            processor_id: None,
            rhi_op: None,
            intercepted: false,
            channel: None,
            attrs: BTreeMap::new(),
            source: None,
            source_ts: None,
            source_seq: None,
        }
    }

    /// The drain hands back what the ring holds, oldest first — the order the
    /// helper emitted them, which is the order the parent's stream shows.
    #[test]
    fn a_drain_hands_back_every_held_record_oldest_first() {
        let (ring, _layer) = HelperProcessEngineLogRecordRing::with_capacity(4);
        for message in ["first", "second", "third"] {
            ring.queue.force_push(a_record_saying(message));
        }

        let drained = ring.drain_waiting_at_most(Duration::ZERO);

        let messages: Vec<&str> = drained
            .records
            .iter()
            .map(|record| record.message.as_str())
            .collect();
        assert_eq!(messages, ["first", "second", "third"]);
        assert_eq!(drained.records_dropped_since_the_last_drain, 0);
    }

    /// A full ring evicts the oldest record and counts it, and the count is
    /// reported once: a helper that lost diagnostics says so, and says it
    /// once per loss rather than on every later drain.
    #[test]
    fn a_full_ring_drops_the_oldest_records_and_reports_each_loss_once() {
        let (ring, layer) = HelperProcessEngineLogRecordRing::with_capacity(2);
        tracing::subscriber::with_default(Registry::default().with(layer), || {
            tracing::error!(target: "iceoryx2", "first");
            tracing::error!(target: "iceoryx2", "second");
            tracing::error!(target: "iceoryx2", "third");
        });

        let drained = ring.drain_waiting_at_most(Duration::ZERO);

        let messages: Vec<&str> = drained
            .records
            .iter()
            .map(|record| record.message.as_str())
            .collect();
        assert_eq!(
            messages,
            ["second", "third"],
            "the oldest record is the one a full ring gives up"
        );
        assert_eq!(drained.records_dropped_since_the_last_drain, 1);
        assert_eq!(
            ring.drain_waiting_at_most(Duration::ZERO)
                .records_dropped_since_the_last_drain,
            0,
            "a loss already reported must not be reported again"
        );
    }

    /// An empty ring returns nothing rather than blocking past its wait, so
    /// the forwarding thread stays responsive to its own shutdown.
    #[test]
    fn an_empty_ring_hands_back_nothing_once_the_wait_elapses() {
        let (ring, _layer) = HelperProcessEngineLogRecordRing::with_capacity(2);

        let drained = ring.drain_waiting_at_most(Duration::from_millis(10));

        assert!(drained.records.is_empty());
        assert_eq!(drained.records_dropped_since_the_last_drain, 0);
    }

    /// A captured event keeps the target, level and fields the JSONL renders,
    /// so a record made in a helper reads in the log exactly as the same call
    /// site reads from the app process.
    #[test]
    fn a_captured_event_keeps_its_target_level_and_fields() {
        let (ring, layer) = HelperProcessEngineLogRecordRing::with_capacity(4);
        tracing::subscriber::with_default(Registry::default().with(layer), || {
            tracing::warn!(
                target: "streamlib_engine::iceoryx2::input",
                port = "frames_from_upstream",
                link = "L-7",
                "InputMailboxes: channel delivered a frame but its bound local port has no mailbox"
            );
        });

        let drained = ring.drain_waiting_at_most(Duration::ZERO);

        let record = drained.records.first().expect("the event was captured");
        assert_eq!(record.level, LogLevel::Warn);
        assert_eq!(record.target, "streamlib_engine::iceoryx2::input");
        assert!(
            record
                .message
                .starts_with("InputMailboxes: channel delivered")
        );
        assert_eq!(
            record.attrs.get("port").and_then(|port| port.as_str()),
            Some("frames_from_upstream")
        );
        assert!(record.emitted_at_wall_clock_nanoseconds > 0);
    }
}
