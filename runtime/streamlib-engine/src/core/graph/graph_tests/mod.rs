// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value as JsonValue, json};

use crate::core::JsonSerializableComponent;
use crate::core::graph::{Graph, GraphEdgeWithComponents, GraphNodeWithComponents};
use crate::core::processors::ProcessorState;
use crate::core::test_support::{
    MockInputOnlyProcessor, MockOutputOnlyProcessor, MockProcessor, ensure_test_mocks_registered,
};

mod component_ops;
mod edge_navigation;
mod edge_query_ops;
mod filter_ops;
mod mutation_persistence;
mod node_names;
mod query_ops;
mod real_world_scenarios;

fn test_graph() -> Graph {
    ensure_test_mocks_registered();
    Graph::new()
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
