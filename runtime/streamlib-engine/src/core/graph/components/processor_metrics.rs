// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::iceoryx2::{
    DiscardedSampleCountsByInboundLink, DroppedBagCountsByInboundLink,
    HelperPlacedProcessorLossCounts, MeshHopDroppedBagCountsByRemoteInboundLink,
    ProcessorLossCountSnapshot, RefusedBagCountsByOutputPort,
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
    /// What this processor's ports lost, read live from wherever they count it.
    pub loss_counts: ProcessorLossCounts,
    /// What the hop from another runtime lost before this processor's ports saw
    /// anything, counted per remote inbound link.
    ///
    /// Beside `loss_counts` rather than inside it, and not an arm of it: the
    /// ingress that counts this runs in the app process wherever the
    /// destination runs, so a helper-placed destination's hop count reaches
    /// `graph` directly while its ports' own counts still come off its
    /// helper's board.
    pub mesh_hop_dropped_bag_counts_by_remote_inbound_link:
        Arc<MeshHopDroppedBagCountsByRemoteInboundLink>,
}

/// Where a processor's loss counts are counted, and so where `graph` reads them.
#[derive(Clone)]
pub enum ProcessorLossCounts {
    /// Counted by ports in this process.
    CountedByPortsInThisProcess(LossCountsOfPortsInThisProcess),
    /// Counted in the processor's helper process and read off the board it
    /// writes them on.
    CountedInItsHelperProcess(Arc<HelperPlacedProcessorLossCounts>),
}

impl Default for ProcessorLossCounts {
    fn default() -> Self {
        Self::CountedByPortsInThisProcess(LossCountsOfPortsInThisProcess::default())
    }
}

/// The loss counts a processor's ports in this process share with its node.
#[derive(Default, Clone)]
pub struct LossCountsOfPortsInThisProcess {
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

impl ProcessorLossCounts {
    /// Every count as it stands right now.
    pub fn loss_count_snapshot(&self) -> ProcessorLossCountSnapshot {
        match self {
            Self::CountedByPortsInThisProcess(counted_here) => ProcessorLossCountSnapshot {
                dropped_bags_by_inbound_link: counted_here
                    .dropped_bag_counts_by_inbound_link
                    .dropped_bag_count_snapshot_by_inbound_link(),
                discarded_samples_by_inbound_link: counted_here
                    .discarded_sample_counts_by_inbound_link
                    .discarded_sample_count_snapshot_by_inbound_link(),
                refused_bags_by_output_port: counted_here
                    .refused_bag_counts_by_output_port
                    .refused_bag_count_snapshot_by_output_port(),
            },
            Self::CountedInItsHelperProcess(helper_placed) => helper_placed.loss_count_snapshot(),
        }
    }

