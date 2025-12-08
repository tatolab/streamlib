// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Regression tests for ECS component storage behavior.
//!
//! These tests document the expected behavior of component storage on processors
//! and links. They were written before the ECS removal migration to ensure
//! behavioral equivalence after migration.
//!
//! Test approach:
//! 1. Write tests against current (hecs-based) API
//! 2. Verify all tests pass
//! 3. Migrate implementation
//! 4. Verify all tests still pass (behavior preserved)

#![cfg(test)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::core::graph::components::{
    ProcessorMetrics, ProcessorPauseGate, ShutdownChannel, StateComponent,
};
use crate::core::graph::link::LinkState;
use crate::core::graph::Graph;
use crate::core::processors::ProcessorState;

// =============================================================================
// Test Components
// =============================================================================

/// Simple test component with a value.
struct TestComponent(i32);

/// Another test component to verify multiple components work.
struct AnotherComponent(String);

/// Counter component for verifying component mutations.
struct CounterComponent(Arc<AtomicU64>);

impl CounterComponent {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    fn increment(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn value(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

// =============================================================================
// 1. ProcessorNode Component Tests
// =============================================================================

#[test]
fn test_processor_node_insert_component() {
    let mut graph = Graph::new();

    // Add processor to graph topology
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert a component
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();

    // Verify component exists
    assert!(graph.has::<TestComponent>(&"proc1".into()));
}

#[test]
fn test_processor_node_get_component() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();

    // Get component and verify value
    let comp = graph.get::<TestComponent>(&"proc1".into()).unwrap();
    assert_eq!(comp.0, 42);
}

#[test]
fn test_processor_node_get_mut_component() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();

    // Mutate component
    {
        let mut comp = graph.get_mut::<TestComponent>(&"proc1".into()).unwrap();
        comp.0 = 100;
    }

    // Verify mutation persisted
    let comp = graph.get::<TestComponent>(&"proc1".into()).unwrap();
    assert_eq!(comp.0, 100);
}

#[test]
fn test_processor_node_remove_component() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();

    // Remove component
    let removed = graph.remove::<TestComponent>(&"proc1".into()).unwrap();
    assert_eq!(removed.0, 42);

    // Verify component no longer exists
    assert!(!graph.has::<TestComponent>(&"proc1".into()));
    assert!(graph.get::<TestComponent>(&"proc1".into()).is_none());
}

#[test]
fn test_processor_node_has_component() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Initially no component
    assert!(!graph.has::<TestComponent>(&"proc1".into()));

    // After insert, has component
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();
    assert!(graph.has::<TestComponent>(&"proc1".into()));

    // After remove, no component
    graph.remove::<TestComponent>(&"proc1".into());
    assert!(!graph.has::<TestComponent>(&"proc1".into()));
}

#[test]
fn test_processor_node_multiple_components() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert multiple components of different types
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();
    graph
        .insert(&"proc1".into(), AnotherComponent("hello".into()))
        .unwrap();

    // Both components exist independently
    assert!(graph.has::<TestComponent>(&"proc1".into()));
    assert!(graph.has::<AnotherComponent>(&"proc1".into()));

    // Get each component (drop borrows before mutation)
    {
        let test_comp = graph.get::<TestComponent>(&"proc1".into()).unwrap();
        let another_comp = graph.get::<AnotherComponent>(&"proc1".into()).unwrap();
        assert_eq!(test_comp.0, 42);
        assert_eq!(another_comp.0, "hello");
    }

    // Remove one, other remains
    graph.remove::<TestComponent>(&"proc1".into());
    assert!(!graph.has::<TestComponent>(&"proc1".into()));
    assert!(graph.has::<AnotherComponent>(&"proc1".into()));
}

