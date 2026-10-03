// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod components;
mod data_structure;

mod edges;
mod exposed_name_cast;
mod graph_readiness;
mod nodes;
mod processor_state_ecs_component;
mod traits;
mod traversal;
mod validation;

#[cfg(test)]
mod graph_tests;

// top level
pub use data_structure::{Graph, GraphState};
pub use exposed_name_cast::{cast_exposed_name_to_url_safe, EXPOSED_NAME_MAXIMUM_LENGTH};
pub use graph_readiness::ObservableGraphReadiness;
pub use processor_state_ecs_component::{ProcessorState, ProcessorStateComponent};
pub use traits::{GraphEdgeWithComponents, GraphNodeWithComponents, GraphWeight};
pub use validation::validate_graph;

pub use components::*;
pub use edges::*;
pub use nodes::*;
pub use traversal::*;
