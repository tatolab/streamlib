// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A setting a node reads only when it is set up, set while the node runs: the node is stopped
//! and built again from the values its inputs hold, once for every setting one edit sets on it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{LoadedStreamInThisRuntime, commit_live_graph_change};
use crate::core::compiler::PendingOperation;
use crate::core::graph::{
    GraphNodeWithComponents, NodeRunId, ObservableNodeState, StateComponent, ThreadHandleComponent,
};
use crate::core::nodes::NodeState;
use crate::core::{Error, Result};

/// How long a node built again for a setting has to finish its setup: as long as a helper
/// process has to register, the longest step a build of a helper-hosted node waits on.
const SETUP_OF_A_NODE_BUILT_AGAIN_DEADLINE: Duration = Duration::from_secs(60);

/// A node built again for a setting, as its setup is awaited.
struct ANodeBuiltAgain {
    display_name: String,
    node_uuid: uuid::Uuid,
    state: Arc<ObservableNodeState>,
}

impl LoadedStreamInThisRuntime {
    /// Note that the node `node_run_id` is built again, when input `input` it just took a value
    /// on is a setting it reads at setup and it has been built, running or paused. A node not yet
    /// built reads the value when it is, so nothing is noted for it. The build is logged for a
    /// commit only once the edit has set every setting, so a commit a graph change starts
    /// meanwhile does not build the node from part of them.
    pub(in crate::core::runtime) fn build_the_node_again_when_it_reads_the_input_at_setup(
        &self,
        node_run_id: &NodeRunId,
        input: &str,
    ) {
        let is_built_and_reads_the_input_at_setup = self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(node_run_id)
                .first()
                .is_some_and(|node| {
                    node.has::<ThreadHandleComponent>()
                        && node
                            .input_port(input)
                            .is_some_and(|port| port.is_a_setting_read_at_setup())
                })
        });
        if !is_built_and_reads_the_input_at_setup {
            return;
        }
        let mut nodes = self.nodes_a_setting_read_at_setup_was_set_on.lock();
        if !nodes.contains(node_run_id) {
            nodes.push(node_run_id.clone());
        }
    }

    /// Build again each running node a setting it reads at setup was set on since the last
    /// call, and wait for each one's setup. Answers the UUID of each node built again and now
    /// running, in the order the settings were set; refuses with the node's own reason when one
    /// fails its setup, leaving it in `Error`. On a stream not running, answers none: a paused
    /// stream builds each node at the commit of its resume, a stopped one at its start.
    pub(in crate::core::runtime) async fn build_again_each_node_a_setting_was_set_on(
        &self,
    ) -> Result<Vec<uuid::Uuid>> {
        let nodes = std::mem::take(&mut *self.nodes_a_setting_read_at_setup_was_set_on.lock());
        if nodes.is_empty() {
            return Ok(Vec::new());
        }
        self.compiler.scope(|_graph, tx| {
            for node_run_id in &nodes {
                tx.log(PendingOperation::RebuildNode(node_run_id.clone()));
            }
        });
        let live = self.live_commit_context();
        if live.is_none() {
            return Ok(Vec::new());
        }
        commit_live_graph_change(&self.compiler, live).await?;
        let built_again: Vec<ANodeBuiltAgain> = self.compiler.scope(|graph, _tx| {
            nodes
                .iter()
                .filter_map(|node_run_id| {
                    let node = graph.traversal().v(node_run_id).first()?;
                    Some(ANodeBuiltAgain {
                        display_name: node.display_name.clone(),
                        node_uuid: node.node_uuid,
                        state: node.get::<StateComponent>()?.clone_inner(),
                    })
                })
                .collect()
        });
        tokio::task::spawn_blocking(move || {
            wait_for_the_setup_of_each_node_built_again(
                built_again,
                Instant::now() + SETUP_OF_A_NODE_BUILT_AGAIN_DEADLINE,
            )
        })
        .await
        .map_err(|join_error| {
            Error::Runtime(format!(
                "the wait for the setup of each node built again for a setting ended: \
                 {join_error}"
            ))
        })?
    }
}

/// Wait until each of `built_again` finishes its setup, by `deadline`: answers their UUIDs when
/// each runs, and refuses with the first that does not, by its name and its own reason.
fn wait_for_the_setup_of_each_node_built_again(
    built_again: Vec<ANodeBuiltAgain>,
    deadline: Instant,
) -> Result<Vec<uuid::Uuid>> {
    built_again
        .into_iter()
        .map(
            |node| match node.state.wait_until_setup_resolved(deadline) {
                NodeState::Running => Ok(node.node_uuid),
                settled if settled.is_before_setup_completed() => Err(Error::Runtime(format!(
                    "`{}` was built again for a setting it reads at setup and had not finished its \
                 setup within {SETUP_OF_A_NODE_BUILT_AGAIN_DEADLINE:?} (still {settled})",
                    node.display_name
                ))),
                settled => Err(Error::Runtime(format!(
                    "`{}` was built again for a setting it reads at setup and is {settled}: {}",
                    node.display_name,
                    node.state
                        .failure_reason()
                        .unwrap_or_else(|| "it gave no reason".to_owned())
                ))),
            },
        )
        .collect()
}
