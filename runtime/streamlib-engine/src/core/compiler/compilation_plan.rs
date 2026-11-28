// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::compiler_ops::running_node_restart::RunningNodesALiveEditRestarts;
use crate::core::graph::{Graph, LinkUniqueId, NodeRunId, WhyTheNodeLeaves};

/// Categorized operations ready for execution.
///
/// Built from [`PendingOperation`]s after validation and dependency analysis.
#[derive(Debug, Default)]
pub(super) struct CompilationPlan {
    pub(super) nodes_to_add: Vec<NodeRunId>,
    /// Each node to remove, once, beside why it leaves the graph.
    pub(super) nodes_to_remove: Vec<(NodeRunId, WhyTheNodeLeaves)>,
    pub(super) links_to_add: Vec<LinkUniqueId>,
    pub(super) links_to_remove: Vec<LinkUniqueId>,
    /// Each node whose type choices are judged again against the links they reach, once.
    pub(super) nodes_whose_type_choices_are_judged_again: Vec<NodeRunId>,
    /// What judging again the links those type choices reach found, marked on the graph only
    /// once the compile begins changing what runs.
    pub(super) links_judged_again: crate::core::graph::LinksATypeChoiceJudgedAgain,
    /// Each wired link whose two ends no longer carry types that connect, to unwire and keep in
    /// the graph, carrying nothing.
    pub(super) links_to_hold_unwired: Vec<LinkUniqueId>,
    /// Each link of `links_to_add` an end out of process refused to open, to unwire, keeping it
    /// in the graph, before it is wired again.
    pub(super) links_to_wire_again: Vec<LinkUniqueId>,
    /// Each function node to promote out of the islands of the reader whose
    /// helper it crashed, beside that reader and why.
    pub(super) function_node_promotions:
        Vec<FunctionNodePromotionOutOfTheIslandsOfTheReaderItCrashed>,
}

/// One function node to promote out of the islands of the reader whose helper
/// it crashed.
#[derive(Debug, Clone)]
pub(super) struct FunctionNodePromotionOutOfTheIslandsOfTheReaderItCrashed {
    pub(super) function_node_id: NodeRunId,
    pub(super) reader_id: NodeRunId,
    pub(super) promotion_request_number: u64,
    pub(super) reason: String,
}

impl CompilationPlan {
    /// Returns true if there are no operations to execute.
    pub(super) fn is_empty(&self) -> bool {
        self.nodes_to_add.is_empty()
            && self.nodes_to_remove.is_empty()
            && self.links_to_add.is_empty()
            && self.links_to_remove.is_empty()
            && self.links_to_hold_unwired.is_empty()
            && self.links_judged_again.differing.is_empty()
            && self.function_node_promotions.is_empty()
    }

    /// Queue a node to leave the graph. A node queued twice leaves once, and removed when either
    /// queueing removes it.
    pub(super) fn remove_the_node(&mut self, node_run_id: NodeRunId, why: WhyTheNodeLeaves) {
        match self
            .nodes_to_remove
            .iter_mut()
            .find(|(queued, _)| *queued == node_run_id)
        {
            Some((_, queued_why)) => {
                if why == WhyTheNodeLeaves::Removed {
                    *queued_why = WhyTheNodeLeaves::Removed;
                }
            }
            None => self.nodes_to_remove.push((node_run_id, why)),
        }
    }

    /// The node of each entry of `nodes_to_remove`.
    pub(super) fn node_ids_to_remove(&self) -> Vec<NodeRunId> {
        self.nodes_to_remove
            .iter()
            .map(|(node_run_id, _)| node_run_id.clone())
            .collect()
    }

    /// Queue each running node a live edit restarts to be built again, and each
    /// of its links to be wired again.
    pub(super) fn add_each_node_and_link_restarted(
        &mut self,
        restarted: &RunningNodesALiveEditRestarts,
    ) {
        for node_id in &restarted.nodes {
            if !self.nodes_to_add.contains(node_id) {
                self.nodes_to_add.push(node_id.clone());
            }
        }
        for link_id in &restarted.links {
            if !self.links_to_add.contains(link_id) {
                self.links_to_add.push(link_id.clone());
            }
        }
    }

