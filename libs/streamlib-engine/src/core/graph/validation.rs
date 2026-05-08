// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::{Result, StreamError};
use crate::core::graph::{Link, ProcessorNode};
use petgraph::algo::is_cyclic_directed;
use petgraph::graph::DiGraph;

/// Validate graph structure
pub fn validate_graph(graph: &DiGraph<ProcessorNode, Link>) -> Result<()> {
    // Check for cycles
    if is_cyclic_directed(graph) {
        return Err(StreamError::InvalidGraph("Graph contains cycles".into()));
    }

    // Future validation:
    // - Check port types match
    // - Check all connections reference valid ports
    // - etc.

    Ok(())
}
