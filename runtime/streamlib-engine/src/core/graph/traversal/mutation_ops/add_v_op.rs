// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use petgraph::graph::DiGraph;

use crate::core::graph::{
    GraphNodeWithComponents, Link, ProcessorNode, ProcessorTraversalMut, StateComponent,
    TraversalSourceMut,
};
use crate::core::processors::{
    PROCESSOR_REGISTRY, ProcessorSpec, ProcessorState,
};

impl<'a> TraversalSourceMut<'a> {
    /// Add a new processor node to the graph.
    ///
    /// The node carries a [`StateComponent`] from here on — `Pending`, or
    /// `Error` on a registry miss.
    ///
    /// On registry miss, the node is still added with empty ports. The caller
    /// (typically `add_processor_impl`) should detect this and surface
    /// `Error::UnknownProcessorType`. Leaving the failed node in the graph
    /// gives API consumers (`GET /api/graph`) visibility of what failed and
    /// why — runtime-dynamic systems prefer "load-and-mark-failed" over
    /// "silently-skip" so observability survives the misconfiguration.
    pub fn add_v(self, spec: ProcessorSpec) -> ProcessorTraversalMut<'a> {
        // Resolve `(org, package, type)` to the installed provider's registered
        // ident + its ports. Gated on `port_info` presence — every registered
        // processor has an entry (subprocess-only descriptors register empty
        // port lists), so this resolves any registered type and misses only a
        // genuinely-unregistered one.
        let resolved = PROCESSOR_REGISTRY
            .resolve_installed_processor_type(
                spec.name.org(),
                spec.name.package(),
                spec.name.r#type(),
            )
            .and_then(|ident| {
                PROCESSOR_REGISTRY
                    .port_info(&ident)
                    .map(|ports| (ident, ports))
            });

        let registry_miss = resolved.is_none();

        if registry_miss {
            tracing::error!(
                "Processor type '{}' is not registered — node added in Error state and will not be compiled",
                spec.name
            );
        }

        // On a miss, build the failed node with the reference's diagnostic
        // ident (`(org, package, type)@0.0.0`) so it stays visible via
        // `GET /api/graph`.
        let (node_ident, (inputs, outputs)) =
            resolved.unwrap_or_else(|| (spec.name.to_diagnostic_ident(), (vec![], vec![])));

        let requested_display_name = spec
            .display_name
            .unwrap_or_else(|| node_ident.r#type.as_str().to_string());
        let display_name =
            disambiguate_display_name_within_graph(self.graph, requested_display_name);

        let node = ProcessorNode::new(node_ident, display_name, Some(spec.config), inputs, outputs);

        let node_idx = self.graph.add_node(node);

        // Attached here rather than when the compiler prepares the node, so a
        // processor has an observable state for its whole life in the graph:
        // a reader waiting for the graph to come up can hold every processor's
        // state before `start()` has prepared any of them.
        if let Some(node_mut) = self.graph.node_weight_mut(node_idx) {
            node_mut.insert(StateComponent::new(if registry_miss {
                ProcessorState::Error
            } else {
                ProcessorState::Pending
            }));
        }

        ProcessorTraversalMut {
            graph: self.graph,
            ids: vec![node_idx],
        }
    }
}

/// Make `requested_display_name` unique among the graph's nodes by appending
/// ` 2`, ` 3` … until nothing else answers to it.
///
/// The spelling is a contract, not a formatting choice: the same string is what
/// the `add` handle reports, what `streamlib graph` renders, and what prefixes
/// the processor's log records, so it must be identical in every language.
/// Applied to an author-supplied name as well as to the type-derived default —
/// two `add(Camera, display_name = "Front")` calls are as ambiguous as two
/// defaults.
fn disambiguate_display_name_within_graph(
    graph: &DiGraph<ProcessorNode, Link>,
    requested_display_name: String,
) -> String {
    let is_taken = |candidate: &str| {
        graph
            .node_weights()
            .any(|node| node.display_name == candidate)
    };

    if !is_taken(&requested_display_name) {
        return requested_display_name;
    }

    let mut ordinal = 2usize;
    loop {
        let candidate = format!("{requested_display_name} {ordinal}");
        if !is_taken(&candidate) {
            return candidate;
        }
        ordinal += 1;
    }
}
