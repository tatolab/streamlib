// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::graph::{
    GraphNodeWithComponents, ProcessorNode, ProcessorTraversalMut, StateComponent,
    TraversalSourceMut,
};
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec, ProcessorState};

impl<'a> TraversalSourceMut<'a> {
    /// Add a new processor node to the graph.
    ///
    /// On registry miss, the node is still added with empty ports and its
    /// `StateComponent` initialized to `ProcessorState::Error`. The caller
    /// (typically `add_processor_impl`) should detect this and surface
    /// `Error::UnknownProcessorType`. Leaving the failed node in the graph
    /// gives API consumers (`GET /api/graph`) visibility of what failed and
    /// why — runtime-dynamic systems prefer "load-and-mark-failed" over
    /// "silently-skip" so observability survives the misconfiguration.
    pub fn add_v(self, spec: ProcessorSpec) -> ProcessorTraversalMut<'a> {
        let port_info = PROCESSOR_REGISTRY.port_info(&spec.name);
        let registry_miss = port_info.is_none();

        if registry_miss {
            tracing::error!(
                "Processor type '{}' is not registered — node added in Error state and will not be compiled",
                spec.name
            );
        }

        let (inputs, outputs) = port_info.unwrap_or_else(|| (vec![], vec![]));

        let display_name = spec
            .display_name
            .unwrap_or_else(|| spec.name.r#type.as_str().to_string());

        let node = ProcessorNode::new(spec.name, display_name, Some(spec.config), inputs, outputs);

        let node_idx = self.graph.add_node(node);

        if registry_miss {
            if let Some(node_mut) = self.graph.node_weight_mut(node_idx) {
                node_mut.insert(StateComponent(Arc::new(Mutex::new(ProcessorState::Error))));
            }
        }

        ProcessorTraversalMut {
            graph: self.graph,
            ids: vec![node_idx],
        }
    }
}
