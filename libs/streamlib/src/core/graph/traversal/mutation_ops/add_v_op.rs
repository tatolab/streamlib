// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{PortInfo, ProcessorNode, ProcessorTraversalMut, TraversalSourceMut};
use crate::core::processors::{Config, Processor};

impl<'a> TraversalSourceMut<'a> {
    /// Add a new processor node to the graph.
    ///
    /// Returns a traversal with the new node, or an empty traversal if creation failed
    /// (e.g., config validation failed, no descriptor).
    pub fn add_v<P>(self, config: P::Config) -> ProcessorTraversalMut<'a>
    where
        P: Processor + 'static,
    {
        // 1. Validate config round-trips through JSON (catches #[serde(skip)] fields)
        if config.validate_round_trip().is_err() {
            return ProcessorTraversalMut {
                graph: self.graph,
                ids: vec![],
            };
        }

        // 2. Get processor descriptor
        let Some(descriptor) = P::descriptor() else {
            return ProcessorTraversalMut {
                graph: self.graph,
                ids: vec![],
            };
        };

        // 3. Build port info from descriptor
        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.name.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.name.clone(),
                port_kind: Default::default(),
            })
            .collect();

        // 4. Serialize config
        let config_json = serde_json::to_value(&config).ok();

        // 5. Create ProcessorNode (handles its own ID generation)
        let node = ProcessorNode::new(descriptor.name.clone(), config_json, inputs, outputs);

        // 6. Add to graph
        let node_idx = self.graph.add_node(node);

        // 7. Return traversal with new node
        ProcessorTraversalMut {
            graph: self.graph,
            ids: vec![node_idx],
        }
    }
}
