// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{
    IntoLinkPortRef, Link, LinkDirection, LinkTraversalMut, TraversalSourceMut,
};
use crate::core::{Result, StreamError};

impl<'a> TraversalSourceMut<'a> {
    /// Add a new edge (link) between two ports.
    ///
    /// Accepts port addresses in "processor_id.port_name" format.
    pub fn add_e(
        self,
        from: impl IntoLinkPortRef,
        to: impl IntoLinkPortRef,
    ) -> Result<LinkTraversalMut<'a>> {
        // 1. Parse port references with default directions
        let from_ref = from.into_link_port_ref(LinkDirection::Output)?;
        let to_ref = to.into_link_port_ref(LinkDirection::Input)?;

        // 2. Validate directions
        if from_ref.direction != LinkDirection::Output {
            return Err(StreamError::InvalidLink(format!(
                "Source port '{}' must be an output, not an input",
                from_ref.to_address()
            )));
        }
        if to_ref.direction != LinkDirection::Input {
            return Err(StreamError::InvalidLink(format!(
                "Target port '{}' must be an input, not an output",
                to_ref.to_address()
            )));
        }

        // 3. Find source and target node indices
        let from_idx = self
            .graph
            .node_indices()
            .find(|&idx| self.graph[idx].id.as_str() == from_ref.processor_id.as_str())
            .ok_or_else(|| {
                StreamError::ProcessorNotFound(from_ref.processor_id.to_string())
            })?;

        let to_idx = self
            .graph
            .node_indices()
            .find(|&idx| self.graph[idx].id.as_str() == to_ref.processor_id.as_str())
            .ok_or_else(|| {
                StreamError::ProcessorNotFound(to_ref.processor_id.to_string())
            })?;

        // 4. Validate ports exist on the processors
        let from_node = &self.graph[from_idx];
        if !from_node.has_output(&from_ref.port_name) {
            return Err(StreamError::InvalidLink(format!(
                "Processor '{}' has no output port '{}'. Available: {:?}",
                from_ref.processor_id,
                from_ref.port_name,
                from_node
                    .ports
                    .outputs
                    .iter()
                    .map(|p| &p.name)
                    .collect::<Vec<_>>()
            )));
        }

        let to_node = &self.graph[to_idx];
        if !to_node.has_input(&to_ref.port_name) {
            return Err(StreamError::InvalidLink(format!(
                "Processor '{}' has no input port '{}'. Available: {:?}",
                to_ref.processor_id,
                to_ref.port_name,
                to_node
                    .ports
                    .inputs
                    .iter()
                    .map(|p| &p.name)
                    .collect::<Vec<_>>()
            )));
        }

        // 5. Create link and add edge
        let link = Link::new(&from_ref.to_address(), &to_ref.to_address());
        let edge_idx = self.graph.add_edge(from_idx, to_idx, link);

        // 6. Return traversal with new edge
        Ok(LinkTraversalMut {
            graph: self.graph,
            ids: vec![edge_idx],
        })
    }
}
