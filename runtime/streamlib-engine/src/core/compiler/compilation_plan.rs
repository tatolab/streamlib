// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{Graph, LinkUniqueId, ProcessorUniqueId};

/// Categorized operations ready for execution.
///
/// Built from [`PendingOperation`]s after validation and dependency analysis.
#[derive(Debug, Default)]
pub(super) struct CompilationPlan {
    pub(super) processors_to_add: Vec<ProcessorUniqueId>,
    pub(super) processors_to_remove: Vec<ProcessorUniqueId>,
    pub(super) links_to_add: Vec<LinkUniqueId>,
    pub(super) links_to_remove: Vec<LinkUniqueId>,
    pub(super) config_updates: Vec<ProcessorUniqueId>,
}

impl CompilationPlan {
    /// Returns true if there are no operations to execute.
    pub(super) fn is_empty(&self) -> bool {
        self.processors_to_add.is_empty()
            && self.processors_to_remove.is_empty()
            && self.links_to_add.is_empty()
            && self.links_to_remove.is_empty()
            && self.config_updates.is_empty()
    }

    /// Drop any queued link wiring whose endpoint processor this same batch
    /// removes. petgraph's `remove_node` cascades a node's incident edges, so a
    /// link scheduled to wire against a processor that this commit also tears
    /// down no longer exists by the WIRE phase — wiring it would raise a
    /// spurious [`Error::LinkNotFound`]. Disconnect-then-remove and
    /// connect-then-immediate-stop are legitimate live-graph edits; reconciling
    /// the plan keeps processor removal idempotent to the links its own
    /// cascade removes.
    ///
    /// [`Error::LinkNotFound`]: crate::core::error::Error::LinkNotFound
    pub(super) fn drop_link_adds_into_removed_processors(&mut self, graph: &Graph) {
        let Self {
            links_to_add,
            processors_to_remove,
            ..
        } = self;
        if processors_to_remove.is_empty() || links_to_add.is_empty() {
            return;
        }
        links_to_add.retain(|link_id| {
            let Some(link) = graph.traversal().e(link_id).first() else {
                tracing::debug!("[commit] link-add {link_id} skipped — link absent from graph");
                return false;
            };
            let source_removed = processors_to_remove
                .iter()
                .any(|removed| removed == &link.from_port().processor_id);
            let target_removed = processors_to_remove
                .iter()
                .any(|removed| removed == &link.to_port().processor_id);
            if source_removed || target_removed {
                tracing::debug!(
                    "[commit] link-add {link_id} skipped — endpoint processor removed in same batch"
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
    use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};

    /// Build `source_output → target_input` between two live mock processors
    /// and return `(source_id, link_id)`.
    fn graph_with_one_link() -> (Graph, ProcessorUniqueId, LinkUniqueId) {
        crate::core::test_support::ensure_test_mocks_registered();
        let ident = |short: &str| {
            PROCESSOR_REGISTRY
                .list_registered()
                .into_iter()
                .find(|descriptor| descriptor.name.r#type.as_str() == short)
                .map(|descriptor| descriptor.name)
                .expect("mock processor must be registered")
        };

        let mut graph = Graph::new();
        let source_id = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                ident("TestMockOutputOnlyProcessor"),
                serde_json::Value::Null,
            ))
            .first()
            .expect("source node added")
            .id
            .clone();
        let target_id = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                ident("TestMockInputOnlyProcessor"),
                serde_json::Value::Null,
            ))
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

    /// A link scheduled for wiring whose source processor the same batch
    /// removes is dropped from `links_to_add`: `remove_node` will cascade the
    /// edge away, so the WIRE phase would otherwise `LinkNotFound`. Mentally
    /// revert `drop_link_adds_into_removed_processors` to a no-op and the link
    /// survives — reproducing the shutdown-time crash the dynamic-reconfigure
    /// example hit (connect-then-immediate-stop leaves a doomed link add that
    /// runtime teardown removes the endpoints of in the same commit).
    #[test]
    fn drops_link_add_when_its_endpoint_is_removed_in_the_same_batch() {
        let (graph, source_id, link_id) = graph_with_one_link();

        let mut plan = CompilationPlan {
            links_to_add: vec![link_id],
            processors_to_remove: vec![source_id],
            ..Default::default()
        };
        plan.drop_link_adds_into_removed_processors(&graph);

        assert!(
            plan.links_to_add.is_empty(),
            "a link whose endpoint processor is removed in the same batch must not \
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
        plan.drop_link_adds_into_removed_processors(&graph);

        assert_eq!(
            plan.links_to_add,
            vec![link_id],
            "a link with no removed endpoint must remain queued for wiring",
        );
    }
}
