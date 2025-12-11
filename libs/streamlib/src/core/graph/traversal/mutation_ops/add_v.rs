// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{PortInfo, ProcessorNode, ProcessorTraversalMut, TraversalSourceMut};
use crate::core::processors::{Config, Processor};
use crate::core::{Result, StreamError};

impl<'a> TraversalSourceMut<'a> {
    pub fn add_v<P>(self, config: P::Config) -> Result<ProcessorTraversalMut<'a>>
    where
        P: Processor + 'static,
    {
        // 1. Validate config round-trips through JSON (catches #[serde(skip)] fields)
        config
            .validate_round_trip()
            .map_err(|e| StreamError::Config(e.to_string()))?;

        // 2. Get processor descriptor
        let descriptor = P::descriptor().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!(
                "Processor {} has no descriptor",
                std::any::type_name::<P>()
            ))
        })?;

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
        Ok(ProcessorTraversalMut {
            graph: self.graph,
            ids: vec![node_idx],
        })
    }
}
