// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use streamlib_ipc_types::HelperProcessStepProgressBoardEntry;
use streamlib_runtime_client_contract::local_api_wire_contract::NodeMetricsOnTheWire;

use super::{FunctionNodePromotionOutOfItsReadersIslands, NodeComponentGraphRenders};
use crate::core::json_schema::GraphNodeComponentsOnTheWire;
use crate::iceoryx2::{
    BagCountsByOutputPort, DiscardedSampleCountsByInboundLink, DroppedBagCountsByInboundLink,
    HelperPlacedNodeLossCounts, NodeLossCountSnapshot,
};

#[derive(Default, Clone)]
pub struct NodeMetrics {
    /// Frames per second throughput.
    pub throughput_fps: f64,
    /// 50th percentile latency in milliseconds.
    pub latency_p50_ms: f64,
    /// 99th percentile latency in milliseconds.
    pub latency_p99_ms: f64,
    /// Total frames processed.
    pub frames_processed: u64,
    pub loss_counts: NodeLossCounts,
    /// What moved this function node out of its readers' islands into a
    /// process of its own; `None` for every node never moved.
    pub promotion_out_of_its_readers_islands: Option<FunctionNodePromotionOutOfItsReadersIslands>,
}

#[derive(Clone)]
pub enum NodeLossCounts {
    /// Counted by ports in this process.
    CountedByPortsInThisProcess(LossCountsOfPortsInThisProcess),
    CountedInItsHelperProcess(Arc<HelperPlacedNodeLossCounts>),
}

impl Default for NodeLossCounts {
    fn default() -> Self {
        Self::CountedByPortsInThisProcess(LossCountsOfPortsInThisProcess::default())
    }
}

#[derive(Default, Clone)]
pub struct LossCountsOfPortsInThisProcess {
    pub dropped_bag_counts_by_inbound_link: Arc<DroppedBagCountsByInboundLink>,
    pub discarded_sample_counts_by_inbound_link: Arc<DiscardedSampleCountsByInboundLink>,
    pub bag_counts_by_output_port: Arc<BagCountsByOutputPort>,
}

impl NodeLossCounts {
    /// Every count as it stands right now.
    pub fn loss_count_snapshot(&self) -> NodeLossCountSnapshot {
        match self {
            Self::CountedByPortsInThisProcess(counted_here) => NodeLossCountSnapshot {
                dropped_bags_by_inbound_link: counted_here
                    .dropped_bag_counts_by_inbound_link
                    .dropped_bag_count_snapshot_by_inbound_link(),
                taken_bags_by_inbound_link: counted_here
                    .dropped_bag_counts_by_inbound_link
                    .taken_bag_count_snapshot_by_inbound_link(),
                discarded_samples_by_inbound_link: counted_here
                    .discarded_sample_counts_by_inbound_link
                    .discarded_sample_count_snapshot_by_inbound_link(),
                refused_bags_by_output_port: counted_here
                    .bag_counts_by_output_port
                    .refused_bag_count_snapshot_by_output_port(),
                written_bags_by_output_port: counted_here
                    .bag_counts_by_output_port
                    .written_bag_count_snapshot_by_output_port(),
                given_up_groups_by_output_port: counted_here
                    .bag_counts_by_output_port
                    .given_up_group_count_snapshot_by_output_port(),
            },
            Self::CountedInItsHelperProcess(helper_placed) => helper_placed.loss_count_snapshot(),
        }
    }

