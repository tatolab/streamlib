// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The graph as data: the spec a runtime loads.
//!
//! A [`GraphSnapshot`] is the spec keys of a `graph` document — `stream`, `clock`,
//! `nodes[].name` / `type` / `input_values` and `links[].source` / `target` — and
//! nothing else. A document is upgraded to the current shape before it is read, so
//! `graph`'s own output, which still renders a node's `config` and `pins`, reads into
//! one with its live keys skipped, and any other key is refused by name. Loading one is
//! [`Runner::load_stream_from_graph_snapshot`](crate::core::runtime::Runner::load_stream_from_graph_snapshot);
//! there is no saver, because the render is the export.

mod graph_document_refusals_and_link_end_naming;
mod graph_only_nodes_as_drawn;

use graph_document_refusals_and_link_end_naming::{
    name_each_link_end_of_one_level_by_its_nodes_name,
    name_each_link_end_of_one_level_by_its_nodes_uuid, refuse_a_group_of_no_uuid_or_one_uuid_twice,
    refuse_a_key_this_runtime_does_not_read,
};
pub(crate) use graph_document_refusals_and_link_end_naming::{
    refuse_a_port_the_node_does_not_have, refuse_an_integer_literal_wider_than_64_bits,
    the_ports_of_a_graph_document_node,
};
pub use graph_only_nodes_as_drawn::{
    GraphOnlyNodesAsDrawn, GraphSnapshotWithGraphOnlyNodesResolved, LinkSetAside,
    NodeChangeADrawnLinkBlocks,
};

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::core::context::StreamClockSpec;
use crate::core::descriptors::NodeClassImportPath;
use crate::core::graph::GraphNodePorts;
use crate::core::graph::graph_interface_resolution::{
    GraphInterfaceLevel, is_a_graph_interface_node_type,
    the_family_a_port_linked_to_an_interface_node_carries,
};
use crate::core::graph::graph_variable_resolution::is_a_graph_variable_node_type;
use crate::core::graph::{
    cast_exposed_name_to_url_safe, config_keys_of_a_node_type_holding_a_node_reference,
    config_node_reference_edges_of_one_node, node_names_listed_for_a_refusal,
    refuse_a_reference_to_a_node_of_the_wrong_type,
    refuse_a_second_referrer_of_a_node_that_takes_one,
};
use crate::core::json_schema::{LabelledNodeGroup, LinkPortRefOutput};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::{Error, PortDirection, Result};

/// A graph a runtime runs: its nodes by class import path, config and name,
/// and the links between their ports.
///
/// Read only through [`Self::from_json_str`] and [`Self::from_graph_document`],
/// which refuse a key the runtime does not read.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GraphSnapshot {
    /// The stream asset whose graph document this graph is. Absent on a graph no document holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<Uuid>,

    /// The name the stream asset is shown under, written in its graph document; absent shows
    /// it under its document's file name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// The stream this graph is. Absent on a graph no stream was loaded as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,

    /// The clock the stream runs on. Absent reads as real time.
    #[serde(skip_serializing_if = "StreamClockSpec::is_real")]
    pub clock: StreamClockSpec,

    /// Every node, each under the name links name it by.
    pub nodes: Vec<GraphSnapshotNode>,

    /// Every link, each end a node's port.
    pub links: Vec<GraphSnapshotLink>,

    /// Every labelled group of the stream's own level, which nothing that runs reads.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<LabelledNodeGroup>,

    /// What Tato Studio draws beside the nodes, its canvas notes, under the studio contract;
    /// nothing that runs reads it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub studio: Option<serde_json::Value>,
}

/// The spec keys a graph document is read through, before its keys are checked.
#[derive(Deserialize)]
struct GraphSnapshotAsWritten {
    #[serde(default)]
    asset: Option<Uuid>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    stream: Option<String>,
    #[serde(default)]
    clock: StreamClockSpec,
    nodes: Vec<GraphSnapshotNode>,
    #[serde(default)]
    links: Vec<GraphSnapshotLink>,
    #[serde(default)]
    groups: Vec<LabelledNodeGroup>,
    #[serde(default)]
    studio: Option<serde_json::Value>,
}

/// One JSON object of a graph document: the graph, a node, a link or a link
/// end.
type GraphDocumentObject = serde_json::Map<String, serde_json::Value>;

