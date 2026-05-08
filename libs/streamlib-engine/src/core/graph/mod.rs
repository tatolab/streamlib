// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod components;
mod data_structure;

mod edges;
mod nodes;
mod traits;
mod traversal;
mod validation;

#[cfg(test)]
mod graph_tests;

// top level
pub use data_structure::{Graph, GraphState};
pub use traits::{GraphEdgeWithComponents, GraphNodeWithComponents, GraphWeight};
pub use validation::validate_graph;

pub use components::*;
pub use edges::*;
pub use nodes::*;
pub use traversal::*;