#[test]
fn test_processor_node_component_overwrite() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert component
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();
    let comp = graph.get::<TestComponent>(&"proc1".into()).unwrap();
    assert_eq!(comp.0, 42);
    drop(comp);

    // Insert again (overwrite)
    graph.insert(&"proc1".into(), TestComponent(100)).unwrap();
    let comp = graph.get::<TestComponent>(&"proc1".into()).unwrap();
    assert_eq!(comp.0, 100);
}

#[test]
fn test_processor_node_component_after_removal() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();

    // Remove processor
    graph.remove_processor(&"proc1".into());

    // Component access should fail gracefully
    assert!(!graph.has::<TestComponent>(&"proc1".into()));
    assert!(graph.get::<TestComponent>(&"proc1".into()).is_none());
}

#[test]
fn test_processor_node_state_component() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert StateComponent
    graph
        .insert(&"proc1".into(), StateComponent::default())
        .unwrap();

    // Initial state is Idle
    {
        let state = graph.get::<StateComponent>(&"proc1".into()).unwrap();
        assert_eq!(*state.0.lock(), ProcessorState::Idle);
    }

    // Modify state
    {
        let state = graph.get::<StateComponent>(&"proc1".into()).unwrap();
        *state.0.lock() = ProcessorState::Running;
    }

    // Verify state persisted
    {
        let state = graph.get::<StateComponent>(&"proc1".into()).unwrap();
        assert_eq!(*state.0.lock(), ProcessorState::Running);
    }
}

#[test]
fn test_processor_node_pause_gate() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert pause gate
    graph
        .insert(&"proc1".into(), ProcessorPauseGate::new())
        .unwrap();

    // Initially not paused
    {
        let gate = graph.get::<ProcessorPauseGate>(&"proc1".into()).unwrap();
        assert!(!gate.is_paused());
        assert!(gate.should_process());
    }

    // Pause
    {
        let gate = graph.get::<ProcessorPauseGate>(&"proc1".into()).unwrap();
        gate.pause();
    }

    // Verify paused
    {
        let gate = graph.get::<ProcessorPauseGate>(&"proc1".into()).unwrap();
        assert!(gate.is_paused());
        assert!(!gate.should_process());
    }

    // Resume
    {
        let gate = graph.get::<ProcessorPauseGate>(&"proc1".into()).unwrap();
        gate.resume();
    }

    // Verify resumed
    {
        let gate = graph.get::<ProcessorPauseGate>(&"proc1".into()).unwrap();
        assert!(!gate.is_paused());
    }
}

#[test]
fn test_processor_node_shutdown_channel() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert shutdown channel
    graph
        .insert(&"proc1".into(), ShutdownChannel::new())
        .unwrap();

    // Take receiver
    let receiver = {
        let mut channel = graph.get_mut::<ShutdownChannel>(&"proc1".into()).unwrap();
        channel.take_receiver().expect("receiver should exist")
    };

    // Second take should return None
    {
        let mut channel = graph.get_mut::<ShutdownChannel>(&"proc1".into()).unwrap();
        assert!(channel.take_receiver().is_none());
    }

    // Send shutdown signal
    {
        let channel = graph.get::<ShutdownChannel>(&"proc1".into()).unwrap();
        channel.sender.send(()).unwrap();
    }

    // Receive signal
    assert!(receiver.recv().is_ok());
}

#[test]
fn test_processor_node_metrics_component() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert metrics
    let mut metrics = ProcessorMetrics::default();
    metrics.throughput_fps = 30.0;
    metrics.frames_processed = 100;
    graph.insert(&"proc1".into(), metrics).unwrap();

    // Verify metrics
    {
        let m = graph.get::<ProcessorMetrics>(&"proc1".into()).unwrap();
        assert_eq!(m.throughput_fps, 30.0);
        assert_eq!(m.frames_processed, 100);
    }

    // Mutate metrics
    {
        let mut m = graph.get_mut::<ProcessorMetrics>(&"proc1".into()).unwrap();
        m.frames_processed += 50;
    }

    // Verify mutation
    {
        let m = graph.get::<ProcessorMetrics>(&"proc1".into()).unwrap();
        assert_eq!(m.frames_processed, 150);
    }
}

