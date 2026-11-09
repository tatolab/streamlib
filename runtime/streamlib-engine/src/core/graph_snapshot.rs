// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph as data: the spec a runtime loads, in the one shape `graph`
//! renders.
//!
//! A [`GraphSnapshot`] is the spec keys of a `graph` document — `stream`, `clock`,
//! `nodes[].name` / `type` / `config` and `links[].source` / `target` — and
//! nothing else, so `graph`'s own output reads into one with
//! its live keys skipped, and any other key is refused by name. Loading one is
//! [`Runner::load_stream_from_graph_snapshot`](crate::core::runtime::Runner::load_stream_from_graph_snapshot);
//! there is no saver, because the render is the export.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::core::context::StreamClockSpec;
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{
    cast_exposed_name_to_url_safe, config_keys_of_a_node_type_holding_a_node_reference,
    config_node_reference_edges_of_one_node, node_names_listed_for_a_refusal,
    port_names_listed_for_a_refusal, refuse_a_reference_to_a_node_of_the_wrong_type,
    refuse_a_second_referrer_of_a_node_that_takes_one,
};
use crate::core::graph_fragment_flattening::GraphFragmentsAsDrawn;
use crate::core::graph_interface_resolution::{
    GraphInterfaceAsDrawn, is_a_graph_interface_node_type,
};
use crate::core::graph_variable_resolution::{
    GraphVariablesAsDrawn, SetVariableAndTheValueItHolds, ValueASetVariableHolds,
    is_a_graph_variable_node_type,
};
use crate::core::json_schema::{
    ForkedStreamOrigin, GraphResponse, LabelledNodeGroup, LinkPortRefOutput,
};
use crate::core::processors::NodeTypesOneStreamResolves;
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

    /// The stream of another project this graph document was forked from, which nothing that
    /// runs reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<ForkedStreamOrigin>,
}

/// The spec keys a graph document is read through, before its keys are checked.
#[derive(Deserialize)]
struct GraphSnapshotAsWritten {
    #[serde(default)]
    asset: Option<Uuid>,
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
    forked_from: Option<ForkedStreamOrigin>,
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
        "stream",
        "clock",
        "nodes",
        "links",
        "groups",
        "forked_from",
    ],
    // `runtime_name` stays so the graphs recorded while `graph` rendered it still load.
    live_keys: &["extensions", "machine", "references", "runtime_name"],
};

const GRAPH_NODE_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "a node",
    spec_keys: &[
        "id",
        "name",
        "type",
        "config",
        "pins",
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
    live_keys: &["id", "capacity", "state", "error_reason", "components"],
};

const GRAPH_LINK_END_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "a link end",
    spec_keys: &["node", "port"],
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
    pub processor_type: ProcessorClassImportPath,

    /// The node's config. Absent reads as `{}`, the config `add_node` takes
    /// when it is given none.
    #[serde(default = "a_config_naming_no_setting")]
    pub config: serde_json::Value,

    /// The value set on each unlinked input, by input name.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub pins: serde_json::Map<String, serde_json::Value>,

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

fn a_config_naming_no_setting() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
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

    /// Read a graph from a parsed `graph` document, its live keys skipped and
    /// any other key refused by name.
    pub fn from_graph_document(graph_document: serde_json::Value) -> Result<Self> {
        refuse_a_key_this_runtime_does_not_read(&graph_document)?;
        let GraphSnapshotAsWritten {
            asset,
            stream,
            clock,
            nodes,
            links,
            groups,
            forked_from,
        } = GraphSnapshotAsWritten::deserialize(&graph_document)
            .map_err(|e| Error::GraphError(format!("the graph does not parse: {e}")))?;
        let mut graph = Self {
            asset,
            stream,
            clock,
            nodes,
            links,
            groups,
            forked_from,
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

    /// The type of every node at every level that a processor runs as: no fragment, variable or
    /// interface node. These are the types to describe before the graph is resolved.
    pub fn node_types_its_running_nodes_name_at_every_level(
        &self,
    ) -> Vec<ProcessorClassImportPath> {
        self.nodes_at_every_level()
            .into_iter()
            .filter(|(_, node)| {
                node.fragment.is_none()
                    && !is_a_graph_variable_node_type(&node.processor_type)
                    && !is_a_graph_interface_node_type(node.processor_type.as_str())
            })
            .map(|(_, node)| node.processor_type.clone())
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
            forked_from: None,
            ..self.clone()
        }
        .with_graph_fragments_flattened(node_types)?;
        let variables = fragments
            .graph_the_runtime_runs
            .with_graph_variables_resolved(&fragments.fragments_as_drawn)?;
        let interface = variables
            .graph_the_runtime_runs
            .with_graph_interface_resolved()?;
        Ok(GraphSnapshotWithGraphOnlyNodesResolved {
            graph_the_runtime_runs: interface.graph_the_runtime_runs,
            graph_only_nodes_as_drawn: GraphOnlyNodesAsDrawn {
                fragments_as_drawn: fragments.fragments_as_drawn,
                variables_as_drawn: variables.variables_as_drawn,
                interface_as_drawn: interface.interface_as_drawn,
                groups_as_drawn: self.groups.clone(),
                forked_from_as_drawn: self.forked_from.clone(),
            },
            values_the_set_variables_hold: variables.values_the_set_variables_hold,
        })
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
                &node.processor_type,
                &node.config,
            )?;
            if node_types.port_info(&node.processor_type).is_none() {
                return Err(Error::UnknownProcessorType {
                    ident: node.processor_type.clone(),
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
                &node.config,
                config_keys_of_a_node_type_holding_a_node_reference(
                    node_types,
                    &node.processor_type,
                ),
            )? {
                let referenced_node_name = &reference.edge.referenced_node;
                let Some(referenced_node) = nodes_by_cast_name
                    .get(cast_exposed_name_to_url_safe(referenced_node_name)?.as_ref())
                else {
                    return Err(Error::GraphError(format!(
                        "node `{}`'s config key `{}` names node `{referenced_node_name}`, which \
                         the graph does not hold. The graph holds: {}",
                        node.name,
                        reference.edge.config_key,
                        the_names_the_graph_holds()
                    )));
                };
                refuse_a_reference_to_a_node_of_the_wrong_type(
                    &reference,
                    &referenced_node.processor_type,
                )?;
                references_of_every_node.push(reference);
            }
        }
        refuse_a_second_referrer_of_a_node_that_takes_one(&references_of_every_node)?;

        Ok(())
    }
}

/// What the graph holds that runs nothing, as drawn: fragments, variables, interface nodes and
/// the stream level's labelled groups, so `graph` renders them back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphOnlyNodesAsDrawn {
    pub fragments_as_drawn: GraphFragmentsAsDrawn,
    pub variables_as_drawn: GraphVariablesAsDrawn,
    pub interface_as_drawn: GraphInterfaceAsDrawn,
    pub groups_as_drawn: Vec<LabelledNodeGroup>,
    pub forked_from_as_drawn: Option<ForkedStreamOrigin>,
}

/// A change to a node that a drawn link naming it by name blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeChangeADrawnLinkBlocks {
    /// Taking the node out.
    Removal,
    /// Giving the node another name.
    Rename,
}

impl NodeChangeADrawnLinkBlocks {
    fn as_refused(self) -> &'static str {
        match self {
            Self::Removal => "removed",
            Self::Rename => "renamed",
        }
    }
}

impl GraphOnlyNodesAsDrawn {
    /// Refuse `change` to the node named `node_name` while it is part of a fragment, or while a
    /// drawn link between it and a fragment, a variable or an interface node names it, which
    /// would leave that link dangling in the graph `graph` renders.
    pub fn refuse_a_node_change_a_drawn_link_names(
        &self,
        node_name: &str,
        change: NodeChangeADrawnLinkBlocks,
    ) -> Result<()> {
        let node_cast = cast_exposed_name_to_url_safe(node_name)?;
        if let Some(fragment) = self
            .fragments_as_drawn
            .fragment_holding_the_running_node(&node_cast)
        {
            return Err(Error::GraphError(format!(
                "node `{node_name}` cannot be {}: it is part of fragment `{fragment}`, which \
                 its graph generator built and which is read-only; re-run the generator to \
                 change it",
                change.as_refused()
            )));
        }
        let names_the_node = |end: &LinkPortRefOutput| {
            cast_exposed_name_to_url_safe(&end.node).is_ok_and(|end_cast| end_cast == node_cast)
        };
        let links_drawn_to_a_graph_only_node = self
            .variables_as_drawn
            .links_touching_a_variable_node
            .iter()
            .chain(
                self.interface_as_drawn
                    .links_touching_an_interface_node
                    .iter()
                    .map(|drawn| &drawn.link),
            )
            .chain(
                self.fragments_as_drawn
                    .links_touching_a_fragment
                    .iter()
                    .map(|drawn| &drawn.link),
            );
        for link in links_drawn_to_a_graph_only_node {
            if names_the_node(&link.source) || names_the_node(&link.target) {
                return Err(Error::GraphError(format!(
                    "node `{node_name}` cannot be {}: the graph links `{}/{}` to `{}/{}`, \
                     an end of which is a fragment, a variable or an interface node, and that link would \
                     name a node the graph no longer holds",
                    change.as_refused(),
                    link.source.node,
                    link.source.port,
                    link.target.node,
                    link.target.port
                )));
            }
        }
        Ok(())
    }

