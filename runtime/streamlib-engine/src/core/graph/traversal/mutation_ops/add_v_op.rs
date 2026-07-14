// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::graph::{
    GraphNodeWithComponents, ProcessorNode, ProcessorTraversalMut, StateComponent,
    TraversalSourceMut,
};
use crate::core::processors::{
    PROCESSOR_REGISTRY, ProcessorSpec, ProcessorState, ProcessorTypeReference,
};

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
        // Resolve the reference to the concrete registered ident + its ports.
        // `VersionPinned` matches version-exact (the #1325 path); a version-free
        // `ResolveToInstalled` resolves `(org, package, type)` to the single
        // installed provider. Both gate on `port_info` presence — every
        // registered processor has a `port_info` entry (subprocess-only
        // descriptors register empty port lists), so both variants resolve any
        // registered type and miss only a genuinely-unregistered one.
        let resolved = match &spec.name {
            ProcessorTypeReference::VersionPinned(ident) => PROCESSOR_REGISTRY
                .port_info(ident)
                .map(|ports| (ident.clone(), ports)),
            ProcessorTypeReference::ResolveToInstalled {
                org,
                package,
                r#type,
            } => PROCESSOR_REGISTRY
                .resolve_installed_processor_type(org, package, r#type)
                .and_then(|ident| {
                    PROCESSOR_REGISTRY
                        .port_info(&ident)
                        .map(|ports| (ident, ports))
                }),
        };

        let registry_miss = resolved.is_none();

        if registry_miss {
            tracing::error!(
                "Processor type '{}' is not registered — node added in Error state and will not be compiled",
                spec.name
            );
        }

        // On a miss, build the failed node with the reference's diagnostic
        // ident (concrete for `VersionPinned`; `(org, package, type)@0.0.0` for
        // a version-free reference) so it stays visible via `GET /api/graph`.
        let (node_ident, (inputs, outputs)) =
            resolved.unwrap_or_else(|| (spec.name.to_diagnostic_ident(), (vec![], vec![])));

        let display_name = spec
            .display_name
            .unwrap_or_else(|| node_ident.r#type.as_str().to_string());

        let node = ProcessorNode::new(node_ident, display_name, Some(spec.config), inputs, outputs);

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