    /// Leave each of `node_ids` unbuilt, and each link into or out of one unwired.
    pub(super) fn leave_unbuilt_each_node_and_unwired_each_of_its_links(
        &mut self,
        graph: &Graph,
        node_ids: &[NodeRunId],
    ) {
        if node_ids.is_empty() {
            return;
        }
        self.nodes_to_add
            .retain(|node_run_id| !node_ids.contains(node_run_id));
        self.links_to_add.retain(|link_id| {
            graph.traversal().e(link_id).first().is_none_or(|link| {
                !node_ids.contains(link.source().node_run_id())
                    && !node_ids.contains(link.to_port().node_run_id())
            })
        });
    }

    pub(super) fn drop_link_adds_into_removed_nodes(&mut self, graph: &Graph) {
        let Self {
            links_to_add,
            nodes_to_remove,
            ..
        } = self;
        if nodes_to_remove.is_empty() || links_to_add.is_empty() {
            return;
        }
        links_to_add.retain(|link_id| {
            let Some(link) = graph.traversal().e(link_id).first() else {
                tracing::debug!("[commit] link-add {link_id} skipped — link absent from graph");
                return false;
            };
            let removed = |node_run_id: &NodeRunId| {
                nodes_to_remove
                    .iter()
                    .any(|(queued, _)| queued == node_run_id)
            };
            let source_removed = removed(link.source().node_run_id());
            let target_removed = removed(link.to_port().node_run_id());
            if source_removed || target_removed {
                tracing::debug!(
                    "[commit] link-add {link_id} skipped — endpoint node removed in same batch"
                );
                return false;
            }
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::nodes::NodeSpec;

    fn graph_with_one_link() -> (Graph, NodeRunId, LinkUniqueId) {
        crate::core::test_support::ensure_test_mocks_registered();

        let mut graph = Graph::new();
        let source_id = graph
            .traversal_mut()
            .add_v(NodeSpec::new(
                crate::core::test_support::MockOutputOnlyNode::node_class_import_path(),
                serde_json::Value::Null,
            ))
            .expect("the node is named")
            .first()
            .expect("source node added")
            .id
            .clone();
        let target_id = graph
            .traversal_mut()
            .add_v(NodeSpec::new(
                crate::core::test_support::MockInputOnlyNode::node_class_import_path(),
                serde_json::Value::Null,
            ))
            .expect("the node is named")
            .first()
            .expect("target node added")
            .id
            .clone();
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&source_id, "out1"),
            InputLinkPortRef::new(&target_id, "in1"),
        );
        let link_id = graph
            .traversal()
            .e(())
            .first()
            .expect("link added")
            .id
            .clone();
        (graph, source_id, link_id)
    }

    #[test]
    fn drops_link_add_when_its_endpoint_is_removed_in_the_same_batch() {
        let (graph, source_id, link_id) = graph_with_one_link();

        let mut plan = CompilationPlan {
            links_to_add: vec![link_id],
            nodes_to_remove: vec![(source_id, WhyTheNodeLeaves::Removed)],
            ..Default::default()
        };
        plan.drop_link_adds_into_removed_nodes(&graph);

        assert!(
            plan.links_to_add.is_empty(),
            "a link whose endpoint node is removed in the same batch must not \
             stay queued for the WIRE phase",
        );
    }

    /// A link whose endpoints both survive the batch stays queued — the
    /// reconciliation only drops links the removal cascade invalidates, never
    /// a live wiring.
    #[test]
    fn keeps_link_add_when_no_endpoint_is_removed() {
        let (graph, _source_id, link_id) = graph_with_one_link();

        let mut plan = CompilationPlan {
            links_to_add: vec![link_id.clone()],
            ..Default::default()
        };
        plan.drop_link_adds_into_removed_nodes(&graph);

        assert_eq!(
            plan.links_to_add,
            vec![link_id],
            "a link with no removed endpoint must remain queued for wiring",
        );
    }
}