    /// The cast name of every fragment, variable and interface node drawn.
    pub fn node_names_cast(&self) -> HashSet<String> {
        let fragment_names_cast = self.fragments_as_drawn.fragment_names_flattened_and_cast();
        self.variables_as_drawn
            .variable_nodes
            .iter()
            .chain(
                self.interface_as_drawn
                    .interface_nodes
                    .iter()
                    .map(|interface_node| &interface_node.node),
            )
            .filter_map(|node| {
                cast_exposed_name_to_url_safe(&node.name)
                    .ok()
                    .map(std::borrow::Cow::into_owned)
            })
            .chain(fragment_names_cast)
            .collect()
    }

    /// Render the graph-only nodes into `graph_response`, a render of the graph
    /// the runtime runs, in the reverse of the order they were resolved: the
    /// interface, then the variables, then each fragment as a container.
    pub fn render_into(&self, graph_response: &mut GraphResponse) {
        self.interface_as_drawn.render_into(graph_response);
        self.variables_as_drawn.render_into(graph_response);
        self.fragments_as_drawn.render_into(graph_response);
        graph_response.groups = self.groups_as_drawn.clone();
        graph_response.forked_from = self.forked_from_as_drawn.clone();
    }
}

/// A graph with its variables and interface nodes resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphSnapshotWithGraphOnlyNodesResolved {
    /// The graph the runtime runs: no variable or interface node.
    pub graph_the_runtime_runs: GraphSnapshot,
    pub graph_only_nodes_as_drawn: GraphOnlyNodesAsDrawn,
    /// Every `SetVariable` and its value, by the setter's name.
    pub values_the_set_variables_hold: Vec<SetVariableAndTheValueItHolds>,
}

impl GraphSnapshotWithGraphOnlyNodesResolved {
    /// Check the graph the runtime runs, that each `SetVariable` holds a node
    /// of that graph, or an output port one of its nodes has, whether or not
    /// a get reads it, and that each interface port is linked to ports of its
    /// family.
    pub fn validate(&self, node_types: &NodeTypesOneStreamResolves) -> Result<()> {
        let graph = &self.graph_the_runtime_runs;
        graph.validate_a_graph_holding_no_graph_only_node(node_types)?;
        self.graph_only_nodes_as_drawn
            .interface_as_drawn
            .validate(graph, node_types)?;
        self.graph_only_nodes_as_drawn
            .fragments_as_drawn
            .validate(graph, node_types)?;
        let node_named = |node_name: &str| -> Result<Option<&GraphSnapshotNode>> {
            let cast = cast_exposed_name_to_url_safe(node_name)?;
            Ok(graph.nodes.iter().find(|node| {
                cast_exposed_name_to_url_safe(&node.name).is_ok_and(|node_cast| node_cast == cast)
            }))
        };
        let the_names_the_graph_holds =
            || node_names_listed_for_a_refusal(graph.nodes.iter().map(|node| node.name.as_str()));
        for SetVariableAndTheValueItHolds {
            set_variable_node,
            value,
        } in &self.values_the_set_variables_hold
        {
            let held_node_name = match value {
                ValueASetVariableHolds::PortOutput(source) => &source.node,
                ValueASetVariableHolds::NodeReference(referenced_node) => referenced_node,
            };
            let Some(held_node) = node_named(held_node_name)? else {
                if let Some(interface_node) = self
                    .graph_only_nodes_as_drawn
                    .interface_as_drawn
                    .interface_node_named(held_node_name)
                {
                    if let ValueASetVariableHolds::PortOutput(source) = value
                        && let Some(refusal) = interface_node
                            .declared_port
                            .refusal_of_an_output_it_does_not_have(source)
                    {
                        return Err(Error::GraphError(format!(
                            "`SetVariable` node `{set_variable_node}` holds a port the graph \
                             does not hold: {refusal}"
                        )));
                    }
                    continue;
                }
                return Err(Error::GraphError(format!(
                    "`SetVariable` node `{set_variable_node}` holds node `{held_node_name}`, \
                     which the graph does not hold. The graph holds: {}",
                    the_names_the_graph_holds()
                )));
            };
            if let ValueASetVariableHolds::PortOutput(source) = value {
                refuse_a_port_the_node_does_not_have(
                    node_types,
                    &source.node,
                    held_node,
                    &source.port,
                    PortDirection::Output,
                )?;
            }
        }
        Ok(())
    }
}

/// Refuse the first key of `graph_document` the runtime does not read — at the
/// top, on a node, a fragment, a link or a link end, at every level — naming it and where it
/// sits, before the spec is read, so a misspelled key is named rather than
/// reported as the key it stands in for being missing. A link end naming a
/// machine or a stream is refused naming how a stream reads a port outside it.
/// An object the walk expects and does not find is left for the read to
/// refuse.
/// Refuse a labelled group of `groups`, one level's, whose `id` is no UUID or names another
/// group of the level too, and the same in every fragment `nodes` holds; `node_path` is the
/// path of names down to the level, each followed by `/`.
fn refuse_a_group_of_no_uuid_or_one_uuid_twice(
    groups: &[LabelledNodeGroup],
    nodes: &[GraphSnapshotNode],
    node_path: &str,
) -> Result<()> {
    let level_named = || {
        if node_path.is_empty() {
            "the graph".to_string()
        } else {
            format!("fragment `{}`", node_path.trim_end_matches('/'))
        }
    };
    let mut group_uuids = HashSet::new();
    for group in groups {
        let group_uuid = Uuid::parse_str(&group.id).map_err(|_| {
            Error::GraphError(format!(
                "group `{}` of {} has the id `{}`, which is no UUID",
                group.title,
                level_named(),
                group.id
            ))
        })?;
        if !group_uuids.insert(group_uuid) {
            return Err(Error::GraphError(format!(
                "{} holds two groups of the UUID {group_uuid}",
                level_named()
            )));
        }
    }
    for node in nodes {
        if let Some(fragment) = &node.fragment {
            refuse_a_group_of_no_uuid_or_one_uuid_twice(
                &fragment.groups,
                &fragment.nodes,
                &format!("{node_path}{}/", node.name),
            )?;
        }
    }
    Ok(())
}

fn refuse_a_key_this_runtime_does_not_read(graph_document: &serde_json::Value) -> Result<()> {
    if let Some(graph_object) = graph_document.as_object() {
        refuse_a_key_no_such_object_holds(graph_object, &GRAPH_DOCUMENT_KEYS, || {
            "the graph".to_string()
        })?;
    }
    refuse_a_key_one_level_does_not_read(graph_document, "")
}

/// Refuse the first key the runtime does not read on a node, a link or a link end of `level` —
/// the graph, or a fragment — and of every fragment it holds; `node_path` is the path of names
/// down to `level`, each followed by `/`.
fn refuse_a_key_one_level_does_not_read(level: &serde_json::Value, node_path: &str) -> Result<()> {
    fn text_at<'o>(object: &'o GraphDocumentObject, key: &str) -> &'o str {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
    }
    fn port_address_of(object: &GraphDocumentObject) -> String {
        ["machine", "stream"]
            .into_iter()
            .filter(|leading_key| object.contains_key(*leading_key))
            .chain(["node", "port"])
            .map(|key| text_at(object, key))
            .collect::<Vec<&str>>()
            .join("/")
    }
    fn port_address_at(link: &GraphDocumentObject, end_key: &str) -> String {
        link.get(end_key)
            .and_then(serde_json::Value::as_object)
            .map_or_else(|| "?".to_string(), port_address_of)
    }
    let objects_of = |key: &str| {
        level
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_object)
    };

    for node in objects_of("nodes") {
        let node_named = || format!("node `{node_path}{}`", text_at(node, "name"));
        refuse_a_key_no_such_object_holds(node, &GRAPH_NODE_KEYS, node_named)?;
        let Some(fragment) = node.get("fragment") else {
            continue;
        };
        if let Some(fragment_object) = fragment.as_object() {
            refuse_a_key_no_such_object_holds(fragment_object, &GRAPH_FRAGMENT_KEYS, || {
                format!("the fragment of {}", node_named())
            })?;
        }
        refuse_a_key_one_level_does_not_read(
            fragment,
            &format!("{node_path}{}/", text_at(node, "name")),
        )?;
    }
    for link in objects_of("links") {
        let link_named = || {
            format!(
                "the link `{node_path}{}` → `{node_path}{}`",
                port_address_at(link, "source"),
                port_address_at(link, "target")
            )
        };
        let link_ends = ["source", "target"].into_iter().filter_map(|end_key| {
            let link_end = link.get(end_key).and_then(serde_json::Value::as_object)?;
            Some((link_end, move || {
                format!("the {end_key} end of {}", link_named())
            }))
        });
        // A link to another stream is refused naming the way to read one before
        // any key a runtime once rendered on such a link is refused unread.
        for (link_end, link_end_named) in link_ends.clone() {
            refuse_a_link_end_naming_a_port_outside_this_stream(link_end, link_end_named)?;
        }
        refuse_a_key_no_such_object_holds(link, &GRAPH_LINK_KEYS, link_named)?;
        for (link_end, link_end_named) in link_ends {
            refuse_a_key_no_such_object_holds(link_end, &GRAPH_LINK_END_KEYS, link_end_named)?;
        }
    }
    Ok(())
}