    pub fn step_progress_its_helper_process_wrote(
        &self,
    ) -> Option<HelperProcessStepProgressBoardEntry> {
        match self {
            Self::CountedByPortsInThisProcess(_) => None,
            Self::CountedInItsHelperProcess(helper_placed) => {
                helper_placed.step_progress_its_helper_wrote()
            }
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

impl NodeMetrics {
    pub fn total_dropped_bag_count(&self) -> u64 {
        self.loss_counts.total_dropped_bag_count()
    }
}

impl NodeComponentGraphRenders for NodeMetrics {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        // One snapshot for every key: taken apart, an eviction landing between them renders a
        // total smaller than the per-link counts it claims to be the sum of.
        let NodeLossCountSnapshot {
            dropped_bags_by_inbound_link,
            taken_bags_by_inbound_link: _,
            discarded_samples_by_inbound_link,
            refused_bags_by_output_port,
            written_bags_by_output_port,
            given_up_groups_by_output_port,
        } = self.loss_counts.loss_count_snapshot();
        components.metrics = Some(NodeMetricsOnTheWire {
            frames_dropped: dropped_bags_by_inbound_link.values().sum(),
            dropped_bags_by_link: dropped_bags_by_inbound_link,
            refused_bags_by_output_port,
            written_bags_by_output_port,
            discarded_samples_by_link: discarded_samples_by_inbound_link,
            given_up_groups_by_output_port,
            promoted_out_of_its_readers_islands: self
                .promotion_out_of_its_readers_islands
                .as_ref()
                .map(FunctionNodePromotionOutOfItsReadersIslands::rendered),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait RenderedAsGraphAnswersIt {
        /// The node's `metrics`, as `graph` answers it.
        fn rendered_as_graph_answers_it(&self) -> serde_json::Value;
    }

    impl RenderedAsGraphAnswersIt for NodeMetrics {
        fn rendered_as_graph_answers_it(&self) -> serde_json::Value {
            let mut components = GraphNodeComponentsOnTheWire::default();
            self.render_onto(&mut components);
            serde_json::to_value(components.metrics).unwrap()
        }
    }

    fn metrics_counted_here(counted_here: LossCountsOfPortsInThisProcess) -> NodeMetrics {
        NodeMetrics {
            loss_counts: NodeLossCounts::CountedByPortsInThisProcess(counted_here),
            ..Default::default()
        }
    }

    #[test]
    fn a_nodes_metrics_render_every_inbound_links_losses_by_name() {
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
        .rendered_as_graph_answers_it();

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 8,
                "dropped_bags_by_link": { "L-first": 7, "L-second": 1 },
                "refused_bags_by_output_port": {},
                "written_bags_by_output_port": {}
            }),
            "the whole rendering, so no uncomputed field creeps back onto the wire as a zero"
        );
    }

    #[test]
    fn a_nodes_metrics_render_every_output_ports_refusals_beside_its_inbound_losses() {
        let bag_counts = Arc::new(BagCountsByOutputPort::default());
        let video = bag_counts.counters_for_output_port("video");
        let _ = bag_counts.counters_for_output_port("audio");
        video.record_one_refused_bag();
        video.record_one_refused_bag();

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            bag_counts_by_output_port: bag_counts,
            ..Default::default()
        })
        .rendered_as_graph_answers_it();

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": {},
                "refused_bags_by_output_port": { "audio": 0, "video": 2 },
                "written_bags_by_output_port": { "audio": 0, "video": 0 }
            }),
            "refusals stay per output port and never enter the inbound bag total"
        );
    }

    #[test]
    fn a_nodes_metrics_render_every_output_ports_sent_bags_apart_from_its_refusals() {
        let bag_counts = Arc::new(BagCountsByOutputPort::default());
        let video = bag_counts.counters_for_output_port("video");
        let _ = bag_counts.counters_for_output_port("audio");
        for _ in 0..60 {
            video.record_one_written_bag();
        }

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            bag_counts_by_output_port: bag_counts,
            ..Default::default()
        })
        .rendered_as_graph_answers_it();

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": {},
                "refused_bags_by_output_port": { "audio": 0, "video": 0 },
                "written_bags_by_output_port": { "audio": 0, "video": 60 }
            }),
            "a sent bag is counted per output port and is never a loss"
        );
    }

    /// Only an output port whose writer gave up a group upstream renders a
    /// given-up count, beside its refusals, and never in the inbound total.
    #[test]
    fn a_nodes_metrics_render_given_up_groups_only_for_output_ports_that_gave_one_up() {
        let bag_counts = Arc::new(BagCountsByOutputPort::default());
        let track = bag_counts.counters_for_output_port("track");
        let _ = bag_counts.counters_for_output_port("catalog");
        track.record_given_up_groups(3);

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            bag_counts_by_output_port: Arc::clone(&bag_counts),
            ..Default::default()
        })
        .rendered_as_graph_answers_it();
        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": {},
                "refused_bags_by_output_port": { "catalog": 0, "track": 0 },
                "written_bags_by_output_port": { "catalog": 0, "track": 0 },
                "given_up_groups_by_output_port": { "track": 3 }
            }),
        );

        let with_nothing_given_up = metrics_counted_here(LossCountsOfPortsInThisProcess {
            bag_counts_by_output_port: Arc::new(BagCountsByOutputPort::default()),
            ..Default::default()
        })
        .rendered_as_graph_answers_it();
        assert!(
            with_nothing_given_up
                .get("given_up_groups_by_output_port")
                .is_none(),
            "a node that never gave a group up has no count to render; got {with_nothing_given_up}"
        );
    }

    #[test]
    fn a_nodes_metrics_render_discarded_samples_only_for_links_into_windowed_ports() {
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
        .rendered_as_graph_answers_it();
        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": { "L-unwindowed": 0, "L-windowed": 0 },
                "discarded_samples_by_link": { "L-windowed": 480 },
                "refused_bags_by_output_port": {},
                "written_bags_by_output_port": {}
            }),
            "samples stay out of the bag total, and the unwindowed link carries none"
        );

        let with_no_windowed_link = metrics_counted_here(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: dropped,
            ..Default::default()
        })
        .rendered_as_graph_answers_it();
        assert!(
            with_no_windowed_link
                .get("discarded_samples_by_link")
                .is_none(),
            "a node with no windowed link has no sample count to render; got \
             {with_no_windowed_link}"
        );
    }

    #[test]
    fn a_node_that_has_lost_nothing_says_so_rather_than_staying_silent() {
        let counts = Arc::new(DroppedBagCountsByInboundLink::default());
        let _ = counts.counter_for_inbound_link("L-healthy");

        let rendered = metrics_counted_here(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: counts,
            ..Default::default()
        })
        .rendered_as_graph_answers_it();

        assert_eq!(
            rendered,
            serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": { "L-healthy": 0 },
                "refused_bags_by_output_port": {},
                "written_bags_by_output_port": {}
            }),
        );
    }
}
