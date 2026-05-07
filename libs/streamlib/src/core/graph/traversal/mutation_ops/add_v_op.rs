// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{ProcessorNode, ProcessorTraversalMut, TraversalSourceMut};
use crate::core::processors::{ProcessorSpec, PROCESSOR_REGISTRY};

impl<'a> TraversalSourceMut<'a> {
    /// Add a new processor node to the graph.
    ///
    /// Returns a traversal with the new node, or an empty traversal if creation failed.
    pub fn add_v(self, spec: ProcessorSpec) -> ProcessorTraversalMut<'a> {
        // Lookup port info from global registry
        let Some((inputs, outputs)) = PROCESSOR_REGISTRY.port_info(&spec.name) else {
            tracing::warn!("Processor '{}' not found in registry", spec.name);
            return ProcessorTraversalMut {
                graph: self.graph,
                ids: vec![],
            };
        };

        // Resolve display_name: override if provided, otherwise default to the
        // PascalCase short-name segment of the structured ident (`SchemaIdent`'s
        // joined `Display` form is too verbose for UIs).
        let display_name = spec
            .display_name
            .unwrap_or_else(|| spec.name.r#type.as_str().to_string());

        // Build ProcessorNode with resolved port info
        let node = ProcessorNode::new(spec.name, display_name, Some(spec.config), inputs, outputs);

        // Add to graph
        let node_idx = self.graph.add_node(node);

        ProcessorTraversalMut {
            graph: self.graph,
            ids: vec![node_idx],
        }
    }
}
