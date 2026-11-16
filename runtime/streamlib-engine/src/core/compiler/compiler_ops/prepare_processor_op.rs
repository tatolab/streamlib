// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, GraphState, ProcessorPauseGateComponent,
    ProcessorReadyBarrierComponent, ProcessorReadyBarrierHandle, ProcessorUniqueId,
    ShutdownChannelComponent, StateComponent, StepProgressOfOneNodeComponent,
};
use crate::core::processors::ProcessorState;
use crate::core::pubsub::LoadedStreamIdentity;

pub(crate) fn prepare_processor(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
    the_stream_the_processor_belongs_to: &LoadedStreamIdentity,
) -> Result<ProcessorReadyBarrierHandle> {
    let paused_with_its_stream = graph.state() == GraphState::Paused;
    let node_mut = graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .ok_or_else(|| Error::ProcessorNotFound(format!("Processor '{}' not found", proc_id)))?;

    let (barrier_component, barrier_handle) = ProcessorReadyBarrierComponent::new();

    node_mut.insert(barrier_component);
    node_mut.insert(ShutdownChannelComponent::new());
    let pause_gate = ProcessorPauseGateComponent::new();
    pause_gate.set_paused(paused_with_its_stream);
    node_mut.insert(pause_gate);
    node_mut.insert_component_without_rendering_it(StepProgressOfOneNodeComponent::default());

    // Transitioned, never re-inserted: `add_v` attached the state when the node
    // was added, and a fresh component would strand every waiter already
    // holding the old one.
    if let Some(state) = node_mut.get::<StateComponent>() {
        state.publish_each_change_as_a_processor_of(
            proc_id.clone(),
            the_stream_the_processor_belongs_to.clone(),
        );
        state.transition_to(ProcessorState::Idle);
    }

    tracing::debug!("[{}] Infrastructure components attached", proc_id);
    Ok(barrier_handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::{MockOutputOnlyProcessor, ensure_test_mocks_registered};

    fn the_pause_gate_a_processor_is_prepared_with_while_its_graph_is(
        graph_state: GraphState,
    ) -> bool {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .expect("the node is added");
        let processor_id = graph.traversal().v(()).ids().remove(0);
        graph.set_state(graph_state);

        let _barrier = prepare_processor(
            &mut graph,
            &processor_id,
            &crate::core::pubsub::LoadedStreamIdentity {
                runtime_id: "prepare-processor-test".to_string(),
                stream_id: "main".to_string(),
                stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
            },
        )
        .expect("the node is prepared");

        graph
            .traversal()
            .v(&processor_id)
            .first()
            .and_then(|node| node.get::<ProcessorPauseGateComponent>())
            .expect("the node holds a pause gate")
            .is_paused()
    }

    #[test]
    fn a_processor_prepared_while_its_stream_is_paused_starts_paused() {
        assert!(the_pause_gate_a_processor_is_prepared_with_while_its_graph_is(GraphState::Paused));
        assert!(
            !the_pause_gate_a_processor_is_prepared_with_while_its_graph_is(GraphState::Running)
        );
    }
}
