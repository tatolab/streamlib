// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::iceoryx2::{
    DiscardedSampleCountsByInboundLink, DroppedBagCountsByInboundLink, RefusedBagCountsByOutputPort,
};

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
    /// Bags lost on the way to this processor's input ports, counted per
    /// inbound link.
    ///
    /// Shared live with the destination's input mailboxes, so a snapshot reads
    /// the counts as they stand rather than a copy taken at wiring time.
    pub dropped_bag_counts_by_inbound_link: Arc<DroppedBagCountsByInboundLink>,
    /// Samples this processor's windowed input ports discarded when they
    /// flushed, counted per inbound link and shared live with its input
    /// mailboxes.
    pub discarded_sample_counts_by_inbound_link: Arc<DiscardedSampleCountsByInboundLink>,
    /// Bags this processor wrote that its output ports refused at the channel
    /// ceiling, counted per output port and shared live with its output writer.
    pub refused_bag_counts_by_output_port: Arc<RefusedBagCountsByOutputPort>,
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

    /// Only the fields something actually computes reach the wire. Nothing
    /// writes `throughput_fps`, the two latencies or `frames_processed`, and
    /// this component had no insert site at all until drop counts gave it one —
    /// so rendering their zeros would put four permanent false claims on
    /// `graph`'s first-ever `metrics` key. A reader could not tell an idle
    /// processor from an uninstrumented one.
    fn to_json(&self) -> JsonValue {
        // One snapshot for both keys: taken twice, an eviction landing between
        // them renders a total smaller than the per-link counts it claims to be
        // the sum of.
        let by_inbound_link = self
            .dropped_bag_counts_by_inbound_link
            .dropped_bag_count_snapshot_by_inbound_link();
        let mut rendered = serde_json::json!({
            "frames_dropped": by_inbound_link.values().sum::<u64>(),
            "dropped_bags_by_link": by_inbound_link,
            "refused_bags_by_output_port": self
                .refused_bag_counts_by_output_port
                .refused_bag_count_snapshot_by_output_port()
        });
        let discarded_samples_by_link = self
            .discarded_sample_counts_by_inbound_link
            .discarded_sample_count_snapshot_by_inbound_link();
        if !discarded_samples_by_link.is_empty()
            && let Some(rendered_keys) = rendered.as_object_mut()
        {
            rendered_keys.insert(
                "discarded_samples_by_link".to_string(),
                serde_json::json!(discarded_samples_by_link),
            );
        }
        rendered
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

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 8,
                "dropped_bags_by_link": { "L-first": 7, "L-second": 1 },
                "refused_bags_by_output_port": {}
            }),
            "the whole rendering, so no uncomputed field creeps back onto the wire as a zero"
        );
    }

    #[test]
    fn a_processors_metrics_render_every_output_ports_refusals_beside_its_inbound_losses() {
        let refused = Arc::new(RefusedBagCountsByOutputPort::default());
        let video = refused.counter_for_output_port("video");
        let _ = refused.counter_for_output_port("audio");
        video.record_one_refused_bag();
        video.record_one_refused_bag();

        let rendered = ProcessorMetrics {
            refused_bag_counts_by_output_port: refused,
            ..Default::default()
        }
        .to_json();

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": {},
                "refused_bags_by_output_port": { "audio": 0, "video": 2 }
            }),
            "refusals stay per output port and never enter the inbound bag total"
        );
    }

    /// Only a link into a windowed port carries a sample count, and a processor
    /// with no such link renders no key for one rather than an empty map.
    #[test]
    fn a_processors_metrics_render_discarded_samples_only_for_links_into_windowed_ports() {
        let dropped = Arc::new(DroppedBagCountsByInboundLink::default());
        let _ = dropped.counter_for_inbound_link("L-windowed");
        let _ = dropped.counter_for_inbound_link("L-unwindowed");
        let discarded = Arc::new(DiscardedSampleCountsByInboundLink::default());
        discarded
            .counter_for_inbound_link("L-windowed")
            .record_discarded_samples(480);

        let rendered = ProcessorMetrics {
            dropped_bag_counts_by_inbound_link: Arc::clone(&dropped),
            discarded_sample_counts_by_inbound_link: discarded,
            ..Default::default()
        }
        .to_json();
        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": { "L-unwindowed": 0, "L-windowed": 0 },
                "discarded_samples_by_link": { "L-windowed": 480 },
                "refused_bags_by_output_port": {}
            }),
            "samples stay out of the bag total, and the unwindowed link carries none"
        );

        let with_no_windowed_link = ProcessorMetrics {
            dropped_bag_counts_by_inbound_link: dropped,
            ..Default::default()
        }
        .to_json();
        assert!(
            with_no_windowed_link
                .get("discarded_samples_by_link")
                .is_none(),
            "a processor with no windowed link has no sample count to render; got \
             {with_no_windowed_link}"
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

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": { "L-healthy": 0 },
                "refused_bags_by_output_port": {}
            }),
        );
    }
}
