// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;

use petgraph::graph::DiGraph;

use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    EXPOSED_NAME_MAXIMUM_LENGTH, GraphNode, GraphNodePorts, GraphNodeWithComponents, Link,
    NodeFailureReasonComponent, NodeNotReadyBecauseComponent, NodeRunId, NodeTraversalMut,
    ProcessRaisingBecauseComponent, StateComponent, TraversalSourceMut,
    cast_exposed_name_to_url_safe, config_keys_of_a_node_type_holding_a_node_reference,
    config_node_reference_edges_of_one_node, the_input_ports_of_the_exposed_config_fields,
};
use crate::core::nodes::{NodeSpec, NodeState, NodeTypesOneStreamResolves};

impl<'a> TraversalSourceMut<'a> {
    pub fn add_v(self, spec: NodeSpec) -> Result<NodeTraversalMut<'a>> {
        let node_types = self.node_types_this_stream_resolves;
        let resolved_ports = node_types.port_info(&spec.name);

        let registry_miss = resolved_ports.is_none();
        let placeholder_of_its_type =
            node_types.placeholder_of_the_node_type_not_described(&spec.name);

        let name = the_name_a_new_node_takes(
            self.graph,
            self.node_names_the_graph_only_nodes_hold,
            node_types,
            spec.display_name.as_deref(),
            &spec.name,
        )?;
        node_types.refuse_a_node_this_stream_cannot_add(&name, &spec.name, &spec.config)?;
        if node_types.is_a_view_type_of_this_streams_project(&spec.name)
            && let Some(holder) = self
                .graph
                .node_weights()
                .find(|node| node.node_type == spec.name && node.stands())
        {
            return Err(Error::GraphError(format!(
                "node `{name}` cannot be of the view type `{}`: node `{}` ({}) of the stream \
                 already holds it, and a stream holds one node of each view",
                spec.name, holder.display_name, holder.node_uuid
            )));
        }

        if registry_miss {
            tracing::error!(
                "Node type '{}' is not registered — node added in Error state and will not be compiled",
                spec.name
            );
        }

        // A missed node carries the requested path verbatim, so
        // `GET /api/graph` names exactly what the caller asked for.
        let GraphNodePorts {
            mut inputs,
            outputs,
        } = resolved_ports.unwrap_or_default();
        if !registry_miss && placeholder_of_its_type.is_none() {
            let declared_ports: Vec<_> = inputs.iter().chain(&outputs).cloned().collect();
            inputs.extend(the_input_ports_of_the_exposed_config_fields(
                &name,
                &spec.name,
                node_types,
                &declared_ports,
                &spec.exposed_config_fields,
            )?);
        }

        if let Some(node_uuid) = spec.node_uuid
            && let Some(holder) = self
                .graph
                .node_weights()
                .find(|node| node.node_uuid == node_uuid && node.stands())
        {
            return Err(Error::GraphError(format!(
                "node `{name}` cannot take the UUID {node_uuid}: node `{}` of the graph holds it",
                holder.display_name
            )));
        }
        let node_kind = node_types.node_kind(&spec.name);
        let mut node = GraphNode::new(
            spec.name,
            node_kind,
            name,
            Some(spec.config),
            inputs,
            outputs,
        );
        if let Some(node_uuid) = spec.node_uuid {
            node.node_uuid = node_uuid;
        }
        node.pins = crate::core::nodes::the_saved_values_without_any_on_an_output_a_panel_writes(
            node_types,
            &node.node_type,
            &node.ports,
            &node.display_name,
            node.ports.input_values_under_the_current_names(spec.pins),
        );
        node.resolve_the_ports_its_settings_declare();
        node.exposed_config_fields = spec.exposed_config_fields;
        node.studio = spec.studio;

        let node_idx = self.graph.add_node(node);

        if let Some(node_mut) = self.graph.node_weight_mut(node_idx) {
            let state = StateComponent::new(if registry_miss {
                NodeState::Error
            } else {
                NodeState::Pending
            });
            if let Some(placeholder) = &placeholder_of_its_type {
                state.hold_as_a_placeholder(format!(
                    "its type `{}` was not described, so the node is held as written and runs \
                     nothing: {}",
                    node_mut.node_type, placeholder.why_its_node_code_did_not_describe_it
                ));
            }
            node_mut.insert(NodeFailureReasonComponent(state.clone_inner()));
            node_mut.insert(ProcessRaisingBecauseComponent(state.clone_inner()));
            node_mut.insert(NodeNotReadyBecauseComponent(state.clone_inner()));
            node_mut.insert(state);
        }

