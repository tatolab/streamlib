// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph data structure tests using only the traversal API.
//!
//! Tests verify Graph operates as a standalone data structure.
//! MockProcessor implements the Processor trait to work with add_v.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value as JsonValue};

use crate::core::graph::{Graph, GraphEdgeWithComponents, GraphNodeWithComponents};
use crate::core::processors::ProcessorState;
use crate::core::JsonSerializableComponent;

// =============================================================================
// Mock Processor and Config
// =============================================================================

/// Mock processor for testing graph operations.
#[crate::processor("schemas/processors/test/mock_processor.yaml")]
struct MockProcessor;

impl crate::core::ManualProcessor for MockProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: crate::core::context::RuntimeContext,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn teardown(
        &mut self,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn start(&mut self) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Processor with only output ports.
#[crate::processor("schemas/processors/test/mock_output_only_processor.yaml")]
struct MockOutputOnlyProcessor;

impl crate::core::ManualProcessor for MockOutputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: crate::core::context::RuntimeContext,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn teardown(
        &mut self,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn start(&mut self) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Processor with only input ports.
#[crate::processor("schemas/processors/test/mock_input_only_processor.yaml")]
struct MockInputOnlyProcessor;

impl crate::core::ManualProcessor for MockInputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: crate::core::context::RuntimeContext,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn teardown(
        &mut self,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn start(&mut self) -> crate::core::error::Result<()> {
        Ok(())
    }
}

// =============================================================================
// Mock Components (for component storage tests)
// =============================================================================

/// Mock state component.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MockState(ProcessorState);

impl Default for MockState {
    fn default() -> Self {
        Self(ProcessorState::Idle)
    }
}

impl JsonSerializableComponent for MockState {
    fn json_key(&self) -> &'static str {
        "mock_state"
    }

    fn to_json(&self) -> JsonValue {
        json!({ "state": format!("{:?}", self.0) })
    }
}

/// Mock processor instance component.
struct MockProcessorInstance;

impl JsonSerializableComponent for MockProcessorInstance {
    fn json_key(&self) -> &'static str {
        "mock_processor_instance"
    }

    fn to_json(&self) -> JsonValue {
        json!({ "type": "MockProcessorInstance" })
    }
}

/// Mock pause gate component.
struct MockPauseGate {
    paused: Arc<AtomicBool>,
}

impl MockPauseGate {
    fn new() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
        }
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}

impl Clone for MockPauseGate {
    fn clone(&self) -> Self {
        Self {
            paused: Arc::clone(&self.paused),
        }
    }
}

impl JsonSerializableComponent for MockPauseGate {
    fn json_key(&self) -> &'static str {
        "mock_pause_gate"
    }

    fn to_json(&self) -> JsonValue {
        json!({ "paused": self.is_paused() })
    }
}

/// Mock link instance component.
struct MockLinkInstance {
    capacity: usize,
}

impl MockLinkInstance {
    fn new(capacity: usize) -> Self {
        Self { capacity }
    }
}

impl JsonSerializableComponent for MockLinkInstance {
    fn json_key(&self) -> &'static str {
        "mock_link_instance"
    }

    fn to_json(&self) -> JsonValue {
        json!({ "capacity": self.capacity })
    }
}

/// Counter component for mutation tests.
struct CounterComponent(i32);

impl JsonSerializableComponent for CounterComponent {
    fn json_key(&self) -> &'static str {
        "counter"
    }

    fn to_json(&self) -> JsonValue {
        json!({ "value": self.0 })
    }
}

// =============================================================================
// 1. Basic Query Operations
// =============================================================================

mod query_ops {
    use super::*;

    #[test]
    fn test_v_all_returns_all_processors() {
        let mut graph = Graph::new();

        graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()));
        graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()));
        graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()));

        let ids = graph.traversal().v(()).ids();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn test_v_by_id_returns_specific_processor() {
        let mut graph = Graph::new();

        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()));

        let found = graph.traversal().v(id.as_str()).first();
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().processor_type,
            "com.streamlib.test.mock_processor"
        );
    }

    #[test]
    fn test_v_nonexistent_returns_empty() {
        let graph = Graph::new();

        let found = graph.traversal().v("nonexistent").first();
        assert!(found.is_none());
    }

    #[test]
    fn test_exists_true_when_found() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        assert!(graph.traversal().v(id.as_str()).exists());
    }

    #[test]
    fn test_exists_false_when_not_found() {
        let graph = Graph::new();

        assert!(!graph.traversal().v("nonexistent").exists());
    }

    #[test]
    fn test_ids_returns_all_processor_ids() {
        let mut graph = Graph::new();

        let id1 = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let id2 = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        let ids = graph.traversal().v(()).ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().any(|id| id.as_str() == id1));
        assert!(ids.iter().any(|id| id.as_str() == id2));
    }

    #[test]
    fn test_first_returns_some_processor() {
        let mut graph = Graph::new();
        graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()));

        let first = graph.traversal().v(()).first();
        assert!(first.is_some());
    }

    #[test]
    fn test_first_on_empty_returns_none() {
        let graph = Graph::new();

        assert!(graph.traversal().v(()).first().is_none());
    }

    #[test]
    fn test_iter_yields_all_processors() {
        let mut graph = Graph::new();

        graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()));
        graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()));
        graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()));

        let types: Vec<_> = graph
            .traversal()
            .v(())
            .iter()
            .map(|n| n.processor_type.clone())
            .collect();

        assert_eq!(types.len(), 3);
        assert!(types.contains(&"com.streamlib.test.mock_processor".to_string()));
        assert!(types.contains(&"com.streamlib.test.mock_output_only_processor".to_string()));
        assert!(types.contains(&"com.streamlib.test.mock_input_only_processor".to_string()));
    }
}

