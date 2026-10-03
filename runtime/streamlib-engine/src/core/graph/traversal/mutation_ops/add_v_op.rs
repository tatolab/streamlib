// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use petgraph::graph::DiGraph;

use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    EXPOSED_NAME_MAXIMUM_LENGTH, GraphNodeWithComponents, Link, ProcessorNode,
    ProcessorTraversalMut, StateComponent, TraversalSourceMut, cast_exposed_name_to_url_safe,
};
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec, ProcessorState};

impl<'a> TraversalSourceMut<'a> {
    /// Add a new processor node to the graph, named by `the_name_a_new_node_takes`.
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
    pub fn add_v(self, spec: ProcessorSpec) -> Result<ProcessorTraversalMut<'a>> {
        // Gated on `port_info` presence — every registered processor has an
        // entry (subprocess-only descriptors register empty port lists), so
        // this resolves any registered type and misses only a
        // genuinely-unregistered one.
        let resolved_ports = PROCESSOR_REGISTRY.port_info(&spec.name);

        let registry_miss = resolved_ports.is_none();

        let name = the_name_a_new_node_takes(self.graph, spec.display_name.as_deref(), &spec.name)?;

        if registry_miss {
            tracing::error!(
                "Processor type '{}' is not registered — node added in Error state and will not be compiled",
                spec.name
            );
        }

        // A missed node carries the requested path verbatim, so
        // `GET /api/graph` names exactly what the caller asked for.
        let (inputs, outputs) = resolved_ports.unwrap_or_default();

        let node = ProcessorNode::new(spec.name, name, Some(spec.config), inputs, outputs);

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

        Ok(ProcessorTraversalMut {
            graph: self.graph,
            links_from_another_runtime: self.links_from_another_runtime,
            ids: vec![node_idx],
        })
    }
}

/// The class's short name, falling back to the requested import path when
/// nothing is registered under it — uncast.
///
/// Read off the registered descriptor rather than recovered from the import
/// path. Splitting the path on `:` or `::` would re-derive the short name the
/// identity grammar used to carry — the engine holds the path opaque, and a
/// default name is not a reason to start parsing it.
fn default_display_name_for(processor_class_import_path: &ProcessorClassImportPath) -> String {
    PROCESSOR_REGISTRY
        .default_display_name(processor_class_import_path)
        .unwrap_or_else(|| processor_class_import_path.as_str().to_string())
}

/// The name a node added to `graph` takes: `requested_name` cast, or, when the
/// caller gave none, the class's short name cast with the next free `-2`,
/// `-3` … appended.
///
/// A given name that casts to one a node already has is [`Error::NodeNameTaken`]
/// rather than suffixed, because a name the author typed is an address. A
/// suffix is fitted by truncating the name before it, so the result stays
/// within [`EXPOSED_NAME_MAXIMUM_LENGTH`].
pub(crate) fn the_name_a_new_node_takes(
    graph: &DiGraph<ProcessorNode, Link>,
    requested_name: Option<&str>,
    processor_class_import_path: &ProcessorClassImportPath,
) -> Result<String> {
    let is_taken = |candidate: &str| {
        graph
            .node_weights()
            .any(|node| node.display_name == candidate)
    };

    if let Some(requested_name) = requested_name {
        let cast = cast_exposed_name_to_url_safe(requested_name)?;
        if is_taken(&cast) {
            return Err(Error::NodeNameTaken {
                name: requested_name.to_string(),
                cast: cast.into_owned(),
            });
        }
        return Ok(cast.into_owned());
    }

    let default_name = default_display_name_for(processor_class_import_path);
    let cast = cast_exposed_name_to_url_safe(&default_name)?;
    if !is_taken(&cast) {
        return Ok(cast.into_owned());
    }
    let mut ordinal = 2usize;
    loop {
        let suffix = format!("-{ordinal}");
        let room_before_the_suffix = EXPOSED_NAME_MAXIMUM_LENGTH - suffix.len();
        let stem = cast[..cast.len().min(room_before_the_suffix)].trim_end_matches('-');
        let candidate = format!("{stem}{suffix}");
        if !is_taken(&candidate) {
            return Ok(candidate);
        }
        ordinal += 1;
    }
}
