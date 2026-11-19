// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::compiler_ops::running_nodes_restarted::RunningNodesALiveEditRestarts;
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
    pub(super) config_updates: Vec<(NodeRunId, serde_json::Value)>,
    /// Each running node to stop and build again from the values its inputs hold, once.
    pub(super) nodes_to_rebuild: Vec<NodeRunId>,
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
            && self.config_updates.is_empty()
            && self.nodes_to_rebuild.is_empty()
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

    pub(super) fn leave_unbuilt_each_node_whose_thread_was_abandoned(
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

    pub(super) fn hand_config_updates_to_nodes_this_batch_constructs(&mut self, graph: &mut Graph) {
        let Self {
            nodes_to_add,
            config_updates,
            ..
        } = self;
        if nodes_to_add.is_empty() || config_updates.is_empty() {
            return;
        }
        for (node_run_id, config_to_apply) in
            config_updates.extract_if(.., |(node_run_id, _)| nodes_to_add.contains(node_run_id))
        {
            if let Some(node) = graph.traversal_mut().v(&node_run_id).first_mut() {
                node.set_config(config_to_apply);
            }
        }
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

    #[test]
    fn a_config_update_for_a_node_this_batch_constructs_lands_on_its_node_before_it_is_built() {
        let (mut graph, constructed_in_this_batch, _link_id) = graph_with_one_link();
        let already_running: NodeRunId = "P-already-running".into();

        let mut plan = CompilationPlan {
            nodes_to_add: vec![constructed_in_this_batch.clone()],
            config_updates: vec![
                (
                    constructed_in_this_batch.clone(),
                    serde_json::json!({"gain": 2}),
                ),
                (already_running.clone(), serde_json::json!({"gain": 5})),
                (
                    constructed_in_this_batch.clone(),
                    serde_json::json!({"gain": 3}),
                ),
            ],
            ..Default::default()
        };
        plan.hand_config_updates_to_nodes_this_batch_constructs(&mut graph);

        assert_eq!(
            plan.config_updates,
            vec![(already_running, serde_json::json!({"gain": 5}))],
            "only a node that is already running is asked to reconfigure",
        );
        let config_on_the_node = graph
            .traversal()
            .v(&constructed_in_this_batch)
            .first()
            .expect("the node is in the graph")
            .config
            .clone();
        assert_eq!(config_on_the_node, Some(serde_json::json!({"gain": 3})));
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