/// The keys one kind of object in a graph document is written with: the spec
/// a load reads, and the live keys `graph` renders beside it, which a load
/// skips. A live key `graph` stops rendering stays listed, so a graph recorded
/// while it rendered still loads.
struct GraphDocumentObjectKeys {
    /// The kind of object, as a refusal names it.
    object_kind: &'static str,
    spec_keys: &'static [&'static str],
    live_keys: &'static [&'static str],
}

const GRAPH_DOCUMENT_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "a graph",
    spec_keys: &[
        "asset",
        "display_name",
        "stream",
        "clock",
        "nodes",
        "links",
        "groups",
        "studio",
    ],
    // `runtime_name` stays so the graphs recorded while `graph` rendered it still load.
    live_keys: &[
        "condition",
        "extensions",
        "function_node_pool",
        "machine",
        "references",
        "runtime_name",
    ],
};

const GRAPH_NODE_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "a node",
    spec_keys: &[
        "id",
        "name",
        "type",
        "input_values",
        "exposed_config_fields",
        "studio",
        "fragment",
    ],
    live_keys: &[
        "kind",
        "language",
        "config_checksum",
        "ports",
        "inputs_waiting_for_a_value",
        "components",
        "live",
    ],
};

const GRAPH_FRAGMENT_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "a fragment",
    spec_keys: &["nodes", "links", "groups"],
    live_keys: &[],
};

const GRAPH_LINK_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "a link",
    spec_keys: &["source", "target"],
    live_keys: &[
        "id",
        "capacity",
        "state",
        "error_reason",
        "free_conversion",
        "warnings",
        "components",
        "live",
    ],
};

const GRAPH_LINK_END_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "a link end",
    spec_keys: &["node", "port", "member"],
    live_keys: &[],
};

/// The keys a link end naming another stream's or another machine's port
/// would carry, each refused naming [`TRACK_RECEIVE_READS_ANOTHER_STREAMS_PORT`].
const LINK_END_KEYS_NAMING_A_PORT_OUTSIDE_THIS_STREAM: [&str; 2] = ["machine", "stream"];

/// How a stream reads a port outside it, as a refusal of a link end naming
/// one says.
const TRACK_RECEIVE_READS_ANOTHER_STREAMS_PORT: &str = "a link's ends are both in this \
     stream; read another stream's track at a relay, on this machine or another, through a \
     `MoqTrackReceive` node";

/// One node of a [`GraphSnapshot`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSnapshotNode {
    /// The node's UUID, kept across every run; absent on a node named by its name alone, which
    /// takes a fresh one as it is placed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "a_node_uuid_or_none_for_a_string_that_is_no_uuid"
    )]
    pub id: Option<Uuid>,

    /// The node's name, unique in the graph once cast.
    pub name: String,

    /// The import path of the class the node is.
    #[serde(rename = "type")]
    pub node_type: NodeClassImportPath,

    /// The value the node holds for each input, by input name; an input it names no value for
    /// reads its declared default.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub input_values: serde_json::Map<String, serde_json::Value>,

    /// The config fields the node takes as inputs too, each an input of the field's name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed_config_fields: Vec<String>,

    /// What the studio draws the node with: canvas position, group, comment, colour.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub studio: serde_json::Map<String, serde_json::Value>,

    /// The stored output of the graph generator `type` names, on a fragment, whose `config` is
    /// the generator's promoted config; absent on every other node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<GraphFragment>,
}

impl GraphSnapshotNode {
    /// `node`, a node of a running graph, as a graph document holds it: its uuid, name, type,
    /// input values and studio fields, exposing nothing and of no fragment.
    pub(crate) fn of_a_running_node(node: &crate::core::graph::GraphNode) -> Self {
        Self {
            id: Some(node.node_uuid),
            name: node.display_name.clone(),
            node_type: node.node_type.clone(),
            input_values: node.input_values.clone(),
            exposed_config_fields: Vec::new(),
            studio: node.studio.clone(),
            fragment: None,
        }
    }
}

/// A fragment's stored output: the nodes and links its graph generator built, each link end
/// naming a node of the fragment.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GraphFragment {
    /// Every node the generator built, a fragment it placed among them.
    pub nodes: Vec<GraphSnapshotNode>,

    /// Every link between those nodes' ports.
    #[serde(default)]
    pub links: Vec<GraphSnapshotLink>,

    /// Every labelled group the generator made of those nodes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<LabelledNodeGroup>,
}

/// A node's `id` as a UUID; a string that is no UUID reads as none.
fn a_node_uuid_or_none_for_a_string_that_is_no_uuid<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Uuid>, D::Error> {
    let written = Option::<String>::deserialize(deserializer)?;
    Ok(written.and_then(|written| Uuid::parse_str(&written).ok()))
}