// =============================================================================
// 2. Link Component Tests
// =============================================================================

#[test]
fn test_link_insert_component() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);
    let link = graph.add_link("source.output", "sink.input").unwrap();

    // Insert component on link
    graph.insert_link(&link.id, TestComponent(42)).unwrap();

    // Verify component exists
    let comp = graph.get_link_component::<TestComponent>(&link.id).unwrap();
    assert_eq!(comp.0, 42);
}

#[test]
fn test_link_get_component() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);
    let link = graph.add_link("source.output", "sink.input").unwrap();

    graph
        .insert_link(&link.id, AnotherComponent("link_data".into()))
        .unwrap();

    let comp = graph
        .get_link_component::<AnotherComponent>(&link.id)
        .unwrap();
    assert_eq!(comp.0, "link_data");
}

#[test]
fn test_link_remove_component() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);
    let link = graph.add_link("source.output", "sink.input").unwrap();

    graph.insert_link(&link.id, TestComponent(42)).unwrap();
    assert!(graph
        .get_link_component::<TestComponent>(&link.id)
        .is_some());

    // Remove component
    graph
        .remove_link_component::<TestComponent>(&link.id)
        .unwrap();

    // Verify removed
    assert!(graph
        .get_link_component::<TestComponent>(&link.id)
        .is_none());
}

#[test]
fn test_link_state_get_set() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);
    let link = graph.add_link("source.output", "sink.input").unwrap();

    // Set link state
    graph.set_link_state(&link.id, LinkState::Wired).unwrap();

    // Get link state
    let state = graph.get_link_state(&link.id).unwrap();
    assert_eq!(state, LinkState::Wired);
}

#[test]
fn test_link_state_transitions() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);
    let link = graph.add_link("source.output", "sink.input").unwrap();

    // Initial state (after set) is Pending
    graph.set_link_state(&link.id, LinkState::Pending).unwrap();
    assert_eq!(graph.get_link_state(&link.id).unwrap(), LinkState::Pending);

    // Transition to Wired
    graph.set_link_state(&link.id, LinkState::Wired).unwrap();
    assert_eq!(graph.get_link_state(&link.id).unwrap(), LinkState::Wired);

    // Transition to Disconnecting
    graph
        .set_link_state(&link.id, LinkState::Disconnecting)
        .unwrap();
    assert_eq!(
        graph.get_link_state(&link.id).unwrap(),
        LinkState::Disconnecting
    );

    // Transition to Disconnected
    graph
        .set_link_state(&link.id, LinkState::Disconnected)
        .unwrap();
    assert_eq!(
        graph.get_link_state(&link.id).unwrap(),
        LinkState::Disconnected
    );
}

#[test]
fn test_link_component_after_removal() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);
    let link = graph.add_link("source.output", "sink.input").unwrap();
    let link_id = link.id.clone();

    graph.insert_link(&link_id, TestComponent(42)).unwrap();

    // Remove link fully (topology + ECS)
    graph.remove_link_fully(&link_id);

    // Component access should fail gracefully
    assert!(graph
        .get_link_component::<TestComponent>(&link_id)
        .is_none());
}

#[test]
fn test_link_multiple_components() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);
    let link = graph.add_link("source.output", "sink.input").unwrap();

    // Insert multiple components
    graph.insert_link(&link.id, TestComponent(42)).unwrap();
    graph
        .insert_link(&link.id, AnotherComponent("link".into()))
        .unwrap();

    // Both exist
    assert!(graph
        .get_link_component::<TestComponent>(&link.id)
        .is_some());
    assert!(graph
        .get_link_component::<AnotherComponent>(&link.id)
        .is_some());

    // Remove one, other remains
    graph
        .remove_link_component::<TestComponent>(&link.id)
        .unwrap();
    assert!(graph
        .get_link_component::<TestComponent>(&link.id)
        .is_none());
    assert!(graph
        .get_link_component::<AnotherComponent>(&link.id)
        .is_some());
}