    /// Every inbound link's dropped bags summed.
    pub fn total_dropped_bag_count(&self) -> u64 {
        match self {
            Self::CountedByPortsInThisProcess(counted_here) => counted_here
                .dropped_bag_counts_by_inbound_link
                .total_dropped_bag_count(),
            Self::CountedInItsHelperProcess(helper_placed) => {
                helper_placed.total_dropped_bag_count()
            }
        }
    }
}

impl ProcessorMetrics {
    /// This processor's dropped bags across every inbound link. Derived from
    /// the per-link counts, which stay the record.
    pub fn total_dropped_bag_count(&self) -> u64 {
        self.loss_counts.total_dropped_bag_count()
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
        // One snapshot for every key: taken apart, an eviction landing between
        // them renders a total smaller than the per-link counts it claims to be
        // the sum of.
        let ProcessorLossCountSnapshot {
            dropped_bags_by_inbound_link,
            discarded_samples_by_inbound_link,
            refused_bags_by_output_port,
        } = self.loss_counts.loss_count_snapshot();
        let mut rendered = serde_json::json!({
            "frames_dropped": dropped_bags_by_inbound_link.values().sum::<u64>(),
            "dropped_bags_by_link": dropped_bags_by_inbound_link,
            "refused_bags_by_output_port": refused_bags_by_output_port,
        });
        // A per-link map with nothing in it renders no key at all rather than
        // an empty object: a port that cannot discard samples and a link that
        // cannot lose a hop are not the same as ones that have not yet. And
        // `frames_dropped` stays exactly the sum of what this processor's own
        // ports lost, since a bag the hop lost never reached one to be dropped
        // at.
        if let Some(rendered_keys) = rendered.as_object_mut() {
            for (key, counts) in [
                (
                    "discarded_samples_by_link",
                    discarded_samples_by_inbound_link,
                ),
                (
                    "mesh_hop_dropped_bags_by_link",
                    self.mesh_hop_dropped_bag_counts_by_remote_inbound_link
                        .mesh_hop_dropped_bag_count_snapshot_by_inbound_link(),
                ),
            ] {
                if !counts.is_empty() {
                    rendered_keys.insert(key.to_string(), serde_json::json!(counts));
                }
            }
        }
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics_counted_here(counted_here: LossCountsOfPortsInThisProcess) -> ProcessorMetrics {
        ProcessorMetrics {
            loss_counts: ProcessorLossCounts::CountedByPortsInThisProcess(counted_here),
            ..Default::default()
        }
    }

    #[test]
    fn a_processors_metrics_render_every_inbound_links_losses_by_name() {
        let counts = Arc::new(DroppedBagCountsByInboundLink::default());
        let from_first_link = counts.counter_for_inbound_link("L-first");
        let from_second_link = counts.counter_for_inbound_link("L-second");
        for _ in 0..7 {
            from_first_link.record_one_dropped_bag();
        }
        from_second_link.record_one_dropped_bag();

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: counts,
            ..Default::default()
        })
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

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            refused_bag_counts_by_output_port: refused,
            ..Default::default()
        })
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

    /// A processor fed across the mesh renders what the hop lost per remote
    /// link, beside what its own ports lost and never blended into it: the two
    /// name different losses at different places, and `frames_dropped` counts
    /// only bags that actually reached a port to be dropped at.
    #[test]
    fn a_processors_metrics_render_mesh_hop_loss_beside_its_ports_own_and_never_inside_it() {
        let dropped = Arc::new(DroppedBagCountsByInboundLink::default());
        dropped
            .counter_for_inbound_link("L-remote")
            .record_dropped_bags(2);
        let hop_loss = Arc::new(MeshHopDroppedBagCountsByRemoteInboundLink::default());
        hop_loss
            .counter_for_inbound_link("L-remote")
            .record_dropped_bags(9);

        let rendered = ProcessorMetrics {
            loss_counts: ProcessorLossCounts::CountedByPortsInThisProcess(
                LossCountsOfPortsInThisProcess {
                    dropped_bag_counts_by_inbound_link: dropped,
                    ..Default::default()
                },
            ),
            mesh_hop_dropped_bag_counts_by_remote_inbound_link: hop_loss,
            ..Default::default()
        }
        .to_json();

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 2,
                "dropped_bags_by_link": { "L-remote": 2 },
                "refused_bags_by_output_port": {},
                "mesh_hop_dropped_bags_by_link": { "L-remote": 9 }
            })
        );
    }

    /// A wired remote link that has lost nothing on the hop says so, rather
    /// than going missing — the rule every other per-link count already keeps.
    #[test]
    fn a_remote_link_that_has_lost_nothing_on_the_hop_renders_a_zero_rather_than_nothing() {
        let hop_loss = Arc::new(MeshHopDroppedBagCountsByRemoteInboundLink::default());
        let _ = hop_loss.counter_for_inbound_link("L-remote");

        let rendered = ProcessorMetrics {
            mesh_hop_dropped_bag_counts_by_remote_inbound_link: hop_loss,
            ..Default::default()
        }
        .to_json();

        assert_eq!(
            rendered["mesh_hop_dropped_bags_by_link"],
            serde_json::json!({ "L-remote": 0 })
        );
    }

    /// A processor fed only from this runtime renders no hop-loss key at all.
    /// A zero there would claim a hop it does not have, and a reader could not
    /// tell it from a remote link that has lost nothing.
    #[test]
    fn a_processor_with_no_remote_link_renders_no_mesh_hop_key_rather_than_an_empty_one() {
        let dropped = Arc::new(DroppedBagCountsByInboundLink::default());
        let _ = dropped.counter_for_inbound_link("L-local");

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: dropped,
            ..Default::default()
        })
        .to_json();

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": { "L-local": 0 },
                "refused_bags_by_output_port": {}
            }),
            "the whole rendering, so no hop-loss key appears where there is no hop"
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

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: Arc::clone(&dropped),
            discarded_sample_counts_by_inbound_link: discarded,
            ..Default::default()
        })
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

        let with_no_windowed_link = metrics_counted_here(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: dropped,
            ..Default::default()
        })
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

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: counts,
            ..Default::default()
        })
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