// =============================================================================
// 2. Edge (Link) Query Operations
// =============================================================================

mod edge_query_ops {
    use super::*;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

    #[test]
    fn test_e_all_returns_all_links() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream1_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream2_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream_id, "out1"),
            InputLinkPortRef::new(&downstream1_id, "in1"),
        );
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream_id, "out2"),
            InputLinkPortRef::new(&downstream2_id, "in1"),
        );

        let link_ids = graph.traversal().e(()).ids();
        assert_eq!(link_ids.len(), 2);
    }

    #[test]
    fn test_e_by_id_returns_specific_link() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&upstream_id, "out1"),
                InputLinkPortRef::new(&downstream_id, "in1"),
            )
            .first()
            .expect("link should be created")
            .id
            .to_string();

        let found = graph.traversal().e(link_id.as_str()).first();
        assert!(found.is_some());
    }

    #[test]
    fn test_link_from_port_and_to_port() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&upstream_id, "out1"),
                InputLinkPortRef::new(&downstream_id, "in1"),
            )
            .first()
            .expect("link should be created")
            .id
            .to_string();

        let link = graph.traversal().e(link_id.as_str()).first().unwrap();

        assert_eq!(link.from_port().processor_id.as_str(), upstream_id);
        assert_eq!(link.from_port().port_name, "out1");
        assert_eq!(link.to_port().processor_id.as_str(), downstream_id);
        assert_eq!(link.to_port().port_name, "in1");
    }
}

// =============================================================================
// 3. Filtering Operations
// =============================================================================

mod filter_ops {
    use super::*;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

    #[test]
    fn test_filter_by_processor_type() {
        let mut graph = Graph::new();

        graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()));
        graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()));
        graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()));

        let mock_processors: Vec<_> = graph
            .traversal()
            .v(())
            .filter(|n| n.processor_type == "com.streamlib.test.mock_processor")
            .ids();

        assert_eq!(mock_processors.len(), 2);
    }

    #[test]
    fn test_has_component_filters_correctly() {
        let mut graph = Graph::new();

        let id1 = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()));

        graph
            .traversal_mut()
            .v(id1.as_str())
            .first_mut()
            .unwrap()
            .insert(MockState::default());

        let with_state: Vec<_> = graph.traversal().v(()).has_component::<MockState>().ids();

        assert_eq!(with_state.len(), 1);
        assert_eq!(with_state[0].as_str(), id1);
    }

    #[test]
    fn test_filter_links_by_destination() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream1_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream2_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream_id, "out1"),
            InputLinkPortRef::new(&downstream1_id, "in1"),
        );
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream_id, "out2"),
            InputLinkPortRef::new(&downstream2_id, "in1"),
        );

        let to_downstream1: Vec<_> = graph
            .traversal()
            .e(())
            .filter(|link| link.to_port().processor_id.as_str() == downstream1_id)
            .ids();

        assert_eq!(to_downstream1.len(), 1);
    }
}

// =============================================================================
// 4. Component Operations
// =============================================================================

mod component_ops {
    use super::*;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

    #[test]
    fn test_insert_and_get_component() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        graph
            .traversal_mut()
            .v(id.as_str())
            .first_mut()
            .unwrap()
            .insert(MockState(ProcessorState::Running));

        let state = graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .get::<MockState>();

