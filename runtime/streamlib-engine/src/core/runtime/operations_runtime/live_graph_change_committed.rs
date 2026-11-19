// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Committing a live graph change: compiled on a stream a commit builds, and otherwise handed to
//! the graph it holds to read and edit at once.

use std::collections::HashSet;
use std::sync::Arc;

use super::LiveCommitContext;
use crate::core::compiler::{ACommitFailed, Compiler, WhatTheHeldCommitFound};
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId, NodeInstanceComponent,
    NodeRunId, PendingDeletionComponent, WhyTheNodeLeaves,
};
use crate::core::logging::carrying_this_threads_project_log_route_entry;
use crate::core::{Error, Result};

pub(super) async fn commit_live_graph_change(
    compiler: &Arc<Compiler>,
    live: LiveCommitContext,
) -> Result<()> {
    let Some(runtime_ctx) = live else {
        if !compiler.the_transaction_is_held_for_one_commit() {
            take_out_now_each_node_and_link_pending_deletion_no_commit_builds(compiler);
        }
        hand_each_config_update_to_its_node_now_no_commit_builds(compiler);
        return Ok(());
    };
    let compiler = Arc::clone(compiler);
    compile_on_a_blocking_thread(move || compiler.commit(&runtime_ctx)).await?
}

/// Run `compile` on one of the engine's blocking threads, carrying this thread's project log
/// route; refused when its task does not finish.
async fn compile_on_a_blocking_thread<T: Send + 'static>(
    compile: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(carrying_this_threads_project_log_route_entry(compile))
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "the graph change's compile task did not finish: {join_failure}"
            ))
        })
}

/// Compile the graph edits held for one commit, releasing the compiler's hold in the turn the
/// commit holds, so no other commit takes them first: on a stream no commit builds, released
/// and handled as [`commit_live_graph_change`] handles a change there. Either says when it
/// found the hold taken back, and then compiled nothing.
pub(super) async fn commit_the_graph_edits_held_for_one_commit(
    compiler: &Arc<Compiler>,
    live: LiveCommitContext,
) -> std::result::Result<WhatTheHeldCommitFound, ACommitFailed> {
    let Some(runtime_ctx) = live else {
        if !compiler.release_the_transaction_held_for_one_commit() {
            return Ok(WhatTheHeldCommitFound::TheHoldTakenBack);
        }
        return commit_live_graph_change(compiler, None)
            .await
            .map(|()| WhatTheHeldCommitFound::TheHold)
            .map_err(|refusal| ACommitFailed {
                refusal,
                after_changing_what_runs: false,
            });
    };
    let compiler = Arc::clone(compiler);
    compile_on_a_blocking_thread(move || {
        compiler.commit_releasing_the_hold_for_one_commit(&runtime_ctx)
    })
    .await
    .map_err(|refusal| ACommitFailed {
        refusal,
        after_changing_what_runs: true,
    })?
}

/// On a stream no commit builds, a stopped one or one not started, take each node and link
/// pending deletion out of the graph now, with every operation logged for it: the graph it holds
/// to read and edit then shows the removal, and the UUID and name it held are free again.
fn take_out_now_each_node_and_link_pending_deletion_no_commit_builds(compiler: &Compiler) {
    compiler.scope(|this_streams_graph, tx| {
        let links_pending_deletion: HashSet<LinkUniqueId> = this_streams_graph
            .traversal()
            .e(())
            .iter()
            .filter(|link| link.has::<PendingDeletionComponent>())
            .map(|link| link.id.clone())
            .collect();
        let nodes_pending_deletion: HashSet<NodeRunId> = this_streams_graph
            .traversal()
            .v(())
            .iter()
            .filter(|node| {
                node.has::<PendingDeletionComponent>() && !node.has::<NodeInstanceComponent>()
            })
            .map(|node| node.id.clone())
            .collect();
        tx.retract_every_operation_naming(&nodes_pending_deletion, &links_pending_deletion);
        for link_id in &links_pending_deletion {
            this_streams_graph.traversal_mut().e(link_id).drop();
        }
        for node_run_id in &nodes_pending_deletion {
            if !this_streams_graph.drop_the_node(node_run_id, WhyTheNodeLeaves::Removed) {
                tracing::warn!("`{node_run_id}`, pending deletion, was not dropped from the graph");
            }
        }
    });
}

/// On a stream no commit builds, a stopped one or one not started, hand each config update
/// logged for a node no instance runs to that node now: the graph it holds to read and edit then
/// shows the config, and the node is built from it at the next start.
fn hand_each_config_update_to_its_node_now_no_commit_builds(compiler: &Compiler) {
    compiler.scope(|this_streams_graph, tx| {
        let nodes_no_instance_runs: HashSet<NodeRunId> = this_streams_graph
            .traversal()
            .v(())
            .iter()
            .filter(|node| !node.has::<NodeInstanceComponent>())
            .map(|node| node.id.clone())
            .collect();
        for (node_run_id, config_to_apply) in tx.take_each_config_update_of(&nodes_no_instance_runs)
        {
            if let Some(node) = this_streams_graph
                .traversal_mut()
                .v(&node_run_id)
                .first_mut()
            {
                node.set_config(config_to_apply);
            }
        }
    });
}
