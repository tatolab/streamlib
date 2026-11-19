// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::graph::{Graph, GraphEdgeWithComponents, GraphNodeWithComponents, GraphRenders};
use crate::core::json_schema::{GraphLinkComponentsOnTheWire, GraphNodeComponentsOnTheWire};
use crate::core::nodes::NodeState;
use crate::core::test_support::{
    MockInputOnlyNode, MockNode, MockOutputOnlyNode, ensure_test_mocks_registered,
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
struct MockState(NodeState);

impl Default for MockState {
    fn default() -> Self {
        Self(NodeState::Idle)
    }
}

struct MockNodeInstance;

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

/// Mock link instance component.
struct MockLinkInstance {
    capacity: usize,
}

impl MockLinkInstance {
    fn new(capacity: usize) -> Self {
        Self { capacity }
    }
}

/// Counter component for mutation tests.
struct CounterComponent(i32);

/// Each mock `graph` renders under no key of `$onto`: these tests read the component itself.
macro_rules! mocks_render_under_no_key {
    ($onto:ty: $($mock:ty),+) => {
        $(impl GraphRenders<$onto> for $mock {
            fn render_onto(&self, _components: &mut $onto) {}
        })+
    };
}

mocks_render_under_no_key!(
    GraphNodeComponentsOnTheWire: MockState,
    MockNodeInstance,
    MockPauseGate,
    CounterComponent
);
mocks_render_under_no_key!(GraphLinkComponentsOnTheWire: MockLinkInstance);