/// Name each link end of one level that names its node by UUID by that node's name instead,
/// and so for every fragment the level holds, refusing a UUID `node_name_by_uuid` already holds.
fn name_each_link_end_of_one_level_by_its_nodes_name(
    nodes: &mut [GraphSnapshotNode],
    links: &mut [GraphSnapshotLink],
    node_name_by_uuid: &mut HashMap<Uuid, String>,
) -> Result<()> {
    let mut node_name_by_uuid_on_this_level: HashMap<Uuid, String> = HashMap::new();
    for node in nodes.iter_mut() {
        if let Some(node_uuid) = node.id {
            if let Some(holder) = node_name_by_uuid.insert(node_uuid, node.name.clone()) {
                return Err(Error::GraphError(format!(
                    "nodes `{holder}` and `{}` both have the UUID {node_uuid}; a node's UUID \
                     names it alone",
                    node.name
                )));
            }
            node_name_by_uuid_on_this_level.insert(node_uuid, node.name.clone());
        }
        if let Some(fragment) = &mut node.fragment {
            name_each_link_end_of_one_level_by_its_nodes_name(
                &mut fragment.nodes,
                &mut fragment.links,
                node_name_by_uuid,
            )?;
        }
    }
    for link in links {
        for end in [&mut link.source, &mut link.target] {
            if let Some(name) = Uuid::parse_str(&end.node)
                .ok()
                .and_then(|node_uuid| node_name_by_uuid_on_this_level.get(&node_uuid))
            {
                end.node = name.clone();
            }
        }
    }
    Ok(())
}

/// Name each link end of one level by its node's UUID, and so for every fragment the level
/// holds, refusing a node with no UUID.
fn name_each_link_end_of_one_level_by_its_nodes_uuid(
    nodes: &mut [GraphSnapshotNode],
    links: &mut [GraphSnapshotLink],
) -> Result<()> {
    let mut node_uuid_by_name: HashMap<String, Uuid> = HashMap::new();
    for node in nodes.iter_mut() {
        let node_uuid = node.id.ok_or_else(|| {
            Error::GraphError(format!(
                "node `{}` has no UUID, which every node of a graph document has",
                node.name
            ))
        })?;
        node_uuid_by_name.insert(
            cast_exposed_name_to_url_safe(&node.name)?.into_owned(),
            node_uuid,
        );
        if let Some(fragment) = &mut node.fragment {
            name_each_link_end_of_one_level_by_its_nodes_uuid(
                &mut fragment.nodes,
                &mut fragment.links,
            )?;
        }
    }
    for link in links {
        for end in [&mut link.source, &mut link.target] {
            if let Some(node_uuid) =
                node_uuid_by_name.get(cast_exposed_name_to_url_safe(&end.node)?.as_ref())
            {
                end.node = node_uuid.to_string();
            }
        }
    }
    Ok(())
}

/// Refuse `link_end` when it names a machine or a stream, naming each and how
/// a stream reads a port outside it, where `link_end_named` says it sits.
fn refuse_a_link_end_naming_a_port_outside_this_stream(
    link_end: &GraphDocumentObject,
    link_end_named: impl FnOnce() -> String,
) -> Result<()> {
    let what_it_names: Vec<String> = LINK_END_KEYS_NAMING_A_PORT_OUTSIDE_THIS_STREAM
        .into_iter()
        .filter_map(|key| {
            link_end.get(key).map(|named| {
                let named = named
                    .as_str()
                    .map_or_else(|| named.to_string(), str::to_string);
                format!("the {key} `{named}`")
            })
        })
        .collect();
    if what_it_names.is_empty() {
        return Ok(());
    }
    Err(Error::GraphError(format!(
        "{} names {}: {TRACK_RECEIVE_READS_ANOTHER_STREAMS_PORT}",
        link_end_named(),
        what_it_names.join(" and ")
    )))
}

/// Refuse `object`'s first key that is neither a spec key nor a live key of its
/// kind, naming it where `object_named` says it sits.
fn refuse_a_key_no_such_object_holds(
    object: &GraphDocumentObject,
    keys: &GraphDocumentObjectKeys,
    object_named: impl FnOnce() -> String,
) -> Result<()> {
    let Some(unread_key) = object.keys().find(|key| {
        !keys.spec_keys.contains(&key.as_str()) && !keys.live_keys.contains(&key.as_str())
    }) else {
        return Ok(());
    };
    let live_keys_clause = if keys.live_keys.is_empty() {
        String::new()
    } else {
        format!(
            ", beside the live keys `graph` renders on it, which a load skips: {}",
            keys_listed_for_a_refusal(keys.live_keys)
        )
    };
    Err(Error::GraphError(format!(
        "{} carries the key `{unread_key}`, which this runtime does not read. The keys of {} \
         are {}{live_keys_clause}",
        object_named(),
        keys.object_kind,
        keys_listed_for_a_refusal(keys.spec_keys)
    )))
}

/// `keys` as "`a`, `b` and `c`".
fn keys_listed_for_a_refusal(keys: &[&str]) -> String {
    let quoted: Vec<String> = keys.iter().map(|key| format!("`{key}`")).collect();
    match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, before)) => format!("{} and {last}", before.join(", ")),
        None => String::new(),
    }
}

/// Refuse `port` unless `node`'s type declares it in `direction`, listing the
/// ports it does declare there.
pub(crate) fn refuse_a_port_the_node_does_not_have(
    node_types: &NodeTypesOneStreamResolves,
    node_name: &str,
    node: &GraphSnapshotNode,
    port: &str,
    direction: PortDirection,
) -> Result<()> {
    let port_cast = cast_exposed_name_to_url_safe(port)?;
    let port_names: Vec<String> = node_types
        .port_info(&node.processor_type)
        .map(|(inputs, outputs)| match direction {
            PortDirection::Input => inputs,
            PortDirection::Output => outputs,
        })
        .unwrap_or_default()
        .into_iter()
        .map(|declared| declared.name)
        .collect();
    if port_names.iter().any(|name| *name == port_cast) {
        return Ok(());
    }
    Err(Error::GraphError(format!(
        "node `{node_name}` has no {direction} port `{port}`. Its {direction} ports are: {}",
        port_names_listed_for_a_refusal(port_names.iter().map(String::as_str))
    )))
}