        assert!(state.is_some());
        assert_eq!(state.unwrap().0, ProcessorState::Running);
    }

    #[test]
    fn test_has_component() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        assert!(!graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .has::<MockState>());

        graph
            .traversal_mut()
            .v(id.as_str())
            .first_mut()
            .unwrap()
            .insert(MockState::default());

        assert!(graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .has::<MockState>());
    }

    #[test]
    fn test_remove_component() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        graph
            .traversal_mut()
            .v(id.as_str())
            .first_mut()
            .unwrap()
            .insert(MockState(ProcessorState::Running));

        let removed = graph
            .traversal_mut()
            .v(id.as_str())
            .first_mut()
            .unwrap()
            .remove::<MockState>();

        assert!(removed.is_some());
        assert_eq!(removed.unwrap().0, ProcessorState::Running);
        assert!(!graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .has::<MockState>());
    }

    #[test]
    fn test_multiple_components_on_same_node() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        {
            let node = graph.traversal_mut().v(id.as_str()).first_mut().unwrap();
            node.insert(MockState(ProcessorState::Running));
            node.insert(MockProcessorInstance);
            node.insert(CounterComponent(42));
        }

        let node = graph.traversal().v(id.as_str()).first().unwrap();
        assert!(node.has::<MockState>());
        assert!(node.has::<MockProcessorInstance>());
        assert!(node.has::<CounterComponent>());
        assert_eq!(node.get::<CounterComponent>().unwrap().0, 42);
    }

    #[test]
    fn test_link_components() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&upstream_id, "out1"),
                InputLinkPortRef::new(&downstream_id, "in1"),
            )
            .first()
            .expect("link should be created")
            .id
            .to_string();

        graph
            .traversal_mut()
            .e(link_id.as_str())
            .first_mut()
            .unwrap()
            .insert(MockLinkInstance::new(8));

        let instance = graph
            .traversal()
            .e(link_id.as_str())
            .first()
            .unwrap()
            .get::<MockLinkInstance>();

        assert!(instance.is_some());
        assert_eq!(instance.unwrap().capacity, 8);
    }
}

// =============================================================================
// 5. Mutation Persistence Tests
// =============================================================================

mod mutation_persistence {
    use super::*;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

    #[test]
    fn test_component_mutation_persists() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        graph
            .traversal_mut()
            .v(id.as_str())
            .first_mut()
            .unwrap()
            .insert(MockState(ProcessorState::Idle));

        {
            let node = graph.traversal_mut().v(id.as_str()).first_mut().unwrap();
            if let Some(state) = node.get_mut::<MockState>() {
                state.0 = ProcessorState::Running;
            }
        }

        let state = graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .get::<MockState>()
            .unwrap();

        assert_eq!(state.0, ProcessorState::Running);
    }

    #[test]
    fn test_arc_shared_component_modification() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        let gate = MockPauseGate::new();
        let gate_clone = gate.clone();

        graph
            .traversal_mut()
            .v(id.as_str())
            .first_mut()
            .unwrap()
            .insert(gate);

        gate_clone.pause();

        let stored_gate = graph
            .traversal()
            .v(id.as_str())
            .first()
            .unwrap()
            .get::<MockPauseGate>()
            .unwrap();

        assert!(stored_gate.is_paused());
    }

    #[test]
    fn test_drop_removes_processor() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        assert!(graph.traversal().v(id.as_str()).exists());

        graph.traversal_mut().v(id.as_str()).drop();

        assert!(!graph.traversal().v(id.as_str()).exists());
    }

    #[test]
    fn test_drop_removes_link() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&upstream_id, "out1"),
                InputLinkPortRef::new(&downstream_id, "in1"),
            )
            .first()
            .expect("link should be created")
            .id
            .to_string();

        assert!(graph.traversal().e(link_id.as_str()).exists());

        graph.traversal_mut().e(link_id.as_str()).drop();

        assert!(!graph.traversal().e(link_id.as_str()).exists());
    }
}

// =============================================================================
// 6. Real-World Scenario Tests
// =============================================================================

mod real_world_scenarios {
    use super::*;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

    #[test]
    fn test_compiler_create_phase_pattern() {
        let mut graph = Graph::new();
        let id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        {
            let node = graph.traversal_mut().v(id.as_str()).first_mut().unwrap();
            node.insert(MockProcessorInstance);
            node.insert(MockState::default());
            node.insert(MockPauseGate::new());
        }

        let node = graph.traversal().v(id.as_str()).first().unwrap();
        assert!(node.has::<MockProcessorInstance>());
        assert!(node.has::<MockState>());
        assert!(node.has::<MockPauseGate>());
    }