        Ok(NodeTraversalMut {
            graph: self.graph,
            ids: vec![node_idx],
        })
    }
}

impl TraversalSourceMut<'_> {
    pub fn rename_v(self, node_run_id: &NodeRunId, requested_name: &str) -> Result<String> {
        let node_index = self
            .graph
            .node_indices()
            .find(|node_index| self.graph[*node_index].id == *node_run_id)
            .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))?;
        let current_name = self.graph[node_index].display_name.clone();
        let cast = cast_exposed_name_to_url_safe(requested_name)?.into_owned();
        if cast == current_name {
            return Ok(cast);
        }
        if self.node_names_the_graph_only_nodes_hold.contains(&cast)
            || self
                .graph
                .node_weights()
                .any(|node| holds_the_name(node, &cast))
        {
            return Err(Error::NodeNameTaken {
                name: requested_name.to_string(),
                cast,
            });
        }
        for node in self.graph.node_weights() {
            let Some(config) = &node.config else {
                continue;
            };
            for reference in config_node_reference_edges_of_one_node(
                &node.display_name,
                config,
                config_keys_of_a_node_type_holding_a_node_reference(
                    self.node_types_this_stream_resolves,
                    &node.node_type,
                ),
            )? {
                if cast_exposed_name_to_url_safe(&reference.edge.referenced_node)?.as_ref()
                    == current_name
                {
                    return Err(Error::GraphError(format!(
                        "node `{current_name}` cannot be renamed `{cast}`: node `{}`'s config \
                         key `{}` names it by its name",
                        node.display_name, reference.edge.config_key
                    )));
                }
            }
        }
        self.graph[node_index].display_name = cast.clone();
        Ok(cast)
    }
}

/// The class's short name, falling back to the requested import path when
/// nothing is registered under it — uncast.
///
/// Read off the registered descriptor rather than recovered from the import
/// path. Splitting the path on `:` or `::` would re-derive the short name the
/// identity grammar used to carry — the engine holds the path opaque, and a
/// default name is not a reason to start parsing it.
fn default_display_name_for(
    node_types: &NodeTypesOneStreamResolves,
    node_class_import_path: &NodeClassImportPath,
) -> String {
    node_types
        .default_display_name(node_class_import_path)
        .unwrap_or_else(|| node_class_import_path.as_str().to_string())
}

/// Whether `node` holds `name`: one pending deletion, which leaves with the commit that takes
/// it out, holds none.
fn holds_the_name(node: &GraphNode, name: &str) -> bool {
    node.display_name == name && node.stands()
}

/// `requested_name` cast, or [`Error::NodeNameTaken`] when a node in `graph`
/// or a graph-only node already has that name — a name the author typed is an
/// address.
fn the_requested_node_name_unless_taken(
    graph: &DiGraph<GraphNode, Link>,
    node_names_the_graph_only_nodes_hold: &HashSet<String>,
    requested_name: &str,
) -> Result<String> {
    let cast = cast_exposed_name_to_url_safe(requested_name)?;
    if node_names_the_graph_only_nodes_hold.contains(cast.as_ref())
        || graph.node_weights().any(|node| holds_the_name(node, &cast))
    {
        return Err(Error::NodeNameTaken {
            name: requested_name.to_string(),
            cast: cast.into_owned(),
        });
    }
    Ok(cast.into_owned())
}

/// The name a node added to `graph` takes, never one a graph-only node holds:
/// `requested_name` cast, or, when the
/// caller gave none, the class's short name cast with the next free `-2`,
/// `-3` … appended.
///
/// A given name that casts to one a node already has is [`Error::NodeNameTaken`]
/// rather than suffixed, because a name the author typed is an address. A
/// suffix is fitted by truncating the name before it, so the result stays
/// within [`EXPOSED_NAME_MAXIMUM_LENGTH`].
pub(crate) fn the_name_a_new_node_takes(
    graph: &DiGraph<GraphNode, Link>,
    node_names_the_graph_only_nodes_hold: &HashSet<String>,
    node_types: &NodeTypesOneStreamResolves,
    requested_name: Option<&str>,
    node_class_import_path: &NodeClassImportPath,
) -> Result<String> {
    let is_taken = |candidate: &str| {
        node_names_the_graph_only_nodes_hold.contains(candidate)
            || graph
                .node_weights()
                .any(|node| holds_the_name(node, candidate))
    };

    if let Some(requested_name) = requested_name {
        return the_requested_node_name_unless_taken(
            graph,
            node_names_the_graph_only_nodes_hold,
            requested_name,
        );
    }

    the_default_name_a_new_node_takes(node_types, node_class_import_path, is_taken)
}