/// Refuse an integer literal outside `i64::MIN..=u64::MAX`, which `serde_json`
/// would otherwise read as the nearest `f64`, handing a node another number.
pub(crate) fn refuse_an_integer_literal_wider_than_64_bits(json: &str) -> Result<()> {
    let json_bytes = json.as_bytes();
    let mut byte_index = 0;
    while byte_index < json_bytes.len() {
        match json_bytes[byte_index] {
            b'"' => {
                byte_index += 1;
                while byte_index < json_bytes.len() && json_bytes[byte_index] != b'"' {
                    byte_index += if json_bytes[byte_index] == b'\\' {
                        2
                    } else {
                        1
                    };
                }
                byte_index += 1;
            }
            b'-' | b'0'..=b'9' => {
                let literal_start = byte_index;
                byte_index += 1;
                while byte_index < json_bytes.len()
                    && matches!(
                        json_bytes[byte_index],
                        b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                    )
                {
                    byte_index += 1;
                }
                let number_literal = &json[literal_start..byte_index];
                let integer_digits = number_literal.strip_prefix('-').unwrap_or(number_literal);
                let is_an_integer_literal = !integer_digits.is_empty()
                    && integer_digits.bytes().all(|byte| byte.is_ascii_digit());
                if is_an_integer_literal
                    && number_literal.parse::<i64>().is_err()
                    && number_literal.parse::<u64>().is_err()
                {
                    return Err(Error::GraphError(format!(
                        "the graph does not parse: the integer {number_literal} does not fit in 64 bits"
                    )));
                }
            }
            _ => byte_index += 1,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::processors::PROCESSOR_REGISTRY;

    const A_CAMERA_CLASS: &str = "my_app.nodes:CameraNode";

    /// A camera class registered descriptor-only, with an `frames_in` input and
    /// a `video` output. Idempotent across tests.
    fn a_registered_camera_class() -> &'static str {
        const REGISTERED_CAMERA_CLASS: &str = "graph_snapshot_tests:RegisteredCamera";
        let _ = PROCESSOR_REGISTRY.register_descriptor_only(
            crate::core::descriptors::ProcessorDescriptor::new(
                crate::core::descriptors::ProcessorClassShortName::new("RegisteredCamera").unwrap(),
                ProcessorClassImportPath::new(REGISTERED_CAMERA_CLASS).unwrap(),
                "graph snapshot test",
            )
            .with_input(crate::core::descriptors::PortDescriptor::new(
                "frames_in",
                "",
                false,
                crate::core::descriptors::PortFamilyDeclaration::of_family(
                    crate::core::descriptors::PortFamily::Image,
                ),
            ))
            .with_output(crate::core::descriptors::PortDescriptor::new(
                "video",
                "",
                false,
                crate::core::descriptors::PortFamilyDeclaration::of_family(
                    crate::core::descriptors::PortFamily::Image,
                ),
            )),
        );
        REGISTERED_CAMERA_CLASS
    }

    #[test]
    fn a_graph_document_parses_with_its_live_keys_ignored() {
        let graph_document = serde_json::json!({
            "stream": "main",
            "nodes": [
                {"id": "abc", "name": "camera", "type": A_CAMERA_CLASS, "config": {"fps": 30},
                 "config_checksum": 7, "ports": {"inputs": [], "outputs": []}, "components": {}},
                {"id": "def", "name": "display", "type": "my_app.nodes:DisplayNode", "config": {}}
            ],
            "links": [
                {"id": "l1", "state": "wired", "capacity": 4,
                 "source": {"node": "camera", "port": "video"},
                 "target": {"node": "display", "port": "video"}}
            ],
            "extensions": [],
            "machine": "desk"
        });

        let graph = GraphSnapshot::from_graph_document(graph_document).unwrap();

        assert_eq!(graph.stream.as_deref(), Some("main"));
        assert_eq!(graph.nodes[0].name, "camera");
        assert_eq!(graph.nodes[0].processor_type.as_str(), A_CAMERA_CLASS);
        assert_eq!(graph.nodes[0].config, serde_json::json!({"fps": 30}));
        assert_eq!(
            graph.links[0].source,
            LinkPortRefOutput {
                node: "camera".into(),
                port: "video".into()
            }
        );
    }

    #[test]
    fn a_graph_serializes_exactly_the_spec_keys() {
        let graph = GraphSnapshot {
            asset: None,
            stream: None,
            clock: StreamClockSpec::Real,
            nodes: vec![GraphSnapshotNode {
                id: None,
                name: "camera".into(),
                processor_type: ProcessorClassImportPath::new(A_CAMERA_CLASS).unwrap(),
                config: serde_json::json!({}),
                pins: serde_json::Map::new(),
                exposed_config_fields: Vec::new(),
                studio: serde_json::Map::new(),
                fragment: None,
            }],
            links: vec![],
            groups: vec![],
            forked_from: None,
        };

        let serialized = serde_json::to_value(&graph).unwrap();

        assert_eq!(
            serialized,
            serde_json::json!({
                "nodes": [{"name": "camera", "type": A_CAMERA_CLASS, "config": {}}],
                "links": []
            })
        );
        assert_eq!(
            GraphSnapshot::from_json_str(&graph.to_json_string().unwrap()).unwrap(),
            graph
        );
    }

    fn a_graph_document_of_a_camera_feeding_a_display() -> (Uuid, Uuid, Uuid, String) {
        let asset = Uuid::new_v4();
        let camera = Uuid::new_v4();
        let display = Uuid::new_v4();
        let document = serde_json::json!({
            "asset": asset.to_string(),
            "nodes": [
                {"id": camera.to_string(), "name": "camera", "type": A_CAMERA_CLASS,
                 "config": {}, "pins": {"fps": 30}, "exposed_config_fields": ["exposure"],
                 "studio": {"position": [10, 20]}},
                {"id": display.to_string(), "name": "display", "type": A_CAMERA_CLASS,
                 "config": {}},
            ],
            "links": [{"source": {"node": camera.to_string(), "port": "video"},
                       "target": {"node": display.to_string(), "port": "video"}}],
        });
        (asset, camera, display, document.to_string())
    }

    #[test]
    fn a_graph_document_names_link_ends_by_node_uuid_and_reads_back_by_name() {
        let (asset, camera, display, document) = a_graph_document_of_a_camera_feeding_a_display();

        let graph = GraphSnapshot::from_json_str(&document).unwrap();

        assert_eq!(graph.asset, Some(asset));
        assert_eq!(graph.nodes[0].id, Some(camera));
        assert_eq!(graph.nodes[0].pins["fps"], serde_json::json!(30));
        assert_eq!(
            graph.nodes[0].exposed_config_fields,
            vec!["exposure".to_string()]
        );
        assert_eq!(
            graph.nodes[0].studio["position"],
            serde_json::json!([10, 20])
        );
        assert_eq!(graph.links[0].source.node, "camera");
        assert_eq!(graph.links[0].target.node, "display");

        let written: serde_json::Value =
            serde_json::from_slice(&graph.graph_document_bytes().unwrap()).unwrap();
        assert_eq!(written["links"][0]["source"]["node"], camera.to_string());
        assert_eq!(written["links"][0]["target"]["node"], display.to_string());
        assert_eq!(GraphSnapshot::from_graph_document(written).unwrap(), graph);
    }

    #[test]
    fn a_graph_document_is_written_the_same_bytes_every_time() {
        let (_, _, _, document) = a_graph_document_of_a_camera_feeding_a_display();
        let graph = GraphSnapshot::from_json_str(&document).unwrap();
        let bytes = graph.graph_document_bytes().unwrap();

        assert!(bytes.ends_with(b"}\n"));
        assert_eq!(
            GraphSnapshot::from_json_str(std::str::from_utf8(&bytes).unwrap())
                .unwrap()
                .graph_document_bytes()
                .unwrap(),
            bytes
        );
    }

    #[test]
    fn two_nodes_of_one_uuid_are_refused() {
        let (_, camera, display, document) = a_graph_document_of_a_camera_feeding_a_display();
        let document = document.replace(&display.to_string(), &camera.to_string());

        let refusal = GraphSnapshot::from_json_str(&document)
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("both have the UUID"), "got {refusal}");
    }

    #[test]
    fn a_graph_with_no_asset_or_a_node_with_no_uuid_writes_no_document() {
        let (asset, camera, _, document) = a_graph_document_of_a_camera_feeding_a_display();
        let no_asset = document.replace(&format!("\"asset\":\"{asset}\","), "");
        assert!(
            GraphSnapshot::from_json_str(&no_asset)
                .unwrap()
                .graph_document_bytes()
                .is_err()
        );
        let no_uuid = document.replace(&format!("\"id\":\"{camera}\","), "");
        let refusal = GraphSnapshot::from_json_str(&no_uuid)
            .unwrap()
            .graph_document_bytes()
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("`camera` has no UUID"), "got {refusal}");
    }

    #[test]
    fn a_node_id_that_is_no_uuid_reads_as_no_uuid() {
        let graph = GraphSnapshot::from_json_str(
            &serde_json::json!({"nodes": [{"id": "P1abc", "name": "camera",
                                           "type": A_CAMERA_CLASS, "config": {}}],
                                "links": []})
            .to_string(),
        )
        .unwrap();
        assert_eq!(graph.nodes[0].id, None);
    }

    #[test]
    fn the_retired_alias_shape_does_not_parse() {
        let aliases = r#"{"processors": [{"alias": "camera", "type": "a:B", "config": {}}],
                          "connections": [{"from": "camera.video", "to": "display.video"}]}"#;

        assert!(GraphSnapshot::from_json_str(aliases).is_err());
    }

    #[test]
    fn two_nodes_whose_names_cast_alike_are_refused_naming_the_name() {
        let camera_class = a_registered_camera_class();
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [
                {"name": "FrontCam", "type": camera_class},
                {"name": "frontcam", "type": camera_class}
            ]
        }))
        .unwrap();

        match graph.validate(&NodeTypesOneStreamResolves::new()) {
            Err(Error::NodeNameTaken { name, cast }) => {
                assert_eq!(name, "frontcam");
                assert_eq!(cast, "frontcam");
            }
            other => panic!("expected NodeNameTaken, got {other:?}"),
        }
    }

    #[test]
    fn a_link_naming_a_node_the_graph_does_not_hold_is_refused() {
        let camera_class = a_registered_camera_class();
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "camera", "type": camera_class}],
            "links": [{"source": {"node": "camera", "port": "video"},
                       "target": {"node": "display", "port": "video"}}]
        }))
        .unwrap();

        let refusal = graph
            .validate(&NodeTypesOneStreamResolves::new())
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("`display`"), "{refusal}");
        assert!(refusal.contains("camera"), "{refusal}");
    }

    #[test]
    fn a_link_end_naming_a_port_its_node_does_not_have_in_that_direction_is_refused() {
        let camera_class = a_registered_camera_class();
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "front", "type": camera_class}, {"name": "back", "type": camera_class}],
            "links": [{"source": {"node": "front", "port": "frames_in"},
                       "target": {"node": "back", "port": "frames_in"}}]
        }))
        .unwrap();

        let refusal = graph
            .validate(&NodeTypesOneStreamResolves::new())
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("no output port `frames_in`"), "{refusal}");
        assert!(refusal.contains("video"), "{refusal}");
    }

    #[test]
    fn an_unregistered_type_is_refused_as_unknown() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "ghost", "type": "my_app.nodes:NotARegisteredNode"}]
        }))
        .unwrap();

        match graph.validate(&NodeTypesOneStreamResolves::new()) {
            Err(Error::UnknownProcessorType { ident }) => {
                assert_eq!(ident.as_str(), "my_app.nodes:NotARegisteredNode");
            }
            other => panic!("expected UnknownProcessorType, got {other:?}"),
        }
    }

    /// A stream names no runtime version, so the refusal names this runtime's
    /// own beside the type it lacks.
    #[test]
    fn a_built_in_type_this_runtime_lacks_is_refused_naming_it_and_this_runtimes_version() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "newthing", "type": "tatolab.stream:NewThing"}]
        }))
        .unwrap();

        let refusal = graph
            .validate(&NodeTypesOneStreamResolves::new())
            .unwrap_err()
            .to_string();

        assert_eq!(
            refusal,
            format!(
                "this runtime ({}) has no node type `tatolab.stream:NewThing`",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn a_built_in_that_moved_to_a_module_is_refused_naming_the_module_to_install() {
        let moved_out = ProcessorClassImportPath::of_built_in_node("GraphSnapshotTestMovedOut");
        PROCESSOR_REGISTRY.register_built_in_node_type_moved_to_a_module(
            moved_out.clone(),
            ProcessorClassImportPath::new("tatolab.snapshottest:GraphSnapshotTestMovedOut")
                .unwrap(),
            "tatolab-snapshottest",
        );
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "moved", "type": moved_out.as_str()}]
        }))
        .unwrap();

        let refusal = graph
            .validate(&NodeTypesOneStreamResolves::new())
            .unwrap_err();

        assert!(
            matches!(&refusal, Error::BuiltInNodeTypeMovedToAModule { node_type, .. } if *node_type == moved_out),
            "{refusal:?}"
        );
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("`tatolab.snapshottest:GraphSnapshotTestMovedOut`"),
            "{refusal}"
        );
        assert!(
            refusal.contains("pip install tatolab-snapshottest"),
            "{refusal}"
        );
    }

    #[test]
    fn a_setting_the_nodes_type_does_not_take_is_refused_naming_the_node_its_type_and_the_setting()
    {
        crate::core::test_support::ensure_test_mocks_registered();
        let source_type =
            crate::core::test_support::MockSourceTakingOneSetting::processor_class_import_path();
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "Front", "type": source_type.as_str(),
                       "config": {"frame_widht": 640}}]
        }))
        .unwrap();

        match graph.validate(&NodeTypesOneStreamResolves::new()) {
            Err(Error::NodeConfigRefused {
                node_name,
                node_type,
                refusal,
            }) => {
                assert_eq!(node_name, "Front");
                assert_eq!(node_type, source_type);
                assert!(refusal.contains("`frame_widht`"), "{refusal}");
                assert!(refusal.contains("`frame_width`"), "{refusal}");
            }
            other => panic!("expected NodeConfigRefused, got {other:?}"),
        }
    }

    #[test]
    fn a_setting_the_nodes_type_takes_passes() {
        crate::core::test_support::ensure_test_mocks_registered();
        let source_type =
            crate::core::test_support::MockSourceTakingOneSetting::processor_class_import_path();
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "Front", "type": source_type.as_str(),
                       "config": {"frame_width": 640}},
                      {"name": "Back", "type": source_type.as_str()}]
        }))
        .unwrap();

        graph.validate(&NodeTypesOneStreamResolves::new()).unwrap();
    }

    #[test]
    fn a_node_written_without_a_config_takes_the_config_naming_no_setting() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "camera", "type": A_CAMERA_CLASS}]
        }))
        .unwrap();

        assert_eq!(graph.nodes[0].config, serde_json::json!({}));
    }

    /// A misspelled key is read past by serde and the graph loads as something
    /// its author did not write, so every key that is neither read nor one of
    /// `graph`'s live keys is refused, at every level, naming where it sits.
    #[test]
    fn a_key_this_runtime_does_not_read_is_refused_naming_it_and_where_it_sits() {
        let a_node = serde_json::json!({"name": "camera", "type": A_CAMERA_CLASS});
        let a_link = serde_json::json!({"source": {"node": "camera", "port": "video"},
                                        "target": {"node": "display", "port": "video"}});
        let with = |mut object: serde_json::Value, key: &str| {
            object[key] = serde_json::json!(1);
            object
        };
        for (graph_document, refusal_names) in [
            (
                serde_json::json!({"nodes": [], "linkz": []}),
                [
                    "the graph carries the key `linkz`",
                    "`asset`, `stream`, `clock`, `nodes`, `links`, `groups` and `forked_from`",
                ],
            ),
            (
                serde_json::json!({"nodes": [with(a_node.clone(), "confg")]}),
                [
                    "node `camera` carries the key `confg`",
                    "`id`, `name`, `type`, `config`, `pins`, `exposed_config_fields`, `studio` and `fragment`",
                ],
            ),
            (
                serde_json::json!({"nodes": [{"name": "camera", "typ": A_CAMERA_CLASS}]}),
                [
                    "node `camera` carries the key `typ`",
                    "`id`, `name`, `type`, `config`, `pins`, `exposed_config_fields`, `studio` and `fragment`",
                ],
            ),
            (
                serde_json::json!({"nodes": [a_node.clone()], "links": [with(a_link.clone(), "sourec")]}),
                [
                    "the link `camera/video` → `display/video` carries the key `sourec`",
                    "`source` and `target`",
                ],
            ),
            (
                serde_json::json!({"nodes": [a_node.clone()], "links": [{
                    "source": {"node": "camera", "port": "video", "prot": "x"},
                    "target": {"node": "display", "port": "video"}}]}),
                [
                    "the source end of the link `camera/video` → `display/video` carries the key `prot`",
                    "`node` and `port`",
                ],
            ),
        ] {
            let refusal = GraphSnapshot::from_graph_document(graph_document)
                .expect_err("a key the runtime does not read is refused")
                .to_string();
            for named in refusal_names {
                assert!(refusal.contains(named), "{named:?} not in: {refusal}");
            }
        }
    }

    #[test]
    fn a_key_this_runtime_does_not_read_is_refused_from_a_json_string_too() {
        let refusal = GraphSnapshot::from_json_str(r#"{"nodes": [], "nodez": []}"#)
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("`nodez`"), "{refusal}");
    }

    /// `graph` renders spec and live keys together and a load reads it back, so
    /// every key the render can carry has to be one a load reads or skips, and
    /// every spec key one the render carries.
    #[test]
    fn every_key_graph_renders_is_one_a_load_reads_or_skips() {
        let render_schema = serde_json::to_value(schemars::schema_for!(
            crate::core::json_schema::GraphResponse
        ))
        .unwrap();
        let definitions = render_schema
            .get("definitions")
            .or_else(|| render_schema.get("$defs"))
            .expect("the render's schema defines its nested objects");
        let keys_rendered_on = |object_schema: &serde_json::Value| -> HashSet<String> {
            object_schema["properties"]
                .as_object()
                .expect("an object schema lists its properties")
                .keys()
                .cloned()
                .collect()
        };

        for (object_schema, keys) in [
            (&render_schema, &GRAPH_DOCUMENT_KEYS),
            (&definitions["ProcessorNodeOutput"], &GRAPH_NODE_KEYS),
            (&definitions["LinkOutput"], &GRAPH_LINK_KEYS),
            (&definitions["LinkPortRefOutput"], &GRAPH_LINK_END_KEYS),
        ] {
            let rendered = keys_rendered_on(object_schema);
            for key in &rendered {
                assert!(
                    keys.spec_keys.contains(&key.as_str())
                        || keys.live_keys.contains(&key.as_str()),
                    "`graph` renders `{key}` on {}, which a load neither reads nor skips",
                    keys.object_kind
                );
            }
            for spec_key in keys.spec_keys {
                assert!(
                    rendered.contains(*spec_key),
                    "a load reads `{spec_key}` on {}, which `graph` does not render",
                    keys.object_kind
                );
            }
        }
    }

    /// A type described in the stream validates against the stream's own
    /// ports; a stream that described nothing refuses it as unknown.
    #[test]
    fn a_type_described_in_the_stream_validates_against_that_streams_ports() {
        let described_class = "graph_snapshot_tests.nodes:DescribedInTheStream";
        let node_types = NodeTypesOneStreamResolves::new();
        node_types
            .register_a_type_described_in_this_streams_processor_interpreter(
                crate::core::descriptors::ProcessorDescriptor::new(
                    crate::core::descriptors::ProcessorClassShortName::new("DescribedInTheStream")
                        .unwrap(),
                    ProcessorClassImportPath::new(described_class).unwrap(),
                    "graph snapshot test",
                )
                .with_output(crate::core::descriptors::PortDescriptor::new(
                    "video",
                    "",
                    false,
                    crate::core::descriptors::PortFamilyDeclaration::of_family(
                        crate::core::descriptors::PortFamily::Image,
                    ),
                )),
                crate::core::processors::HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(|_node| Err(Error::NotSupported("never constructed".into())))),
            )
            .expect("the type is described into the stream");
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "described", "type": described_class}]
        }))
        .unwrap();

        graph
            .validate(&node_types)
            .expect("the stream resolves the type it described");
        assert!(
            matches!(
                graph.validate(&NodeTypesOneStreamResolves::new()),
                Err(Error::UnknownProcessorType { .. })
            ),
            "a stream that described nothing resolved another stream's type"
        );
    }

    const GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS: &str = "graph_snapshot_tests.nodes:Gateway";
    const TRACK_CLASS_FOR_THE_REFERENCE_TESTS: &str = "graph_snapshot_tests.nodes:Track";
    const SEND_CLASS_FOR_THE_REFERENCE_TESTS: &str = "graph_snapshot_reference_tests:Send";

    /// A stream that described a gateway type and a track type whose config
    /// key `gateway` takes a reference to a gateway.
    fn node_types_with_a_track_naming_its_gateway_by_reference() -> NodeTypesOneStreamResolves {
        let node_types = NodeTypesOneStreamResolves::new();
        for (short_name, import_path, config_schema) in [
            ("Gateway", GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS, None),
            (
                "Track",
                TRACK_CLASS_FOR_THE_REFERENCE_TESTS,
                Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "gateway": {
                            streamlib_processor_schema::NODE_REFERENCE_SCHEMA_KEYWORD:
                                {"node_type": GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS}
                        },
                        "track": {"type": "string"},
                        "gateway_or_its_name": {"anyOf": [
                            {streamlib_processor_schema::NODE_REFERENCE_SCHEMA_KEYWORD: {}},
                            {"type": "string"}
                        ]}
                    }
                })),
            ),
            (
                "Send",
                SEND_CLASS_FOR_THE_REFERENCE_TESTS,
                Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "track": {
                            streamlib_processor_schema::NODE_REFERENCE_SCHEMA_KEYWORD: {
                                "node_type": TRACK_CLASS_FOR_THE_REFERENCE_TESTS,
                                streamlib_processor_schema::NODE_REFERENCE_SCHEMA_ONE_REFERRER_PER_REFERENCED_NODE_KEY: true,
                            }
                        }
                    }
                })),
            ),
        ] {
            let mut descriptor = crate::core::descriptors::ProcessorDescriptor::new(
                crate::core::descriptors::ProcessorClassShortName::new(short_name).unwrap(),
                ProcessorClassImportPath::new(import_path).unwrap(),
                "graph snapshot reference test",
            );
            if let Some(config_schema) = config_schema {
                descriptor = descriptor.with_config_schema(config_schema);
            }
            node_types
                .register_a_type_described_in_this_streams_processor_interpreter(
                    descriptor,
                    crate::core::processors::HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(|_node| Err(Error::NotSupported("never constructed".into())))),
                )
                .expect("the type is described into the stream");
        }
        node_types
    }

    fn a_graph_whose_track_names_in_its_gateway_key(
        gateway_config_value: serde_json::Value,
    ) -> GraphSnapshot {
        GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [
                {"name": "cloudflare", "type": GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS},
                {"name": "other-track", "type": TRACK_CLASS_FOR_THE_REFERENCE_TESTS},
                {
                    "name": "camera-track",
                    "type": TRACK_CLASS_FOR_THE_REFERENCE_TESTS,
                    "config": {"gateway": gateway_config_value, "track": "camera"}
                }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn a_reference_to_a_node_of_the_type_its_key_takes_validates_and_round_trips() {
        let graph =
            a_graph_whose_track_names_in_its_gateway_key(serde_json::json!({"node": "cloudflare"}));
        graph
            .validate(&node_types_with_a_track_naming_its_gateway_by_reference())
            .expect("a track naming a gateway validates");
        let reread = GraphSnapshot::from_json_str(&graph.to_json_string().unwrap()).unwrap();
        assert_eq!(reread, graph);
        assert_eq!(
            reread.nodes[2].config["gateway"],
            serde_json::json!({"node": "cloudflare"})
        );
    }

    #[test]
    fn a_second_node_naming_a_node_under_a_key_taking_one_referrer_is_refused_naming_both() {
        let graph_with_senders_naming = |second_send_track: &str| {
            GraphSnapshot::from_graph_document(serde_json::json!({
                "nodes": [
                    {"name": "cloudflare", "type": GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS},
                    {"name": "camera-track", "type": TRACK_CLASS_FOR_THE_REFERENCE_TESTS,
                     "config": {"gateway": {"node": "cloudflare"}}},
                    {"name": "screen-track", "type": TRACK_CLASS_FOR_THE_REFERENCE_TESTS,
                     "config": {"gateway": {"node": "cloudflare"}}},
                    {"name": "camera-send", "type": SEND_CLASS_FOR_THE_REFERENCE_TESTS,
                     "config": {"track": {"node": "camera-track"}}},
                    {"name": "second-send", "type": SEND_CLASS_FOR_THE_REFERENCE_TESTS,
                     "config": {"track": {"node": second_send_track}}}
                ]
            }))
            .unwrap()
        };
        let node_types = node_types_with_a_track_naming_its_gateway_by_reference();
        graph_with_senders_naming("screen-track")
            .validate(&node_types)
            .expect("two senders on two tracks validate");
        let refusal = graph_with_senders_naming("Camera Track")
            .validate(&node_types)
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("node `second-send`'s config key `track` names node `Camera Track`")
                && refusal.contains("node `camera-send` already names under `track`"),
            "{refusal}"
        );
    }

    #[test]
    fn a_reference_to_a_node_the_graph_does_not_hold_is_refused_naming_it() {
        let refusal =
            a_graph_whose_track_names_in_its_gateway_key(serde_json::json!({"node": "nowhere"}))
                .validate(&node_types_with_a_track_naming_its_gateway_by_reference())
                .unwrap_err()
                .to_string();
        assert!(
            refusal.contains("node `camera-track`'s config key `gateway` names node `nowhere`")
                && refusal.contains("cloudflare"),
            "{refusal}"
        );
    }

    #[test]
    fn a_reference_to_a_node_of_the_wrong_type_is_refused_naming_both_types() {
        let refusal = a_graph_whose_track_names_in_its_gateway_key(
            serde_json::json!({"node": "other-track"}),
        )
        .validate(&node_types_with_a_track_naming_its_gateway_by_reference())
        .unwrap_err()
        .to_string();
        assert!(
            refusal.contains(TRACK_CLASS_FOR_THE_REFERENCE_TESTS)
                && refusal.contains(&format!(
                    "takes a `{GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS}`"
                )),
            "{refusal}"
        );
    }

    #[test]
    fn a_reference_key_holding_anything_but_a_node_object_is_refused_naming_the_key() {
        let refusal = a_graph_whose_track_names_in_its_gateway_key(serde_json::json!("cloudflare"))
            .validate(&node_types_with_a_track_naming_its_gateway_by_reference())
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("config key `gateway` holds \"cloudflare\""),
            "{refusal}"
        );
    }

    #[test]
    fn a_key_taking_a_reference_or_a_string_validates_a_string() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{
                "name": "camera-track",
                "type": TRACK_CLASS_FOR_THE_REFERENCE_TESTS,
                "config": {"gateway_or_its_name": "cloudflare"}
            }]
        }))
        .unwrap();
        graph
            .validate(&node_types_with_a_track_naming_its_gateway_by_reference())
            .expect("a string is the key's other value");
    }

    #[test]
    fn a_setter_no_get_reads_still_holds_a_node_the_graph_holds() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [
                {"name": "cloudflare", "type": GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS},
                {
                    "name": "set-gateway",
                    "type": "tatolab.stream:SetVariable",
                    "config": {"variable": "gateway", "node_reference": {"node": "nowhere"}}
                }
            ]
        }))
        .unwrap();
        let refusal = graph
            .validate(&node_types_with_a_track_naming_its_gateway_by_reference())
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("`SetVariable` node `set-gateway` holds node `nowhere`")
                && refusal.contains("cloudflare"),
            "{refusal}"
        );
    }

    #[test]
    fn a_node_object_under_a_key_no_schema_marks_is_plain_config() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{
                "name": "cloudflare",
                "type": GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS,
                "config": {"anything": {"node": "nowhere"}}
            }]
        }))
        .unwrap();
        graph
            .validate(&node_types_with_a_track_naming_its_gateway_by_reference())
            .expect("a key no schema marks is never read as a reference");
    }

    /// A link's ends are both in the stream that loads the graph, so an end
    /// naming a stream — this one or another — or a machine is refused naming
    /// the key and `MoqTrackReceive` as how a stream reads a port outside it.
    #[test]
    fn a_link_end_naming_a_stream_or_a_machine_is_refused_naming_the_track_receive() {
        for (source, target, the_end_and_what_it_names) in [
            (
                serde_json::json!({"stream": "camera-rig", "node": "camerasource", "port": "video"}),
                serde_json::json!({"node": "camera", "port": "frames_in"}),
                "the source end of the link `camera-rig/camerasource/video` → `camera/frames_in` \
                 names the stream `camera-rig`",
            ),
            (
                serde_json::json!({"machine": "desk", "node": "camera", "port": "video"}),
                serde_json::json!({"node": "camera", "port": "frames_in"}),
                "the source end of the link `desk/camera/video` → `camera/frames_in` names the \
                 machine `desk`",
            ),
            (
                serde_json::json!({"machine": "rig", "stream": "a", "node": "camera",
                                   "port": "video"}),
                serde_json::json!({"node": "camera", "port": "frames_in"}),
                "the source end of the link `rig/a/camera/video` → `camera/frames_in` names the \
                 machine `rig` and the stream `a`",
            ),
            (
                serde_json::json!({"node": "camera", "port": "video"}),
                serde_json::json!({"stream": "main", "node": "camera", "port": "frames_in"}),
                "the target end of the link `camera/video` → `main/camera/frames_in` names the \
                 stream `main`",
            ),
        ] {
            let refusal = GraphSnapshot::from_graph_document(serde_json::json!({
                "nodes": [{"name": "camera", "type": a_registered_camera_class()}],
                "links": [{"source": source, "target": target}]
            }))
            .expect_err("a link end naming a stream or a machine is refused at load")
            .to_string();
            assert_eq!(
                refusal,
                format!(
                    "Stream graph error: {the_end_and_what_it_names}: \
                     {TRACK_RECEIVE_READS_ANOTHER_STREAMS_PORT}"
                )
            );
        }
    }

    /// A graph an older runtime rendered, holding a link to another stream in
    /// the state that runtime gave it, is refused naming how to read that
    /// stream's port, not for the key it rendered on the link.
    #[test]
    fn a_link_to_another_stream_as_an_older_runtime_rendered_it_is_refused_naming_the_track_receive()
     {
        let refusal = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "camera", "type": a_registered_camera_class()}],
            "links": [{
                "source": {"stream": "camera-rig", "node": "camerasource", "port": "video"},
                "target": {"node": "camera", "port": "frames_in"},
                "state": "awaiting_remote",
                "awaiting_remote_reason": "the stream `camera-rig` is not loaded on this machine",
            }]
        }))
        .expect_err("a link to another stream is refused at load")
        .to_string();
        assert!(
            refusal.contains("names the stream `camera-rig`")
                && refusal.contains("`MoqTrackReceive` node"),
            "{refusal}"
        );
    }

    /// A link end is spelled `{node, port}` and no other key: one carrying the
    /// `runtime_name` a retired render wrote is refused, naming the keys a link
    /// end is written with.
    #[test]
    fn a_link_end_carrying_a_key_besides_node_and_port_is_refused_naming_the_keys_of_a_link_end() {
        let refusal = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "camera", "type": A_CAMERA_CLASS}],
            "links": [{"source": {"runtime_name": "rig", "node": "camera", "port": "video"},
                       "target": {"node": "camera", "port": "frames_in"}}]
        }))
        .expect_err("a link end carrying `runtime_name` is refused")
        .to_string();
        for named in [
            "the source end of the link `camera/video` → `camera/frames_in`",
            "`runtime_name`",
            "`node` and `port`",
        ] {
            assert!(refusal.contains(named), "{named:?} not in: {refusal}");
        }
    }

    #[test]
    fn an_empty_graph_is_valid() {
        let graph = GraphSnapshot::from_json_str(r#"{"nodes": []}"#).unwrap();

        assert!(graph.validate(&NodeTypesOneStreamResolves::new()).is_ok());
        assert!(graph.links.is_empty());
    }
    #[test]
    fn an_integer_wider_than_64_bits_is_refused_naming_it_rather_than_read_as_a_float() {
        let refusal = GraphSnapshot::from_json_str(
            r#"{"nodes": [{"name": "sink", "type": "a:B", "config": {"value": 18446744073709551616}}]}"#,
        )
        .expect_err("an integer past u64::MAX is refused");
        assert!(
            refusal
                .to_string()
                .contains("the integer 18446744073709551616 does not fit in 64 bits"),
            "{refusal}"
        );
        let refusal = GraphSnapshot::from_json_str(
            r#"{"nodes": [{"name": "sink", "type": "a:B", "config": {"value": -9223372036854775809}}]}"#,
        )
        .expect_err("an integer below i64::MIN is refused");
        assert!(
            refusal.to_string().contains("-9223372036854775809"),
            "{refusal}"
        );
    }

    #[test]
    fn every_64_bit_integer_a_float_and_digits_inside_a_string_still_parse() {
        let graph = GraphSnapshot::from_json_str(
            r#"{"nodes": [{"name": "sink", "type": "a:B", "config": {
                "largest": 18446744073709551615,
                "smallest": -9223372036854775808,
                "float": 1.8446744073709552e19,
                "digits": "18446744073709551616",
                "escaped": "\"18446744073709551616\""
            }}]}"#,
        )
        .expect("each value fits");
        let config = &graph.nodes[0].config;
        assert_eq!(config["largest"], serde_json::json!(u64::MAX));
        assert_eq!(config["smallest"], serde_json::json!(i64::MIN));
        assert_eq!(config["digits"], "18446744073709551616");
    }

    const IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS: &str = "graph_snapshot_tests:ImageSource";
    const PACKETS_SINK_CLASS_FOR_THE_INTERFACE_TESTS: &str = "graph_snapshot_tests:PacketsSink";

    fn node_types_with_an_image_source_and_a_packets_sink() -> NodeTypesOneStreamResolves {
        use crate::core::descriptors::{PortDescriptor, PortFamily, PortFamilyDeclaration};
        let node_types = NodeTypesOneStreamResolves::new();
        for (short_name, import_path, port) in [
            (
                "ImageSource",
                IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS,
                PortDescriptor::new(
                    "video",
                    "",
                    false,
                    PortFamilyDeclaration::of_family(PortFamily::Image),
                ),
            ),
            (
                "PacketsSink",
                PACKETS_SINK_CLASS_FOR_THE_INTERFACE_TESTS,
                PortDescriptor::new(
                    "encoded",
                    "",
                    false,
                    PortFamilyDeclaration {
                        requires_codec: vec!["h264".into()],
                        ..PortFamilyDeclaration::of_family(PortFamily::Packets)
                    },
                ),
            ),
        ] {
            let descriptor = crate::core::descriptors::ProcessorDescriptor::new(
                crate::core::descriptors::ProcessorClassShortName::new(short_name).unwrap(),
                ProcessorClassImportPath::new(import_path).unwrap(),
                "graph snapshot interface test",
            );
            let descriptor = if import_path == IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS {
                descriptor.with_output(port)
            } else {
                descriptor.with_input(port)
            };
            node_types
                .register_a_type_described_in_this_streams_processor_interpreter(
                    descriptor,
                    crate::core::processors::HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(|_node| Err(Error::NotSupported("never constructed".into())))),
                )
                .expect("the type is described into the stream");
        }
        node_types
    }

    fn a_graph_offering_the_source_and_feeding_the_sink(
        output_family: &str,
        input_family: &str,
    ) -> GraphSnapshot {
        GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [
                {"name": "source", "type": IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS},
                {"name": "video", "type": "tatolab.stream:GraphOutput",
                 "config": {"port": "video", "family": output_family}},
                {"name": "encoded", "type": "tatolab.stream:GraphInput",
                 "config": {"port": "encoded", "family": input_family}},
                {"name": "sink", "type": PACKETS_SINK_CLASS_FOR_THE_INTERFACE_TESTS},
            ],
            "links": [
                {"source": {"node": "source", "port": "video"},
                 "target": {"node": "video", "port": "value"}},
                {"source": {"node": "encoded", "port": "value"},
                 "target": {"node": "sink", "port": "encoded"}},
            ]
        }))
        .unwrap()
    }

    #[test]
    fn interface_ports_of_the_family_their_linked_ports_carry_validate() {
        a_graph_offering_the_source_and_feeding_the_sink("image", "packets")
            .validate(&node_types_with_an_image_source_and_a_packets_sink())
            .expect("an Image Output, and a Packets Input into a sink requiring a codec, validate");
    }

    #[test]
    fn an_interface_port_of_another_family_than_its_linked_port_is_refused_naming_both() {
        let node_types = node_types_with_an_image_source_and_a_packets_sink();
        let output_refusal = a_graph_offering_the_source_and_feeding_the_sink("array", "packets")
            .validate(&node_types)
            .unwrap_err()
            .to_string();
        assert!(
            output_refusal.contains("`GraphOutput` node `video` declares `video` as Array")
                && output_refusal.contains("source.video (Image)"),
            "{output_refusal}"
        );
        let input_refusal = a_graph_offering_the_source_and_feeding_the_sink("image", "records")
            .validate(&node_types)
            .unwrap_err()
            .to_string();
        assert!(
            input_refusal.contains("`GraphInput` node `encoded` declares `encoded` as Records")
                && input_refusal.contains("sink.encoded (Packets)"),
            "{input_refusal}"
        );
    }

    #[test]
    fn an_interface_node_linked_to_a_port_its_node_does_not_have_is_refused_naming_the_ports() {
        let mut graph = serde_json::to_value(a_graph_offering_the_source_and_feeding_the_sink(
            "image", "packets",
        ))
        .unwrap();
        graph["links"][0]["source"]["port"] = serde_json::json!("vidoe");
        let refusal = GraphSnapshot::from_graph_document(graph)
            .unwrap()
            .validate(&node_types_with_an_image_source_and_a_packets_sink())
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("node `source` has no output port `vidoe`")
                && refusal.contains("video"),
            "{refusal}"
        );
    }

    #[test]
    fn a_variable_set_from_a_graph_input_validates() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [
                {"name": "encoded", "type": "tatolab.stream:GraphInput",
                 "config": {"port": "encoded", "family": "packets"}},
                {"name": "set-encoded", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "encoded"}},
                {"name": "get-encoded", "type": "tatolab.stream:GetVariable",
                 "config": {"variable": "encoded"}},
                {"name": "sink", "type": PACKETS_SINK_CLASS_FOR_THE_INTERFACE_TESTS},
            ],
            "links": [
                {"source": {"node": "encoded", "port": "value"},
                 "target": {"node": "set-encoded", "port": "value"}},
                {"source": {"node": "get-encoded", "port": "value"},
                 "target": {"node": "sink", "port": "encoded"}},
            ]
        }))
        .unwrap();
        graph
            .validate(&node_types_with_an_image_source_and_a_packets_sink())
            .expect("a GraphInput held in a variable and read by the sink validates");
    }

    fn a_variable_set_from(held_node: &str, held_port: &str) -> GraphSnapshot {
        GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [
                {"name": "source", "type": IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS},
                {"name": "video", "type": "tatolab.stream:GraphOutput",
                 "config": {"port": "video", "family": "image"}},
                {"name": "encoded", "type": "tatolab.stream:GraphInput",
                 "config": {"port": "encoded", "family": "packets"}},
                {"name": "set-unread", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "unread"}},
            ],
            "links": [
                {"source": {"node": "source", "port": "video"},
                 "target": {"node": "video", "port": "value"}},
                {"source": {"node": held_node, "port": held_port},
                 "target": {"node": "set-unread", "port": "value"}},
            ]
        }))
        .unwrap()
    }

    #[test]
    fn a_variable_set_from_an_interface_port_other_than_a_graph_inputs_value_is_refused() {
        let node_types = node_types_with_an_image_source_and_a_packets_sink();
        a_variable_set_from("encoded", "value")
            .validate(&node_types)
            .expect("an unread variable set from a GraphInput's value validates");
        for (held_node, held_port) in [("encoded", "typo"), ("video", "value")] {
            let refusal = a_variable_set_from(held_node, held_port)
                .validate(&node_types)
                .expect_err("the setter holds no port the graph holds")
                .to_string();
            assert!(
                refusal.contains("`SetVariable` node `set-unread`")
                    && refusal.contains(&format!("`{held_node}/{held_port}`")),
                "{refusal}"
            );
        }
    }

    #[test]
    fn removing_a_node_a_link_to_an_interface_or_variable_node_names_is_refused_naming_the_link() {
        let resolution = a_variable_set_from("encoded", "value")
            .with_graph_only_nodes_resolved(&NodeTypesOneStreamResolves::new())
            .expect("resolves");
        let graph_only_nodes = &resolution.graph_only_nodes_as_drawn;
        let refusal = graph_only_nodes
            .refuse_a_node_change_a_drawn_link_names("Source", NodeChangeADrawnLinkBlocks::Removal)
            .expect_err("the GraphOutput's link names the source")
            .to_string();
        assert!(
            refusal.contains("node `Source` cannot be removed")
                && refusal.contains("`source/video` to `video/value`"),
            "{refusal}"
        );
        assert_eq!(
            graph_only_nodes.node_names_cast(),
            HashSet::from(["video", "encoded", "set-unread"].map(String::from))
        );
    }

    const A_GROUP_UUID: &str = "5e1f9c2a-0d8b-4c47-9a35-7a3b2f1c6d10";

    fn a_camera_graph_document(groups: serde_json::Value) -> serde_json::Value {
        let mut document = serde_json::json!({
            "nodes": [
                {"id": "0b7d3c1e-6a2f-4e98-b5c4-1d9e8f7a6b52", "name": "camera",
                 "type": A_CAMERA_CLASS, "config": {}, "studio": {"group": A_GROUP_UUID}},
                {"id": "7f4e2d9a-1c3b-4a5e-8d6f-0e9c8b7a6d54", "name": "look",
                 "type": "generators.look:look", "config": {},
                 "fragment": {
                     "nodes": [{"id": "9a8b7c6d-5e4f-4a3b-9c2d-1e0f9a8b7c65", "name": "blur",
                                "type": A_CAMERA_CLASS, "config": {},
                                "studio": {"group": "2c6a1f7e-93b4-4d0a-8e15-b6f0c3a9d421"}}],
                     "links": [],
                     "groups": [{"id": "2c6a1f7e-93b4-4d0a-8e15-b6f0c3a9d421",
                                 "title": "soften"}],
                 }},
            ],
            "links": [],
        });
        if !groups.is_null() {
            document["groups"] = groups;
        }
        document
    }

    #[test]
    fn a_graph_with_groups_compiles_to_the_graph_it_compiles_to_without_them() {
        let grouped = GraphSnapshot::from_graph_document(a_camera_graph_document(
            serde_json::json!([{"id": A_GROUP_UUID, "title": "sources",
                                "description": "every camera", "colour": "#3b82f6"}]),
        ))
        .unwrap();
        let ungrouped =
            GraphSnapshot::from_graph_document(a_camera_graph_document(serde_json::Value::Null))
                .unwrap();

        let grouped_resolved = grouped
            .with_graph_only_nodes_resolved(&NodeTypesOneStreamResolves::new())
            .unwrap();
        let ungrouped_resolved = ungrouped
            .with_graph_only_nodes_resolved(&NodeTypesOneStreamResolves::new())
            .unwrap();

        assert_eq!(
            grouped_resolved.graph_the_runtime_runs,
            ungrouped_resolved.graph_the_runtime_runs
        );
        assert!(grouped_resolved.graph_the_runtime_runs.groups.is_empty());
        assert_eq!(
            grouped_resolved.graph_only_nodes_as_drawn.groups_as_drawn,
            grouped.groups
        );
        assert_eq!(grouped.groups[0].title, "sources");
        assert_eq!(
            grouped.nodes[1].fragment.as_ref().unwrap().groups[0].title,
            "soften"
        );
    }

    #[test]
    fn a_group_whose_id_is_no_uuid_is_refused_naming_its_title_and_level() {
        let mut document = a_camera_graph_document(serde_json::Value::Null);
        document["nodes"][1]["fragment"]["groups"][0]["id"] = serde_json::json!("soften");

        let refusal = GraphSnapshot::from_graph_document(document)
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("group `soften`"), "{refusal}");
        assert!(refusal.contains("fragment `look`"), "{refusal}");
    }

    #[test]
    fn a_graph_with_groups_writes_them_into_its_graph_document() {
        let graph = GraphSnapshot {
            asset: Some(Uuid::new_v4()),
            ..GraphSnapshot::from_graph_document(a_camera_graph_document(serde_json::json!([
                {"id": A_GROUP_UUID, "title": "sources"}
            ])))
            .unwrap()
        };

        let document: serde_json::Value =
            serde_json::from_slice(&graph.graph_document_bytes().unwrap()).unwrap();

        assert_eq!(
            document["groups"],
            serde_json::json!([{"id": A_GROUP_UUID, "title": "sources"}])
        );
        assert_eq!(GraphSnapshot::from_graph_document(document).unwrap(), graph);
    }
}