// =============================================================================
// 3. Graph API Tests
// =============================================================================

#[test]
fn test_graph_add_processor_enables_components() {
    let mut graph = Graph::new();

    // Adding processor enables component storage
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Should be able to insert components
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();
    assert!(graph.has::<TestComponent>(&"proc1".into()));
}

#[test]
fn test_graph_add_link_enables_components() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);

    // Adding link enables component storage
    let link = graph.add_link("source.output", "sink.input").unwrap();

    // Should be able to insert components
    graph.insert_link(&link.id, TestComponent(42)).unwrap();
    assert!(graph
        .get_link_component::<TestComponent>(&link.id)
        .is_some());
}

#[test]
fn test_graph_remove_processor_cleans_components() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.insert(&"proc1".into(), TestComponent(42)).unwrap();
    graph
        .insert(&"proc1".into(), AnotherComponent("hello".into()))
        .unwrap();

    // Remove processor (should clean up components)
    graph.remove_processor(&"proc1".into());

    // Components should be gone
    assert!(!graph.has::<TestComponent>(&"proc1".into()));
    assert!(!graph.has::<AnotherComponent>(&"proc1".into()));
}

#[test]
fn test_graph_remove_link_cleans_components() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);
    let link = graph.add_link("source.output", "sink.input").unwrap();
    let link_id = link.id.clone();

    graph.insert_link(&link_id, TestComponent(42)).unwrap();

    // Remove link fully
    graph.remove_link_fully(&link_id);

    // Components should be gone
    assert!(graph
        .get_link_component::<TestComponent>(&link_id)
        .is_none());
}

#[test]
fn test_graph_processors_with_component() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.add_processor("proc2".into(), "TestProcessor".into(), 0);
    graph.add_processor("proc3".into(), "TestProcessor".into(), 0);

    // Only some processors have the component
    graph.insert(&"proc1".into(), TestComponent(1)).unwrap();
    graph.insert(&"proc3".into(), TestComponent(3)).unwrap();

    // Query for processors with component
    let with_component = graph.processors_with::<TestComponent>();
    assert_eq!(with_component.len(), 2);
    assert!(with_component.contains(&"proc1".into()));
    assert!(!with_component.contains(&"proc2".into()));
    assert!(with_component.contains(&"proc3".into()));
}

#[test]
fn test_graph_clear_entities() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.add_processor("proc2".into(), "TestProcessor".into(), 0);
    graph.insert(&"proc1".into(), TestComponent(1)).unwrap();
    graph.insert(&"proc2".into(), TestComponent(2)).unwrap();

    assert_eq!(graph.entity_count(), 2);

    // Clear all components from entities (processors remain, just empty component storage)
    graph.clear_entities();

    // Processors still exist (they ARE the entities now with embedded storage)
    assert_eq!(graph.entity_count(), 2);
    // But their components are cleared
    assert!(!graph.has::<TestComponent>(&"proc1".into()));
    assert!(!graph.has::<TestComponent>(&"proc2".into()));
}

#[test]
fn test_graph_entity_count() {
    let mut graph = Graph::new();

    assert_eq!(graph.entity_count(), 0);

    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    assert_eq!(graph.entity_count(), 1);

    graph.add_processor("proc2".into(), "TestProcessor".into(), 0);
    assert_eq!(graph.entity_count(), 2);

    graph.remove_processor(&"proc1".into());
    assert_eq!(graph.entity_count(), 1);
}

#[test]
fn test_graph_processor_ids() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.add_processor("proc2".into(), "TestProcessor".into(), 0);

    let ids: Vec<_> = graph.processor_ids().collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"proc1".into()));
    assert!(ids.contains(&"proc2".into()));
}

