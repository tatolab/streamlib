// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::iceoryx2::DroppedBagCountsByInboundLink;

/// Runtime metrics for a processor.
#[derive(Default, Clone)]
pub struct ProcessorMetrics {
    /// Frames per second throughput.
    pub throughput_fps: f64,
    /// 50th percentile latency in milliseconds.
    pub latency_p50_ms: f64,
    /// 99th percentile latency in milliseconds.
    pub latency_p99_ms: f64,
    /// Total frames processed.
    pub frames_processed: u64,
    /// Bags evicted at this processor's input ports, counted per inbound link.
    ///
    /// Shared live with the destination's input mailboxes, so a snapshot reads
    /// the counts as they stand rather than a copy taken at wiring time.
    pub dropped_bag_counts_by_inbound_link: Arc<DroppedBagCountsByInboundLink>,
}

impl ProcessorMetrics {
    /// This processor's dropped bags across every inbound link. Derived from
    /// the per-link counts, which stay the record.
    pub fn total_dropped_bag_count(&self) -> u64 {
        self.dropped_bag_counts_by_inbound_link
            .total_dropped_bag_count()
    }
}

impl JsonSerializableComponent for ProcessorMetrics {
    fn json_key(&self) -> &'static str {
        "metrics"
    }

    fn to_json(&self) -> JsonValue {
        // One snapshot for both keys: taken twice, an eviction landing between
        // them renders a total smaller than the per-link counts it claims to be
        // the sum of.
        let by_inbound_link = self
            .dropped_bag_counts_by_inbound_link
            .dropped_bag_count_snapshot_by_inbound_link();
        serde_json::json!({
            "throughput_fps": self.throughput_fps,
            "latency_p50_ms": self.latency_p50_ms,
            "latency_p99_ms": self.latency_p99_ms,
            "frames_processed": self.frames_processed,
            "frames_dropped": by_inbound_link.values().sum::<u64>(),
            "dropped_bags_by_link": by_inbound_link
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_processors_metrics_render_every_inbound_links_losses_by_name() {
        let counts = Arc::new(DroppedBagCountsByInboundLink::default());
        let from_first_link = counts.counter_for_inbound_link("L-first");
        let from_second_link = counts.counter_for_inbound_link("L-second");
        for _ in 0..7 {
            from_first_link.record_one_dropped_bag();
        }
        from_second_link.record_one_dropped_bag();

        let rendered = ProcessorMetrics {
            dropped_bag_counts_by_inbound_link: counts,
            ..Default::default()
        }
        .to_json();

        assert_eq!(rendered["dropped_bags_by_link"]["L-first"], 7);
        assert_eq!(rendered["dropped_bags_by_link"]["L-second"], 1);
        assert_eq!(
            rendered["frames_dropped"], 8,
            "the total must be derived from the per-link counts, never counted apart from them"
        );
    }

    #[test]
    fn a_processor_that_has_lost_nothing_says_so_rather_than_staying_silent() {
        let counts = Arc::new(DroppedBagCountsByInboundLink::default());
        let _ = counts.counter_for_inbound_link("L-healthy");

        let rendered = ProcessorMetrics {
            dropped_bag_counts_by_inbound_link: counts,
            ..Default::default()
        }
        .to_json();

        assert_eq!(rendered["dropped_bags_by_link"]["L-healthy"], 0);
        assert_eq!(rendered["frames_dropped"], 0);
    }
}
