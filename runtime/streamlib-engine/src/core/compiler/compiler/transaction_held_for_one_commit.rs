// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The compiler's transaction held for one commit: every operation logged while it is held is
//! compiled in one commit once it is released, or taken back with what it did to the graph.

use super::Compiler;
use crate::core::compiler::PendingOperation;
use crate::core::compiler::compiler_ops::shader_node_island::{
    ShaderNodeFusedIntoTheIslandOfAnotherComponent, ShaderNodeIslandHostedHereComponent,
};
use crate::core::graph::{
    FunctionNodeRanCompiledComponent, GraphEdgeWithComponents, GraphNode, GraphNodeWithComponents,
    LinkState, LinkStateComponent, LinkUniqueId, NodeInstanceComponent, PendingDeletionComponent,
    ShaderNodeFusedGroupComponent, WhyTheNodeLeaves,
};

/// Whether a commit built `node`: an instance of its own, or a place in an island another node
/// runs, a function node compiled into its helpers or a shader node fused into a host's island.
fn a_commit_built(node: &GraphNode) -> bool {
    node.has::<NodeInstanceComponent>()
        || node.has::<FunctionNodeRanCompiledComponent>()
        || node.has::<ShaderNodeIslandHostedHereComponent>()
        || node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>()
        || node.has::<ShaderNodeFusedGroupComponent>()
}

impl Compiler {
    /// Hold the transaction for one commit: until it is released, a commit asked for compiles
    /// nothing, so every operation logged meanwhile is compiled in one commit, judged together
    /// by the graph they leave.
    ///
    /// Held in the turn a commit holds, so a commit in flight either took what was logged
    /// before the hold or takes nothing logged under it.
    pub(crate) fn hold_the_transaction_for_one_commit(&self) {
        let _one_commit_at_a_time = self.one_commit_at_a_time.lock();
        *self.transaction_held_for_one_commit.lock() = Some(self.transaction.lock().len());
    }

    /// Whether the transaction is held for one commit, so what is pending deletion leaves only
    /// as it closes.
    pub(crate) fn the_transaction_is_held_for_one_commit(&self) -> bool {
        self.transaction_held_for_one_commit.lock().is_some()
    }

    /// The operations the transaction logged under its hold for one commit, which the commit
    /// that releases it compiles; `None` once the hold is gone, taken back by a stop its
    /// stream's forced shutdown would not wait for.
    pub(crate) fn the_operations_logged_under_the_hold_for_one_commit(
        &self,
    ) -> Option<Vec<PendingOperation>> {
        let logged_before = (*self.transaction_held_for_one_commit.lock())?;
        let transaction = self.transaction.lock();
        Some(transaction[logged_before.min(transaction.len())..].to_vec())
    }

    /// Release the transaction's hold for one commit, for a stream no commit builds, which
    /// compiles nothing of what was logged under it.
    pub(crate) fn release_the_transaction_held_for_one_commit(&self) {
        self.transaction_held_for_one_commit.lock().take();
    }

    /// Take back every operation the transaction logged under its hold for one commit, and
    /// what each did to the graph, as [`Self::take_back_what_operations_did_to_the_graph`] does,
    /// then release the hold; whether there was a hold to take back. A promotion a crash asked
    /// for meanwhile stays logged.
    pub(crate) fn take_back_the_transaction_held_for_one_commit(&self) -> bool {
        let Some(logged_before) = self.transaction_held_for_one_commit.lock().take() else {
            return false;
        };
        let logged_under_the_hold: Vec<PendingOperation> = {
            let mut transaction = self.transaction.lock();
            let logged_before = logged_before.min(transaction.len());
            transaction
                .extract_if(logged_before.., |operation| {
                    !matches!(
                        operation,
                        PendingOperation::PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed { .. }
                    )
                })
                .collect()
        };
        self.take_back_what_operations_did_to_the_graph(&logged_under_the_hold);
        true
    }

    /// Take back what each of `operations` did to the graph that is still there to take back:
    /// a node or link it added and nothing built leaves the graph, and a node or link it marked
    /// for removal is kept. A node built, or a link wired, stays.
    pub(crate) fn take_back_what_operations_did_to_the_graph(
        &self,
        operations: &[PendingOperation],
    ) {
        let mut graph = self.graph.write();
        for operation in operations.iter().rev() {
            match operation {
                PendingOperation::AddLink(link_id) => {
                    let unwired = graph.traversal().e(link_id).first().is_some_and(|link| {
                        link.get::<LinkStateComponent>()
                            .is_none_or(|state| state.0 != LinkState::Wired)
                    });
                    if unwired {
                        graph.traversal_mut().e(link_id).drop();
                    }
                }
                PendingOperation::AddNode(node_run_id) => {
                    let unbuilt = graph
                        .traversal()
                        .v(node_run_id)
                        .first()
                        .is_some_and(|node| !a_commit_built(node));
                    if unbuilt && !graph.drop_the_node(node_run_id, WhyTheNodeLeaves::Removed) {
                        tracing::warn!(
                            "`{node_run_id}`, taken back, was not dropped from the graph"
                        );
                    }
                }
                PendingOperation::RemoveLink(link_id) => {
                    if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
                        link.remove::<PendingDeletionComponent>();
                    }
                }
                PendingOperation::RemoveNode(node_run_id, _) => {
                    let links_of_the_node: Vec<LinkUniqueId> = graph
                        .traversal()
                        .e(())
                        .iter()
                        .filter(|link| {
                            link.source().node_run_id() == node_run_id
                                || link.to_port().node_run_id() == node_run_id
                        })
                        .map(|link| link.id.clone())
                        .collect();
                    for link_id in links_of_the_node {
                        if let Some(link) = graph.traversal_mut().e(&link_id).first_mut() {
                            link.remove::<PendingDeletionComponent>();
                        }
                    }
                    if let Some(node) = graph.traversal_mut().v(node_run_id).first_mut() {
                        node.remove::<PendingDeletionComponent>();
                    }
                }
                _ => {}
            }
        }
    }

    /// Take the config updates logged under the hold for one commit, which a commit asked for
    /// meanwhile compiles at once: a value set reaches its node as it would outside the hold,
    /// and the graph edits logged beside it wait for the commit that releases the hold.
    pub(super) fn take_the_config_updates_logged_under_the_hold(
        &self,
        logged_before: usize,
    ) -> Vec<PendingOperation> {
        let mut transaction = self.transaction.lock();
        let logged_before = logged_before.min(transaction.len());
        let config_updates: Vec<PendingOperation> = transaction
            .extract_if(logged_before.., |operation| {
                matches!(operation, PendingOperation::UpdateNodeConfig { .. })
            })
            .collect();
        let mut configs_a_commit_is_applying = self.configs_a_commit_is_applying.lock();
        for operation in &config_updates {
            if let PendingOperation::UpdateNodeConfig {
                node_run_id,
                config_to_apply,
            } = operation
            {
                configs_a_commit_is_applying.insert(node_run_id.clone(), config_to_apply.clone());
            }
        }
        config_updates
    }
}