    #[test]
    fn test_compiler_wire_phase_pattern() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&upstream_id, "out1"),
                InputLinkPortRef::new(&downstream_id, "in1"),
            )
            .first()
            .expect("link should be created")
            .id
            .to_string();

        {
            let link = graph
                .traversal_mut()
                .e(link_id.as_str())
                .first_mut()
                .unwrap();
            link.insert(MockLinkInstance::new(4));
        }

        let link = graph.traversal().e(link_id.as_str()).first().unwrap();
        assert!(link.has::<MockLinkInstance>());
    }

    #[test]
    fn test_runtime_pause_all_pattern() {
        let mut graph = Graph::new();

        let id1 = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let id2 = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let id3 = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        for id in [&id1, &id2, &id3] {
            graph
                .traversal_mut()
                .v(id.as_str())
                .first_mut()
                .unwrap()
                .insert(MockPauseGate::new());
        }

        let ids: Vec<_> = graph.traversal().v(()).ids();
        for id in &ids {
            let node = graph.traversal_mut().v(id.as_str()).first_mut().unwrap();
            if let Some(gate) = node.get::<MockPauseGate>() {
                gate.pause();
            }
        }

        for id in &ids {
            let gate = graph
                .traversal()
                .v(id.as_str())
                .first()
                .unwrap()
                .get::<MockPauseGate>()
                .unwrap();
            assert!(gate.is_paused());
        }
    }

    #[test]
    fn test_runtime_status_pattern() {
        let mut graph = Graph::new();

        let id1 = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let id2 = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()));

        graph
            .traversal_mut()
            .v(id1.as_str())
            .first_mut()
            .unwrap()
            .insert(MockState(ProcessorState::Running));

        graph
            .traversal_mut()
            .v(id2.as_str())
            .first_mut()
            .unwrap()
            .insert(MockState(ProcessorState::Paused));

        let processor_states: Vec<(String, ProcessorState)> = graph
            .traversal()
            .v(())
            .has_component::<MockState>()
            .iter()
            .filter_map(|node| {
                let state = node.get::<MockState>()?;
                Some((node.id.to_string(), state.0))
            })
            .collect();

        assert_eq!(processor_states.len(), 2);

        let states_map: std::collections::HashMap<_, _> = processor_states.into_iter().collect();
        assert_eq!(states_map.get(&id1), Some(&ProcessorState::Running));
        assert_eq!(states_map.get(&id2), Some(&ProcessorState::Paused));
    }

    #[test]
    fn test_pipeline_topology() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let middle_id = graph
            .traversal_mut()
            .add_v(MockProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream_id, "out1"),
            InputLinkPortRef::new(&middle_id, "in1"),
        );
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&middle_id, "out1"),
            InputLinkPortRef::new(&downstream_id, "in1"),
        );

        assert_eq!(graph.traversal().v(()).ids().len(), 3);
        assert_eq!(graph.traversal().e(()).ids().len(), 2);
    }
}

// =============================================================================
// 7. Edge Navigation Tests
// =============================================================================

mod edge_navigation {
    use super::*;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};

    #[test]
    fn test_out_e_returns_outgoing_links() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream1_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream2_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream_id, "out1"),
            InputLinkPortRef::new(&downstream1_id, "in1"),
        );
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream_id, "out2"),
            InputLinkPortRef::new(&downstream2_id, "in1"),
        );

        let outgoing = graph.traversal().v(upstream_id.as_str()).out_e().ids();
        assert_eq!(outgoing.len(), 2);
    }

    #[test]
    fn test_in_e_returns_incoming_links() {
        let mut graph = Graph::new();

        let upstream1_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let upstream2_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();

        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream1_id, "out1"),
            InputLinkPortRef::new(&downstream_id, "in1"),
        );
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&upstream2_id, "out1"),
            InputLinkPortRef::new(&downstream_id, "in2"),
        );

        let incoming = graph.traversal().v(downstream_id.as_str()).in_e().ids();
        assert_eq!(incoming.len(), 2);
    }

    #[test]
    fn test_out_v_returns_upstream_processor() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&upstream_id, "out1"),
                InputLinkPortRef::new(&downstream_id, "in1"),
            )
            .first()
            .expect("link should be created")
            .id
            .to_string();

        // out_v = vertex the edge comes FROM (upstream)
        let upstream_processors = graph.traversal().e(link_id.as_str()).out_v().ids();
        assert_eq!(upstream_processors.len(), 1);
        assert_eq!(upstream_processors[0].as_str(), upstream_id);
    }

    #[test]
    fn test_in_v_returns_downstream_processor() {
        let mut graph = Graph::new();

        let upstream_id = graph
            .traversal_mut()
            .add_v(MockOutputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let downstream_id = graph
            .traversal_mut()
            .add_v(MockInputOnlyProcessor::Processor::node(Default::default()))
            .first()
            .expect("should create processor")
            .id
            .to_string();
        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&upstream_id, "out1"),
                InputLinkPortRef::new(&downstream_id, "in1"),
            )
            .first()
            .expect("link should be created")
            .id
            .to_string();

        // in_v = vertex the edge goes INTO (downstream)
        let downstream_processors = graph.traversal().e(link_id.as_str()).in_v().ids();
        assert_eq!(downstream_processors.len(), 1);
        assert_eq!(downstream_processors[0].as_str(), downstream_id);
    }
}