/// The class's short name cast, with the next free `-2`, `-3` … appended while `is_taken` holds
/// it; a suffix is fitted by truncating the name before it, so the result stays within
/// [`EXPOSED_NAME_MAXIMUM_LENGTH`].
pub(crate) fn the_default_name_a_new_node_takes(
    node_types: &NodeTypesOneStreamResolves,
    node_class_import_path: &NodeClassImportPath,
    is_taken: impl Fn(&str) -> bool,
) -> Result<String> {
    let default_name = default_display_name_for(node_types, node_class_import_path);
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

#[cfg(test)]
mod tests {
    use crate::core::descriptors::NodeClassImportPath;
    use crate::core::error::Error;
    use crate::core::graph::Graph;
    use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
    use crate::core::test_support::{MockSourceTakingOneSetting, ensure_test_mocks_registered};

    #[test]
    fn a_built_in_that_moved_to_a_module_is_refused_and_nothing_is_added() {
        let moved_out = NodeClassImportPath::of_built_in_node("AddVTestMovedOut");
        NODE_REGISTRY.register_built_in_node_type_moved_to_a_module(
            moved_out.clone(),
            NodeClassImportPath::new("tatolab.addvtest:AddVTestMovedOut").unwrap(),
            "tatolab-addvtest",
        );
        let mut graph = Graph::new();

        let refusal = graph
            .traversal_mut()
            .add_v(NodeSpec::new(moved_out, serde_json::json!({})))
            .map(|_| ())
            .unwrap_err();

        assert!(
            matches!(refusal, Error::BuiltInNodeTypeMovedToAModule { .. }),
            "{refusal:?}"
        );
        assert!(graph.traversal().v(()).ids().is_empty());
    }

    /// The node is named in the refusal by the name it would have taken, so a
    /// defaulted add says which node as plainly as a named one.
    #[test]
    fn a_config_the_type_does_not_take_is_refused_naming_the_node_and_nothing_is_added() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();

        let refusal = graph
            .traversal_mut()
            .add_v(NodeSpec::new(
                MockSourceTakingOneSetting::node_class_import_path(),
                serde_json::json!({"frame_widht": 640}),
            ))
            .map(|_| ())
            .unwrap_err();

        match refusal {
            Error::NodeConfigRefused {
                node_name, refusal, ..
            } => {
                assert_eq!(node_name, "mocksourcetakingonesetting");
                assert!(refusal.contains("`frame_widht`"), "{refusal}");
            }
            other => panic!("expected NodeConfigRefused, got {other:?}"),
        }
        assert!(graph.traversal().v(()).ids().is_empty());
    }

    #[test]
    fn a_node_added_live_takes_no_name_a_graph_only_node_holds() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        graph.set_node_names_the_graph_only_nodes_hold(std::collections::HashSet::from([
            "overlay".to_string(),
            "mocksourcetakingonesetting".to_string(),
        ]));
        let a_mock_source = || {
            NodeSpec::new(
                MockSourceTakingOneSetting::node_class_import_path(),
                serde_json::json!({}),
            )
        };

        let refusal = graph
            .traversal_mut()
            .add_v(a_mock_source().with_display_name("Overlay"))
            .map(|_| ())
            .unwrap_err();
        assert!(
            matches!(&refusal, Error::NodeNameTaken { cast, .. } if cast == "overlay"),
            "{refusal:?}"
        );

        let defaulted_name = graph
            .traversal_mut()
            .add_v(a_mock_source())
            .unwrap()
            .first()
            .map(|node| node.display_name.clone());
        assert_eq!(
            defaulted_name.as_deref(),
            Some("mocksourcetakingonesetting-2")
        );
    }
}