#[test]
fn test_graph_needs_recompile() {
    let mut graph = Graph::new();

    // Initially needs recompile (never compiled)
    assert!(graph.needs_recompile());

    // Mark as compiled
    graph.mark_compiled();
    assert!(!graph.needs_recompile());

    // Modify graph
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Now needs recompile
    assert!(graph.needs_recompile());
}

#[test]
fn test_graph_mark_compiled() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    assert!(graph.compiled_at().is_none());

    graph.mark_compiled();

    assert!(graph.compiled_at().is_some());
    assert!(!graph.needs_recompile());
}

#[test]
fn test_graph_state_transitions() {
    use crate::core::graph::GraphState;

    let mut graph = Graph::new();

    assert_eq!(graph.state(), GraphState::Idle);

    graph.set_state(GraphState::Running);
    assert_eq!(graph.state(), GraphState::Running);

    graph.set_state(GraphState::Paused);
    assert_eq!(graph.state(), GraphState::Paused);

    graph.set_state(GraphState::Stopping);
    assert_eq!(graph.state(), GraphState::Stopping);

    graph.set_state(GraphState::Idle);
    assert_eq!(graph.state(), GraphState::Idle);
}

// =============================================================================
// 4. Serialization Tests
// =============================================================================

#[test]
fn test_graph_to_json_includes_components() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Add state component
    graph
        .insert(&"proc1".into(), StateComponent::default())
        .unwrap();

    // Add metrics
    let mut metrics = ProcessorMetrics::default();
    metrics.throughput_fps = 60.0;
    graph.insert(&"proc1".into(), metrics).unwrap();

    let json = graph.to_json();

    // Verify processor exists with type
    assert_eq!(json["processors"]["proc1"]["type"], "TestProcessor");

    // Verify state component serialized
    assert!(json["processors"]["proc1"]["state"].is_string());

    // Verify metrics component serialized
    assert_eq!(
        json["processors"]["proc1"]["metrics"]["throughput_fps"],
        60.0
    );
}

#[test]
fn test_graph_to_json_handles_missing_components() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Don't add any components - just serialize
    let json = graph.to_json();

    // Processor should still be in output
    assert!(json["processors"]["proc1"].is_object());
    assert_eq!(json["processors"]["proc1"]["type"], "TestProcessor");

    // Component fields should be null/missing
    assert!(json["processors"]["proc1"]["state"].is_null());
    assert!(json["processors"]["proc1"]["metrics"].is_null());
}

#[test]
fn test_graph_to_dot_includes_state() {
    let mut graph = Graph::new();
    graph.add_processor("camera".into(), "CameraProcessor".into(), 0);

    // Add state component
    let state = StateComponent::default();
    *state.0.lock() = ProcessorState::Running;
    graph.insert(&"camera".into(), state).unwrap();

    let dot = graph.to_dot();

    // DOT should include processor
    assert!(dot.contains("\"camera\""));
    assert!(dot.contains("CameraProcessor"));

    // DOT should include state in label
    assert!(dot.contains("Running"));
}

// =============================================================================
// 5. Component Isolation Tests
// =============================================================================

#[test]
fn test_components_isolated_between_processors() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.add_processor("proc2".into(), "TestProcessor".into(), 0);

    // Insert different values for same component type
    graph.insert(&"proc1".into(), TestComponent(1)).unwrap();
    graph.insert(&"proc2".into(), TestComponent(2)).unwrap();

    // Each processor has its own value
    let comp1 = graph.get::<TestComponent>(&"proc1".into()).unwrap();
    let comp2 = graph.get::<TestComponent>(&"proc2".into()).unwrap();
    assert_eq!(comp1.0, 1);
    assert_eq!(comp2.0, 2);
}