/// One link of a [`GraphSnapshot`], from an output port to an input port,
/// both on nodes the graph holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSnapshotLink {
    /// The output port the link carries from, `{node, port}`.
    pub source: LinkPortRefOutput,

    /// The input port the link carries into, `{node, port}`.
    pub target: LinkPortRefOutput,
}

impl GraphSnapshot {
    /// Read a graph from JSON — a `graph` document or the spec keys alone.
    pub fn from_json_str(json: &str) -> Result<Self> {
        refuse_an_integer_literal_wider_than_64_bits(json)?;
        Self::from_graph_document(
            serde_json::from_str(json)
                .map_err(|e| Error::GraphError(format!("the graph does not parse: {e}")))?,
        )
    }

    /// Read a graph from a parsed `graph` document, in the shape an older runtime wrote it or in
    /// this one's, its live keys skipped and any other key refused by name.
    pub fn from_graph_document(graph_document: serde_json::Value) -> Result<Self> {
        let graph_document =
            streamlib_project_repository::stream_document_in_the_current_shape(graph_document);
        refuse_a_key_this_runtime_does_not_read(&graph_document)?;
        let GraphSnapshotAsWritten {
            asset,
            display_name,
            stream,
            clock,
            nodes,
            links,
            groups,
            studio,
        } = GraphSnapshotAsWritten::deserialize(&graph_document)
            .map_err(|e| Error::GraphError(format!("the graph does not parse: {e}")))?;
        let mut graph = Self {
            asset,
            display_name,
            stream,
            clock,
            nodes,
            links,
            groups,
            studio,
        };
        refuse_a_group_of_no_uuid_or_one_uuid_twice(&graph.groups, &graph.nodes, "")?;
        graph.name_each_link_end_by_its_nodes_name()?;
        Ok(graph)
    }

    /// Name each link end that names its node by UUID by that node's name instead, at every
    /// level, as the runtime runs a graph, refusing two nodes of one UUID anywhere in it.
    fn name_each_link_end_by_its_nodes_name(&mut self) -> Result<()> {
        name_each_link_end_of_one_level_by_its_nodes_name(
            &mut self.nodes,
            &mut self.links,
            &mut HashMap::new(),
        )
    }

    /// This graph as its stream asset's graph document: canonical JSON, keys sorted, two-space
    /// indent and one trailing newline, each link end naming its node by UUID, and no `stream`,
    /// which the runtime computes. Refused for a graph with no `asset` or with a node without a UUID.
    pub fn graph_document_bytes(&self) -> Result<Vec<u8>> {
        if self.asset.is_none() {
            return Err(Error::GraphError(
                "a graph document holds its stream asset's UUID as `asset`, and this graph \
                 holds none"
                    .to_string(),
            ));
        }
        let mut document = Self {
            stream: None,
            ..self.clone()
        };
        name_each_link_end_of_one_level_by_its_nodes_uuid(
            &mut document.nodes,
            &mut document.links,
        )?;
        streamlib_project_repository::canonical_project_json_bytes(&document)
            .map_err(|e| Error::GraphError(format!("the graph document does not serialize: {e}")))
    }

    /// Every node at every level, each under its path of names from the stream, depth first:
    /// a fragment's nodes come just before the fragment node itself.
    pub fn nodes_at_every_level(&self) -> Vec<(String, &GraphSnapshotNode)> {
        fn push_each_node_of_one_level<'graph>(
            nodes: &'graph [GraphSnapshotNode],
            path_of_names: &str,
            every_node: &mut Vec<(String, &'graph GraphSnapshotNode)>,
        ) {
            for node in nodes {
                let node_path = format!("{path_of_names}{}", node.name);
                if let Some(fragment) = &node.fragment {
                    push_each_node_of_one_level(
                        &fragment.nodes,
                        &format!("{node_path}/"),
                        every_node,
                    );
                }
                every_node.push((node_path, node));
            }
        }
        let mut every_node = Vec::new();
        push_each_node_of_one_level(&self.nodes, "", &mut every_node);
        every_node
    }

    /// This graph with a fresh UUID given to each node that holds none.
    pub fn with_a_uuid_minted_for_each_node_holding_none(&self) -> Self {
        fn mint_a_uuid_for_each_node_holding_none(nodes: &mut [GraphSnapshotNode]) {
            for node in nodes {
                node.id.get_or_insert_with(Uuid::new_v4);
                if let Some(fragment) = &mut node.fragment {
                    mint_a_uuid_for_each_node_holding_none(&mut fragment.nodes);
                }
            }
        }
        let mut graph = self.clone();
        mint_a_uuid_for_each_node_holding_none(&mut graph.nodes);
        graph
    }

