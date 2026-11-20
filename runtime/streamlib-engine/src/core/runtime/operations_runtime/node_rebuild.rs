// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value set on a setting a node reads at setup rebuilds that one node while its stream runs:
//! each set saves the value and leaves the node to rebuild, and the next rebuild stops it and
//! builds it again once, however many of its settings were set, the rest of the stream running
//! on. A rebuild that fails is answered with the node's own words.

use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{LoadedStreamInThisRuntime, RuntimeStatus};
use crate::core::compiler::PendingOperation;
use crate::core::graph::{GraphNodeWithComponents, NodeRunId, ObservableNodeState, StateComponent};
use crate::core::logging::carrying_this_threads_project_log_route_entry;
use crate::core::nodes::NodeState;
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, Result};

/// How long a rebuild waits for each node it builds again to finish its setup: past the longest
/// a helper process is given to start and register, so a helper-hosted node's own refusal
/// arrives before the wait ends.
const LONGEST_WAIT_FOR_A_REBUILT_NODE_TO_SET_UP: Duration = Duration::from_secs(90);

/// A node a rebuild builds again: its name, its UUID and its state.
struct NodeRebuilt {
    node_run_id: NodeRunId,
    display_name: String,
    node_uuid: Uuid,
    state: Arc<ObservableNodeState>,
}

impl LoadedStreamInThisRuntime {
    /// Whether input `input` of the node `node_run_id` is a setting the node reads once, when it
    /// is set up.
    pub(crate) fn is_a_setting_the_node_reads_at_setup(
        &self,
        node_run_id: &NodeRunId,
        input: &str,
    ) -> bool {
        self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(node_run_id)
                .first()
                .is_some_and(|node| {
                    node.input_port(input)
                        .is_some_and(|port| port.is_a_setting_read_at_setup())
                })
        })
    }

    /// Leave the node `node_run_id` to rebuild at the next
    /// [`Self::rebuild_each_node_a_setting_read_at_setup_changed`], while this stream is
    /// started. A stopped stream builds the node from the values saved at its next start.
    pub(crate) fn leave_the_node_to_rebuild(&self, node_run_id: &NodeRunId) {
        if *self.status.lock() != RuntimeStatus::Started {
            return;
        }
        let mut nodes_to_rebuild = self.nodes_a_setting_read_at_setup_rebuilds.lock();
        if !nodes_to_rebuild.contains(node_run_id) {
            nodes_to_rebuild.push(node_run_id.clone());
        }
    }

    /// Rebuild, once each, every running node a set on a setting read at setup has left to
    /// rebuild since the last rebuild, and wait for each to finish its setup; the UUID of each
    /// rebuilt, in the order first set. A stream not started rebuilds none and answers none.
    ///
    /// Refused, naming the node and in its own words, when a node rebuilt does not finish its
    /// setup: it stays as its setup left it, reading `Error` with `components.failed_because`,
    /// for the caller to put its earlier values back and rebuild it from them. Refused by name,
    /// rebuilding nothing, when the compile refuses the rebuild.
    pub(crate) async fn rebuild_each_node_a_setting_read_at_setup_changed(
        &self,
    ) -> Result<Vec<Uuid>> {
        let node_run_ids = std::mem::take(&mut *self.nodes_a_setting_read_at_setup_rebuilds.lock());
        if node_run_ids.is_empty() {
            return Ok(Vec::new());
        }
        if self.live_commit_context().is_none() {
            return Ok(Vec::new());
        }
        // Logged and compiled holding the stream's one writer, so no other edit's commit takes
        // the rebuild or compiles between the log and the commit.
        let rebuild_began = Instant::now();
        let compiled = self
            .in_one_commit(async {
                Ok(self.compiler.scope(|graph, tx| {
                    node_run_ids
                        .iter()
                        .filter_map(|node_run_id| {
                            let node = graph.traversal().v(node_run_id).first()?;
                            let state = node.get::<StateComponent>()?.clone_inner();
                            tx.log(PendingOperation::RebuildNode(node_run_id.clone()));
                            Some(NodeRebuilt {
                                node_run_id: node_run_id.clone(),
                                display_name: node.display_name.clone(),
                                node_uuid: node.node_uuid,
                                state,
                            })
                        })
                        .collect::<Vec<NodeRebuilt>>()
                }))
            })
            .await;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        let nodes_rebuilt = compiled?;
        let deadline = rebuild_began + LONGEST_WAIT_FOR_A_REBUILT_NODE_TO_SET_UP;
        let states: Vec<Arc<ObservableNodeState>> = nodes_rebuilt
            .iter()
            .map(|node| Arc::clone(&node.state))
            .collect();
        let settled: Vec<NodeState> =
            tokio::task::spawn_blocking(carrying_this_threads_project_log_route_entry(move || {
                states
                    .iter()
                    .map(|state| state.wait_until_setup_resolved(deadline))
                    .collect()
            }))
            .await
            .map_err(|join_failure| {
                Error::Runtime(format!(
                    "the wait for the rebuilt nodes to set up did not finish: {join_failure}"
                ))
            })?;
        let rebuilt_in = rebuild_began.elapsed();
        for (node, settled) in nodes_rebuilt.iter().zip(&settled) {
            tracing::info!(
                node = %node.node_run_id,
                state = %settled,
                rebuilt_in_ms = rebuilt_in.as_millis(),
                "rebuilt `{}` to read the settings it reads at setup",
                node.display_name
            );
        }
        if let Some((node, settled)) = nodes_rebuilt
            .iter()
            .zip(settled)
            .find(|(_, settled)| !matches!(settled, NodeState::Running | NodeState::Paused))
        {
            let why = match (settled, node.state.failure_reason()) {
                (NodeState::Error, Some(reason)) => reason,
                (settled, _) if settled.is_before_setup_completed() => format!(
                    "it had not finished its setup within {}s",
                    LONGEST_WAIT_FOR_A_REBUILT_NODE_TO_SET_UP.as_secs()
                ),
                (settled, _) => format!("it reads {settled} after its setup"),
            };
            return Err(Error::Configuration(format!(
                "`{}` was rebuilt to read the settings set, and did not build with them: {why}",
                node.display_name
            )));
        }
        Ok(nodes_rebuilt.iter().map(|node| node.node_uuid).collect())
    }
}
