// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value set on a setting a node reads at setup rebuilds that one node while its stream runs:
//! each set saves the value and leaves the node to rebuild, and the next rebuild stops it and
//! builds it again once, however many of its settings were set, the rest of the stream running
//! on. A rebuild that fails is answered with the node's own words. A value a Value wire carries
//! into such a setting rebuilds its node the same way, once for a burst of them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{LoadedStreamInThisRuntime, RuntimeStatus};
use crate::core::compiler::PendingOperation;
use crate::core::graph::input_values::ValueAWireCarriedIntoASetting;
use crate::core::graph::{GraphNodeWithComponents, NodeRunId, ObservableNodeState, StateComponent};
use crate::core::logging::carrying_this_threads_project_log_route_entry;
use crate::core::nodes::NodeState;
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, Result};

/// How long a rebuild waits for each node it builds again to finish its setup: past the longest
/// a helper process is given to start and register, so a helper-hosted node's own refusal
/// arrives before the wait ends.
const LONGEST_WAIT_FOR_A_REBUILT_NODE_TO_SET_UP: Duration = Duration::from_secs(90);

/// How long a rebuild a wire into a setting asks for waits for more of a burst of values on
/// wires into the stream's settings, so the burst rebuilds each node once with its last values.
pub(crate) const A_BURST_OF_VALUES_ON_A_WIRE_INTO_A_SETTING_SETTLES_WITHIN: Duration =
    Duration::from_millis(50);

/// A node a rebuild builds again: its name, its UUID and its state.
struct NodeRebuilt {
    node_run_id: NodeRunId,
    display_name: String,
    node_uuid: Uuid,
    state: Arc<ObservableNodeState>,
}

/// How one node's rebuild went: its UUID, and why it did not build, in its own words.
struct HowANodesRebuildWent {
    node_uuid: Uuid,
    refused_because: Option<String>,
}

/// Why `node` did not build, settled as `settled` after its rebuild, in its own words.
fn the_refusal_of_a_rebuild(node: &NodeRebuilt, settled: NodeState) -> String {
    let why = match (settled, node.state.failure_reason()) {
        (NodeState::Error, Some(reason)) => reason,
        (settled, _) if settled.is_before_setup_completed() => format!(
            "it had not finished its setup within {}s",
            LONGEST_WAIT_FOR_A_REBUILT_NODE_TO_SET_UP.as_secs()
        ),
        (settled, _) => format!("it reads {settled} after its setup"),
    };
    format!(
        "`{}` was rebuilt to read the settings set, and did not build with them: {why}",
        node.display_name
    )
}

impl LoadedStreamInThisRuntime {
    /// Whether input `input` of the node `node_run_id` is a setting the node reads once, when it
    /// is set up.
    pub(crate) fn is_a_setting_the_node_reads_at_setup(
        &self,
        node_run_id: &NodeRunId,
        input: &str,
    ) -> bool {
        self.compiler
            .scope(|graph, _tx| graph.is_a_setting_read_at_setup(node_run_id, input))
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
        let rebuilt = self.rebuild_the_nodes(node_run_ids).await?;
        if let Some(refusal) = rebuilt.iter().find_map(|node| node.refused_because.clone()) {
            return Err(Error::Configuration(refusal));
        }
        Ok(rebuilt.iter().map(|node| node.node_uuid).collect())
    }

    /// Rebuild each of `node_run_ids` once, as
    /// [`Self::rebuild_each_node_a_setting_read_at_setup_changed`] does, and answer how each
    /// one's rebuild went; refused only when the compile refuses the rebuild.
    async fn rebuild_the_nodes(
        &self,
        node_run_ids: Vec<NodeRunId>,
    ) -> Result<Vec<HowANodesRebuildWent>> {
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
        Ok(nodes_rebuilt
            .iter()
            .zip(settled)
            .map(|(node, settled)| HowANodesRebuildWent {
                node_uuid: node.node_uuid,
                refused_because: (!matches!(settled, NodeState::Running | NodeState::Paused))
                    .then(|| the_refusal_of_a_rebuild(node, settled)),
            })
            .collect())
    }

    /// Rebuild, once each, every running node a Value wire into one of its settings read at
    /// setup carried a new value into, after a burst of them settles: each node is built with
    /// the last value each of its wires carried. A node that does not build with them is
    /// refused in its own words, logged, and built again from the values it had; the values it
    /// refused are not tried again until its wire carries another. A stream not started leaves
    /// the values waiting.
    pub(crate) async fn rebuild_each_node_a_wire_into_its_settings_changed(&self) {
        tokio::time::sleep(A_BURST_OF_VALUES_ON_A_WIRE_INTO_A_SETTING_SETTLES_WITHIN).await;
        if *self.status.lock() != RuntimeStatus::Started {
            return;
        }
        let carried = self
            .compiler
            .scope(|graph, _tx| Arc::clone(graph.values_wires_carry_into_settings()));
        let values = carried.take_each_value_waiting();
        let node_run_ids = self.the_nodes_holding_the_settings(&values);
        if node_run_ids.is_empty() {
            return;
        }
        let rebuilt = match self.rebuild_the_nodes(node_run_ids).await {
            Ok(rebuilt) => rebuilt,
            Err(not_compiled) => {
                tracing::warn!(
                    %not_compiled,
                    "the rebuild of each node a wire into its settings carried a new value into \
                     was refused, so their values are put back"
                );
                carried.put_back(&values);
                return;
            }
        };
        let refused: Vec<Uuid> = rebuilt
            .iter()
            .filter_map(|node| {
                let refusal = node.refused_because.as_ref()?;
                tracing::warn!(
                    node = %node.node_uuid,
                    "{refusal}; it is built again from the values it had"
                );
                Some(node.node_uuid)
            })
            .collect();
        tracing::info!(
            nodes_rebuilt = rebuilt.len() - refused.len(),
            nodes_refused = refused.len(),
            "rebuilt each node a wire into its settings carried a new value into"
        );
        if refused.is_empty() {
            return;
        }
        let values_refused: Vec<ValueAWireCarriedIntoASetting> = values
            .into_iter()
            .filter(|carried| refused.contains(&carried.node))
            .collect();
        carried.put_back(&values_refused);
        let node_run_ids = self.the_nodes_holding_the_settings(&values_refused);
        match self.rebuild_the_nodes(node_run_ids).await {
            Ok(rebuilt) => {
                for refusal in rebuilt
                    .iter()
                    .filter_map(|node| node.refused_because.as_ref())
                {
                    tracing::error!(
                        "{refusal}, after the values a wire carried into its settings were put \
                         back"
                    );
                }
            }
            Err(not_rebuilt) => tracing::error!(
                %not_rebuilt,
                "the nodes whose wired settings were put back were not rebuilt"
            ),
        }
    }

    /// Each node of this stream, once, that holds a setting of `values`.
    fn the_nodes_holding_the_settings(
        &self,
        values: &[ValueAWireCarriedIntoASetting],
    ) -> Vec<NodeRunId> {
        self.compiler.scope(|graph, _tx| {
            let mut node_run_ids: Vec<NodeRunId> = Vec::new();
            for carried in values {
                if let Some(node) = graph.node_by_uuid(carried.node)
                    && !node_run_ids.contains(&node.id)
                {
                    node_run_ids.push(node.id.clone());
                }
            }
            node_run_ids
        })
    }
}
