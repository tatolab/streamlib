// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, GraphState, NodePauseGateComponent, NodeReadyBarrierComponent,
    NodeReadyBarrierHandle, NodeRunId, ShutdownChannelComponent, StateComponent,
    StepProgressOfOneNodeComponent,
};
use crate::core::nodes::NodeState;
use crate::core::pubsub::LoadedStreamIdentity;

pub(crate) fn prepare_node(
    graph: &mut Graph,
    proc_id: &NodeRunId,
    the_stream_the_node_belongs_to: &LoadedStreamIdentity,
) -> Result<NodeReadyBarrierHandle> {
    let paused_with_its_stream = graph.state() == GraphState::Paused;
    let node_mut = graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .ok_or_else(|| Error::NodeNotFound(format!("Node '{}' not found", proc_id)))?;

    let (barrier_component, barrier_handle) = NodeReadyBarrierComponent::new();

    node_mut.insert(barrier_component);
    node_mut.insert(ShutdownChannelComponent::new());
    let pause_gate = NodePauseGateComponent::new();
    pause_gate.set_paused(paused_with_its_stream);
    node_mut.insert(pause_gate);
    node_mut.insert_component_without_rendering_it(StepProgressOfOneNodeComponent::default());

    // Transitioned, never re-inserted: `add_v` attached the state when the node
    // was added, and a fresh component would strand every waiter already
    // holding the old one.
    if let Some(state) = node_mut.get::<StateComponent>() {
        state.publish_each_change_as_a_node_of(
            proc_id.clone(),
            the_stream_the_node_belongs_to.clone(),
        );
        state.transition_to(NodeState::Idle);
    }

    tracing::debug!("[{}] Infrastructure components attached", proc_id);
    Ok(barrier_handle)
}

#[cfg(test)]
mod tests;