    /// Serialize the graph as pretty-printed JSON.
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| Error::GraphError(format!("the graph does not serialize: {e}")))
    }

    /// Check the graph without loading it: every `type` one the stream
    /// resolves on this floor, every config one its type takes, names unique
    /// once cast, every link end a port of the right direction on a node the
    /// graph holds, and every reference edge a node of the type its config key
    /// takes.
    ///
    /// Its variables and interface nodes are resolved first: the graph the
    /// runtime would run is checked, and each setter's value and each
    /// interface port with it.
    pub fn validate(&self, node_types: &NodeTypesOneStreamResolves) -> Result<()> {
        self.with_graph_only_nodes_resolved(node_types)?
            .validate(node_types)
    }

    pub fn node_types_its_running_nodes_name_at_every_level(&self) -> Vec<NodeClassImportPath> {
        self.nodes_at_every_level()
            .into_iter()
            .filter(|(_, node)| {
                node.fragment.is_none()
                    && !is_a_graph_variable_node_type(&node.node_type)
                    && !is_a_graph_interface_node_type(node.node_type.as_str())
            })
            .map(|(_, node)| node.node_type.clone())
            .collect()
    }

    /// Resolve the nodes that run nothing out of this graph: its fragments, flattened, then its
    /// variables, then its interface nodes. `node_types` names the config keys of each node
    /// inside a fragment that hold a node reference.
    pub fn with_graph_only_nodes_resolved(
        &self,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<GraphSnapshotWithGraphOnlyNodesResolved> {
        let fragments = Self {
            groups: Vec::new(),
            studio: None,
            ..self.clone()
        }
        .with_each_port_under_its_current_name(node_types)
        .with_graph_fragments_flattened(node_types)?;
        let variables = fragments
            .graph_the_runtime_runs
            .with_graph_variables_resolved(&fragments.fragments_as_drawn, node_types)?;
        let (top_level, connection_wires_as_drawn) = variables
            .graph_the_runtime_runs
            .with_connection_wires_lowered(node_types)?;
        let top_level = &top_level;
        let interface = top_level.with_graph_interface_resolved(
            GraphInterfaceLevel::TheStreamsTopLevel,
            &|linked_port, direction| {
                the_family_a_port_linked_to_an_interface_node_carries(
                    node_types,
                    top_level,
                    linked_port,
                    direction,
                )
            },
        )?;
        Ok(GraphSnapshotWithGraphOnlyNodesResolved {
            graph_the_runtime_runs: interface.graph_the_runtime_runs,
            graph_only_nodes_as_drawn: GraphOnlyNodesAsDrawn {
                fragments_as_drawn: fragments.fragments_as_drawn,
                variables_as_drawn: variables.variables_as_drawn,
                connection_wires_as_drawn,
                interface_as_drawn: interface.interface_as_drawn,
                groups_as_drawn: self.groups.clone(),
                studio_as_drawn: self.studio.clone(),
                links_set_aside: Vec::new(),
            },
            values_the_set_variables_hold: variables.values_the_set_variables_hold,
        })
    }

    /// This graph with each value and each link end held under a name a port had moved to the
    /// port's current name at every level, a fragment's nodes and links too, as the node types
    /// `node_types` resolves declare them, so every step after it, flattening, the checks of a
    /// fragment's interface and the variables included, knows only the current names.
    fn with_each_port_under_its_current_name(
        mut self,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Self {
        each_port_of_one_level_under_its_current_name(&mut self.nodes, &mut self.links, node_types);
        self
    }

    /// [`Self::validate`] of a graph whose variables and interface nodes are
    /// already resolved.
    pub fn validate_a_graph_holding_no_graph_only_node(
        &self,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<()> {
        let mut nodes_by_cast_name: HashMap<String, &GraphSnapshotNode> = HashMap::new();
        for node in &self.nodes {
            node_types.refuse_a_node_this_stream_cannot_add(
                &node.name,
                &node.node_type,
                &node.input_values,
            )?;
            if node_types.port_info(&node.node_type).is_none() {
                return Err(Error::UnknownNodeType {
                    ident: node.node_type.clone(),
                });
            }
            match nodes_by_cast_name.entry(cast_exposed_name_to_url_safe(&node.name)?.into_owned())
            {
                Entry::Occupied(taken) => {
                    return Err(Error::NodeNameTaken {
                        name: node.name.clone(),
                        cast: taken.key().clone(),
                    });
                }
                Entry::Vacant(free) => {
                    free.insert(node);
                }
            }
        }
        let the_names_the_graph_holds =
            || node_names_listed_for_a_refusal(nodes_by_cast_name.keys().map(String::as_str));

        for link in &self.links {
            for (end, direction) in [
                (&link.source, PortDirection::Output),
                (&link.target, PortDirection::Input),
            ] {
                let Some(node) =
                    nodes_by_cast_name.get(cast_exposed_name_to_url_safe(&end.node)?.as_ref())
                else {
                    return Err(Error::GraphError(format!(
                        "a link names node `{}`, which the graph does not hold. The graph \
                         holds: {}",
                        end.node,
                        the_names_the_graph_holds()
                    )));
                };
                refuse_a_port_the_node_does_not_have(
                    node_types, &end.node, node, &end.port, direction,
                )?;
            }
        }

        let mut references_of_every_node = Vec::new();
        for node in &self.nodes {
            for reference in config_node_reference_edges_of_one_node(
                &node.name,
                &node.input_values,
                config_keys_of_a_node_type_holding_a_node_reference(node_types, &node.node_type),
            )? {
                let referenced_node_name = &reference.edge.referenced_node;
                let Some(referenced_node) = nodes_by_cast_name
                    .get(cast_exposed_name_to_url_safe(referenced_node_name)?.as_ref())
                else {
                    return Err(Error::GraphError(format!(
                        "input `{}` of node `{}` names node `{referenced_node_name}`, which \
                         the graph does not hold. The graph holds: {}",
                        reference.edge.config_key,
                        node.name,
                        the_names_the_graph_holds()
                    )));
                };
                refuse_a_reference_to_a_node_of_the_wrong_type(
                    &reference,
                    &referenced_node.node_type,
                )?;
                references_of_every_node.push(reference);
            }
        }
        refuse_a_second_referrer_of_a_node_that_takes_one(&references_of_every_node)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests;

/// Each value and each link end of one level's `nodes` and `links`, and of every fragment level
/// they hold, held under a name a port had moved to the port's current name. A value under a
/// field the node type's config schema lists stays under its own name, and a link held under
/// several names its ports had is held once. A node of a type that takes no config exposes no
/// config field: each it exposed, saved before its type was typed, is now an input of its own
/// or was merged into one, so it is dropped.
fn each_port_of_one_level_under_its_current_name(
    nodes: &mut [GraphSnapshotNode],
    links: &mut Vec<GraphSnapshotLink>,
    node_types: &NodeTypesOneStreamResolves,
) {
    let mut ports_of_each_node: HashMap<String, GraphNodePorts> = HashMap::new();
    for node in nodes.iter_mut() {
        if let Some(fragment) = &mut node.fragment {
            each_port_of_one_level_under_its_current_name(
                &mut fragment.nodes,
                &mut fragment.links,
                node_types,
            );
        }
        let Some(ports) = node_types.port_info(&node.node_type) else {
            continue;
        };
        node.exposed_config_fields.clear();
        let config_fields = node_types.the_settings_its_config_schema_lists(&node.node_type);
        let (config_values, input_values): (serde_json::Map<_, _>, serde_json::Map<_, _>) =
            std::mem::take(&mut node.input_values)
                .into_iter()
                .partition(|(name, _)| config_fields.contains(name));
        node.input_values = ports.input_values_under_the_current_names(input_values);
        node.input_values.extend(config_values);
        if let Ok(cast) = cast_exposed_name_to_url_safe(&node.name) {
            ports_of_each_node.insert(cast.into_owned(), ports);
        }
    }
    let current_name = |end: &mut LinkPortRefOutput, direction: PortDirection| {
        let (Ok(node), Ok(port)) = (
            cast_exposed_name_to_url_safe(&end.node),
            cast_exposed_name_to_url_safe(&end.port),
        ) else {
            return;
        };
        if let Some(current) = ports_of_each_node
            .get(node.as_ref())
            .and_then(|ports| ports.current_name(direction, &port))
            && current != port
        {
            end.port = current.to_string();
        }
    };
    let mut links_held: Vec<GraphSnapshotLink> = Vec::with_capacity(links.len());
    for mut link in std::mem::take(links) {
        current_name(&mut link.source, PortDirection::Output);
        current_name(&mut link.target, PortDirection::Input);
        if !links_held.contains(&link) {
            links_held.push(link);
        }
    }
    *links = links_held;
}