#[test]
fn test_components_isolated_between_links() {
    let mut graph = Graph::new();
    graph.add_processor("source".into(), "SourceProc".into(), 0);
    graph.add_processor("mid".into(), "MidProc".into(), 0);
    graph.add_processor("sink".into(), "SinkProc".into(), 0);

    let link1 = graph.add_link("source.output", "mid.input").unwrap();
    let link2 = graph.add_link("mid.output", "sink.input").unwrap();

    // Insert different values
    graph.insert_link(&link1.id, TestComponent(1)).unwrap();
    graph.insert_link(&link2.id, TestComponent(2)).unwrap();

    // Each link has its own value
    let comp1 = graph
        .get_link_component::<TestComponent>(&link1.id)
        .unwrap();
    let comp2 = graph
        .get_link_component::<TestComponent>(&link2.id)
        .unwrap();
    assert_eq!(comp1.0, 1);
    assert_eq!(comp2.0, 2);
}

#[test]
fn test_processor_and_link_components_independent() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);
    graph.add_processor("proc2".into(), "TestProcessor".into(), 0);
    let link = graph.add_link("proc1.output", "proc2.input").unwrap();

    // Same component type on processor and link
    graph.insert(&"proc1".into(), TestComponent(100)).unwrap();
    graph.insert_link(&link.id, TestComponent(200)).unwrap();

    // They are independent
    let proc_comp = graph.get::<TestComponent>(&"proc1".into()).unwrap();
    let link_comp = graph.get_link_component::<TestComponent>(&link.id).unwrap();
    assert_eq!(proc_comp.0, 100);
    assert_eq!(link_comp.0, 200);
}

// =============================================================================
// 6. Error Handling Tests
// =============================================================================

#[test]
fn test_get_component_nonexistent_processor() {
    let graph = Graph::new();

    // Processor doesn't exist - should return None, not panic
    let result = graph.get::<TestComponent>(&"nonexistent".into());
    assert!(result.is_none());
}

#[test]
fn test_has_component_nonexistent_processor() {
    let graph = Graph::new();

    // Processor doesn't exist - should return false, not panic
    let result = graph.has::<TestComponent>(&"nonexistent".into());
    assert!(!result);
}

#[test]
fn test_remove_component_nonexistent_processor() {
    let mut graph = Graph::new();

    // Processor doesn't exist - should return None, not panic
    let result = graph.remove::<TestComponent>(&"nonexistent".into());
    assert!(result.is_none());
}

#[test]
fn test_get_link_component_nonexistent_link() {
    let graph = Graph::new();

    // Create a fake link ID
    let fake_link_id = crate::core::links::LinkUniqueId::from_string("nonexistent").unwrap();

    // Should return None, not panic
    let result = graph.get_link_component::<TestComponent>(&fake_link_id);
    assert!(result.is_none());
}

// =============================================================================
// 7. Thread Safety Tests (basic)
// =============================================================================

#[test]
fn test_shared_component_state_across_clones() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert pause gate (uses Arc internally)
    let gate = ProcessorPauseGate::new();
    let gate_clone = gate.clone();
    graph.insert(&"proc1".into(), gate).unwrap();

    // Modify via clone
    gate_clone.pause();

    // Verify change visible through graph
    let stored_gate = graph.get::<ProcessorPauseGate>(&"proc1".into()).unwrap();
    assert!(stored_gate.is_paused());
}

#[test]
fn test_state_component_arc_mutex_sharing() {
    let mut graph = Graph::new();
    graph.add_processor("proc1".into(), "TestProcessor".into(), 0);

    // Insert StateComponent
    let state = StateComponent::default();
    let state_arc = Arc::clone(&state.0);
    graph.insert(&"proc1".into(), state).unwrap();

    // Modify via external Arc
    *state_arc.lock() = ProcessorState::Running;

    // Verify change visible through graph
    let stored_state = graph.get::<StateComponent>(&"proc1".into()).unwrap();
    assert_eq!(*stored_state.0.lock(), ProcessorState::Running);
}
