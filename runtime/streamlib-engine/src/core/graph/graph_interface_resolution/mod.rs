// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A graph's interface: each `GraphInput` and `GraphOutput` node declares one
//! port the graph offers, named and of one family.
//!
//! A stream's top-level `GraphInput` of a family a client writes runs as a
//! source node, kept with its links in the graph the runtime runs. Every other
//! interface node runs nothing: the graph the runtime runs holds neither the
//! node nor its links. What each declares is kept beside it, so `graph`
//! renders the interface back and Tato Studio reads a stream's Inputs and
//! Outputs.

use std::collections::HashMap;

use crate::core::descriptors::{
    NodeClassImportPath, NodeValueLifetime, PinFlow, PinLinkEnd, PinLinkInputEnd, PinNumberRange,
    PinType, PortFamily, PortFamilyDeclaration, PortFamilyLinkEndpoint, ResolvedPin,
    pin_link_verdict, refusal_of_a_link_between_port_families,
};
use crate::core::graph::graph_snapshot::{
    GraphSnapshot, GraphSnapshotLink, GraphSnapshotNode, refuse_a_port_the_node_does_not_have,
    the_ports_of_a_graph_document_node,
};
use crate::core::graph::input_values::the_read_of_an_input_a_link_feeds;
use crate::core::graph::legacy_config_of_a_node::{
    config_of_a_node, config_of_a_node_holding_no_pin_value,
};
use crate::core::graph::{
    GraphNodePorts, InputNumberRange, cast_exposed_name_to_url_safe,
    the_refusal_of_a_link_into_a_setting, the_values_port_facts_read,
};
use crate::core::json_schema::{
    GraphLinkComponentsOnTheWire, GraphNodeComponentsOnTheWire, GraphNodeOutput,
    GraphNodePortOnTheWire, GraphNodePortsOnTheWire, GraphResponse, InputValuesRendered,
    LinkOutput, LinkPortRefOutput, PortKindOnTheWire,
};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::{Error, PortDirection, Result};
use streamlib_node_schema::output_port_family_resolved_against_config;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphInterfacePortDirectionOnTheWire, GraphInterfacePortOnTheWire,
    InputValueComesFromOnTheWire, InputValueReadOnTheWire,
};

use interface_node_config::{
    WhatAnInterfaceNodeDeclares, refuse_a_config_key_the_interface_node_does_not_take,
    what_an_interface_nodes_settings_declare,
};

pub use streamlib_project_repository::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_FAMILY_CONFIG_KEY, GRAPH_INTERFACE_PORT_CONFIG_KEY,
    GRAPH_INTERFACE_VALUE_PORT_NAME, GRAPH_OUTPUT_NODE_TYPE, is_a_graph_interface_node_type,
    is_the_graph_input_type,
};

/// The name a top-level `GraphInput`'s `value` input had: a stream saved before it was an input
/// holds its saved value under this key.
pub const GRAPH_INPUT_VALUE_FORMER_NAME: &str = "default";

/// The setting of a `GraphInput` and a `GraphOutput` that chooses the pin type of its `value`
/// port; unset, its `family` setting decides it.
pub const GRAPH_INTERFACE_PIN_TYPE_SETTING: &str = "pin_type";

/// The pin type the interface node holding `input_values` chooses with its `pin_type` setting;
/// `None` while it holds none. Refused when it holds no pin type, or one only a setting takes,
/// which no link carries.
pub fn the_pin_type_an_interface_node_chooses(
    input_values: Option<&serde_json::Map<String, serde_json::Value>>,
) -> std::result::Result<Option<PinType>, String> {
    let Some(held) = input_values
        .and_then(|input_values| input_values.get(GRAPH_INTERFACE_PIN_TYPE_SETTING))
        .filter(|held| !held.is_null())
    else {
        return Ok(None);
    };
    let chosen: PinType = serde_json::from_value(held.clone()).map_err(|why| {
        format!(
            "its `{GRAPH_INTERFACE_PIN_TYPE_SETTING}` is {held}, and a pin type choice holds a \
             pin type, `{{\"type\": \"<name>\"}}`: {why}"
        )
    })?;
    if matches!(
        chosen,
        PinType::NodeReference { .. } | PinType::PinTypeChoice { .. }
    ) {
        return Err(format!(
            "its `{GRAPH_INTERFACE_PIN_TYPE_SETTING}` chooses {chosen}, which only a setting \
             holds, and a stream's port carries what a link carries"
        ));
    }
    Ok(Some(chosen))
}

/// The family a port of the chosen `pin_type` travels on; refused for a type no family's
/// message carries, such as a List or a bundle.
pub fn the_family_a_chosen_pin_type_travels_on(
    pin_type: &PinType,
) -> std::result::Result<PortFamily, String> {
    PortFamily::carrying(pin_type).ok_or_else(|| {
        format!(
            "its `{GRAPH_INTERFACE_PIN_TYPE_SETTING}` chooses {pin_type}, which no stream's port \
             carries yet. To fix: choose a type a single message carries, such as a Float, a \
             String or a Texture"
        )
    })
}

/// The config key of a top-level `GraphInput` holding `[lowest, highest]`, the span a control
/// moves its number in.
pub const GRAPH_INPUT_RANGE_CONFIG_KEY: &str = "range";

/// The config key of a top-level `GraphInput` naming how long a value set live on its `value`
/// input lasts.
pub const GRAPH_INPUT_LIFETIME_CONFIG_KEY: &str = "lifetime";

/// The lifetime a top-level `GraphInput`'s config declares for the value a client writes,
/// `kept` when it declares none.
pub fn the_value_lifetime_a_graph_input_config_declares(
    config: Option<&serde_json::Value>,
) -> std::result::Result<NodeValueLifetime, String> {
    let Some(declared) = config.and_then(|config| config.get(GRAPH_INPUT_LIFETIME_CONFIG_KEY))
    else {
        return Ok(NodeValueLifetime::Kept);
    };
    declared
        .as_str()
        .and_then(NodeValueLifetime::named)
        .ok_or_else(|| {
            format!(
                "its `{GRAPH_INPUT_LIFETIME_CONFIG_KEY}` is {declared}, and a lifetime is \"kept\" or \"reset\""
            )
        })
}

/// Whether the graph being resolved is a stream's top level, whose `GraphInput`s a client
/// writes, or a fragment's, whose `GraphInput`s the placing graph's links enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphInterfaceLevel {
    TheStreamsTopLevel,
    AFragment,
}

/// Whether a `GraphInput` on the stream's top level that carries `family` and chooses
/// `pin_type` runs as a source node: one of a type that holds a value a client sets, travelling
/// on a family a client writes a value of; one choosing no type, of such a family.
pub fn a_top_level_graph_input_runs(
    family: Option<PortFamily>,
    pin_type: Option<&PinType>,
) -> bool {
    let a_family_a_client_writes = family.is_some_and(|family| {
        matches!(
            family,
            PortFamily::Array | PortFamily::Channels | PortFamily::Records
        )
    });
    a_family_a_client_writes && pin_type.is_none_or(PinType::holds_a_stored_value)
}

/// Which way a declared port faces the graph that adds this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphInterfacePortDirection {
    Input,
    Output,
}

/// One port a graph's interface declares.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphInterfacePort {
    /// The interface node's name, as the graph wrote it.
    pub interface_node: String,
    /// The port's name, as the interface node's config wrote it.
    pub port: String,
    /// The port's family: the one its chosen pin type travels on, else the one its `family`
    /// setting names, else the one the first port linked to it carries; `None` while it states
    /// none and nothing is linked, when the node runs nothing, and for a chosen type no family
    /// carries.
    pub family: Option<PortFamily>,
    /// The pin type its `pin_type` setting chooses; `None` while it chooses none.
    pub pin_type: Option<PinType>,
    pub direction: GraphInterfacePortDirection,
    /// A `GraphOutput`'s one source port; a `GraphInput`'s every reader.
    pub linked_ports: Vec<LinkPortRefOutput>,
    /// Whether the interface node runs, as a top-level `GraphInput` a client writes does.
    pub runs_as_a_source_node: bool,
}

/// One interface node as drawn, with the port it declares.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphInterfaceNodeAsDrawn {
    pub node: GraphSnapshotNode,
    /// Where the node sat among the graph's nodes.
    pub position_among_the_graphs_nodes: usize,
    pub declared_port: GraphInterfacePort,
}

impl GraphInterfaceNodeAsDrawn {
    /// Render this running `GraphInput` over the node `graph_response` renders for it, as an
    /// interface node renders, keeping what it carries running and its `value` input with the
    /// value it reads; and mark each link out of it with the port it declares.
    fn render_over_its_running_node_in(&self, graph_response: &mut GraphResponse) {
        let node_cast = cast_or_as_written(&self.node.name);
        if let Some(running_node) = graph_response
            .nodes
            .iter_mut()
            .find(|rendered| cast_or_as_written(&rendered.name) == node_cast)
        {
            let mut rendered_as_an_interface_node = self.rendered_as_a_node();
            rendered_as_an_interface_node.components = GraphNodeComponentsOnTheWire {
                graph_interface: rendered_as_an_interface_node.components.graph_interface,
                ..std::mem::take(&mut running_node.components)
            };
            rendered_as_an_interface_node.id = std::mem::take(&mut running_node.id);
            rendered_as_an_interface_node.ports.inputs =
                std::mem::take(&mut running_node.ports.inputs);
            *running_node = rendered_as_an_interface_node;
        }
        for link in graph_response
            .links
            .iter_mut()
            .filter(|link| cast_or_as_written(&link.source.node) == node_cast)
        {
            link.components.graph_interface = Some(self.declared_port.as_component());
        }
    }

    /// The interface node as `graph` renders it: its settings `port`, `pin_type` and `family`
    /// as inputs, each reading the value it holds, and its one port of the type and family it
    /// declares; a `GraphOutput`'s reads from a wire once a link feeds it.
    pub fn rendered_as_a_node(&self) -> GraphNodeOutput {
        let declared = &self.declared_port;
        let mut value_port = declared.rendered_as_a_port_named(GRAPH_INTERFACE_VALUE_PORT_NAME);
        if declared.direction == GraphInterfacePortDirection::Output
            && !declared.linked_ports.is_empty()
        {
            value_port.value_read = Some(the_read_of_an_input_a_link_feeds());
        }
        let mut inputs = self.its_settings_rendered();
        let outputs = match declared.direction {
            GraphInterfacePortDirection::Output => {
                inputs.push(value_port);
                Vec::new()
            }
            GraphInterfacePortDirection::Input => vec![value_port],
        };
        let components = GraphNodeComponentsOnTheWire {
            graph_interface: Some(declared.as_component()),
            ..GraphNodeComponentsOnTheWire::default()
        };
        let values = InputValuesRendered::of_a_node_holding_no_pin_value_whose_config_renders_as(
            &self.node.input_values,
            the_config_a_graph_interface_node_renders(
                &self.node.node_type,
                config_of_a_node_holding_no_pin_value(&self.node.input_values),
            ),
        );
        GraphNodeOutput {
            id: self
                .node
                .id
                .map(|node_uuid| node_uuid.to_string())
                .unwrap_or_default(),
            node_type: self.node.node_type.clone(),
            kind: None,
            language: None,
            name: cast_or_as_written(&self.node.name),
            config: Some(values.config),
            config_checksum: values.config_checksum,
            ports: GraphNodePortsOnTheWire { inputs, outputs },
            input_values: values.input_values,
            pins: values.pins,
            exposed_config_fields: self.node.exposed_config_fields.clone(),
            studio: self.node.studio.clone(),
            inputs_waiting_for_a_value: Vec::new(),
            components,
            fragment: None,
            live: None,
        }
    }
}

impl GraphInterfaceNodeAsDrawn {
    /// The settings every interface node takes, `port`, `pin_type` and `family`, as its type
    /// declares them, each reading the value the node holds, else the default it declares.
    fn its_settings_rendered(&self) -> Vec<GraphNodePortOnTheWire> {
        let descriptor = match self.declared_port.direction {
            GraphInterfacePortDirection::Input => crate::core::nodes::the_node_type_of_a_graph_input(),
            GraphInterfacePortDirection::Output => {
                crate::core::nodes::the_node_type_of_a_graph_output()
            }
        };
        let Ok(descriptor) = descriptor else {
            return Vec::new();
        };
        descriptor
            .inputs
            .iter()
            .filter(|setting| {
                [
                    GRAPH_INTERFACE_PORT_CONFIG_KEY,
                    GRAPH_INTERFACE_PIN_TYPE_SETTING,
                    GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
                ]
                .contains(&setting.name.as_str())
            })
            .map(|setting| {
                let mut rendered =
                    GraphNodePortOnTheWire::from(&crate::core::graph::PortInfo::from(setting));
                let read = self
                    .node
                    .input_values
                    .get(&setting.name)
                    .filter(|held| !held.is_null())
                    .map(|held| (held.clone(), InputValueComesFromOnTheWire::Saved))
                    .or_else(|| {
                        setting
                            .pin_default
                            .clone()
                            .map(|default| (default, InputValueComesFromOnTheWire::Default))
                    });
                rendered.value_read = Some(match read {
                    Some((value, comes_from)) => InputValueReadOnTheWire {
                        value: Some(value),
                        value_comes_from: Some(comes_from),
                        member_values: None,
                    },
                    None => InputValueReadOnTheWire {
                        value: Some(serde_json::Value::Null),
                        value_comes_from: None,
                        member_values: None,
                    },
                });
                rendered
            })
            .collect()
    }
}

/// The config an interface node of `node_type` holding `config` renders: a `GraphInput`'s saved
/// `value`, else the `default` a config written before its value was an input holds, under
/// `default`, as [`render_the_saved_value_of_a_graph_input_as_its_default`] puts it.
pub(crate) fn the_config_a_graph_interface_node_renders(
    node_type: &NodeClassImportPath,
    mut config: serde_json::Value,
) -> serde_json::Value {
    if !is_the_graph_input_type(node_type) {
        return config;
    }
    if let Some(config) = config.as_object_mut() {
        let saved_value = config
            .remove(GRAPH_INTERFACE_VALUE_PORT_NAME)
            .or_else(|| config.remove(GRAPH_INPUT_VALUE_FORMER_NAME));
        render_the_saved_value_of_a_graph_input_as_its_default(config, saved_value);
    }
    config
}

/// Render `saved_value`, the saved value of a `GraphInput`'s `value` input, in its rendered
/// `config` under `default`, the key a published Tato Studio reads it by; a `GraphInput` saving
/// none renders no `default`.
pub(crate) fn render_the_saved_value_of_a_graph_input_as_its_default(
    config: &mut serde_json::Map<String, serde_json::Value>,
    saved_value: Option<serde_json::Value>,
) {
    match saved_value {
        Some(saved_value) => {
            config.insert(GRAPH_INPUT_VALUE_FORMER_NAME.to_string(), saved_value);
        }
        None => {
            config.remove(GRAPH_INPUT_VALUE_FORMER_NAME);
        }
    }
}

/// The family a `GraphInput`'s `config` names; `None` when it names none, or a name no family
/// has.
pub(crate) fn the_family_a_graph_input_config_names(
    config: Option<&serde_json::Value>,
) -> Option<PortFamily> {
    config?
        .get(GRAPH_INTERFACE_FAMILY_CONFIG_KEY)?
        .as_str()
        .and_then(PortFamily::of_wire_name)
}

/// The range a `GraphInput`'s `config` declares a number of its `value` takes, as
/// `[lowest, highest]`; `None` when it declares none.
fn the_range_a_graph_input_config_declares(
    config: Option<&serde_json::Value>,
) -> Option<PinNumberRange> {
    match config?
        .get(GRAPH_INPUT_RANGE_CONFIG_KEY)?
        .as_array()?
        .as_slice()
    {
        [lowest, highest] => Some(PinNumberRange {
            minimum: lowest.as_f64(),
            maximum: highest.as_f64(),
        }),
        _ => None,
    }
}

/// Resolve the `value` ports among `ports`, of a `GraphInput` whose settings are `settings`,
/// against what those settings declare of them: the pin type it chooses and the family that type
/// travels on, else the family it names; and on its `value` input, the range a number takes and
/// the lifetime of a value set live. Settings choosing no type and naming no valid family give
/// both the family the `GraphInput` type declares, so each settings resolve them alike.
pub(crate) fn resolve_the_value_input_a_graph_input_config_declares(
    ports: &mut GraphNodePorts,
    settings: Option<&serde_json::Value>,
) {
    let chosen = the_pin_type_an_interface_node_chooses(
        settings.and_then(serde_json::Value::as_object),
    )
    .ok()
    .flatten();
    let family = chosen
        .as_ref()
        .and_then(PortFamily::carrying)
        .or_else(|| the_family_a_graph_input_config_names(settings))
        .unwrap_or(crate::core::nodes::THE_FAMILY_A_GRAPH_INPUT_DECLARES);
    for value in ports
        .inputs
        .iter_mut()
        .chain(&mut ports.outputs)
        .filter(|port| port.name == GRAPH_INTERFACE_VALUE_PORT_NAME)
    {
        value.family = PortFamilyDeclaration::of_family(family);
        value.pin = chosen.clone().map(|pin_type| ResolvedPin {
            pin_type,
            flow: PinFlow::Value,
        });
    }
    if let Some(value) = ports
        .inputs
        .iter_mut()
        .find(|port| port.name == GRAPH_INTERFACE_VALUE_PORT_NAME)
    {
        value.range = the_range_a_graph_input_config_declares(settings).map(InputNumberRange);
        value.lifetime =
            the_value_lifetime_a_graph_input_config_declares(settings).unwrap_or_default();
    }
}

/// One link with an end on an interface node, as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTouchingAnInterfaceNodeAsDrawn {
    pub link: GraphSnapshotLink,
    /// Where the link sat among the graph's links.
    pub position_among_the_graphs_links: usize,
}

/// What a graph's interface nodes resolved to, and what was drawn, so `graph`
/// renders them back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphInterfaceAsDrawn {
    /// Every interface node, in the order the graph drew them.
    pub interface_nodes: Vec<GraphInterfaceNodeAsDrawn>,
    pub links_touching_an_interface_node: Vec<LinkTouchingAnInterfaceNodeAsDrawn>,
}

/// A graph with its interface nodes resolved out of it.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphSnapshotWithInterfaceResolved {
    /// The graph the runtime runs, holding no interface node.
    pub graph_the_runtime_runs: GraphSnapshot,
    pub interface_as_drawn: GraphInterfaceAsDrawn,
}

impl GraphSnapshot {
    /// Resolve this graph's interface nodes, at `level`, out of it, keeping a top-level
    /// `GraphInput` a client writes and its links in it. An interface node whose config names
    /// no family takes the one `family_of_a_linked_port` gives the first port linked to it, and
    /// runs nothing while nothing is linked. Refuses by name two nodes whose names cast alike,
    /// an interface node naming no port, a config key it does not take, two declaring one port
    /// name in one direction, a `GraphOutput` of a family linked from no port or from several,
    /// and a link into a `GraphInput` or out of a `GraphOutput`.
    pub fn with_graph_interface_resolved(
        &self,
        level: GraphInterfaceLevel,
        family_of_a_linked_port: &dyn Fn(
            &LinkPortRefOutput,
            GraphInterfacePortDirection,
        ) -> Option<PortFamily>,
    ) -> Result<GraphSnapshotWithInterfaceResolved> {
        if !self
            .nodes
            .iter()
            .any(|node| is_a_graph_interface_node_type(node.node_type.as_str()))
        {
            return Ok(GraphSnapshotWithInterfaceResolved {
                graph_the_runtime_runs: self.clone(),
                interface_as_drawn: GraphInterfaceAsDrawn::default(),
            });
        }

        let mut direction_of_each_interface_node_by_cast = HashMap::new();
        let mut family_of_each_interface_node_by_cast: HashMap<String, Option<PortFamily>> =
            HashMap::new();
        let mut running_graph_input_casts = std::collections::HashSet::new();
        let mut node_name_by_cast: HashMap<String, &str> = HashMap::new();
        for node in &self.nodes {
            let cast = cast_of(&node.name)?;
            if node_name_by_cast.insert(cast.clone(), &node.name).is_some() {
                return Err(Error::NodeNameTaken {
                    name: node.name.clone(),
                    cast,
                });
            }
            let direction = match node.node_type.as_str() {
                GRAPH_INPUT_NODE_TYPE => GraphInterfacePortDirection::Input,
                GRAPH_OUTPUT_NODE_TYPE => GraphInterfacePortDirection::Output,
                _ => continue,
            };
            let declared = what_an_interface_nodes_settings_declare(node)?;
            let family = if declared.states_what_the_port_carries() {
                declared.family
            } else {
                self.the_first_port_linked_to_the_interface_node(&cast, direction)?
                    .and_then(|linked_port| family_of_a_linked_port(&linked_port, direction))
            };
            refuse_a_config_key_the_interface_node_does_not_take(
                node,
                level,
                family,
                declared.pin_type.as_ref(),
            )?;
            if direction == GraphInterfacePortDirection::Input
                && level == GraphInterfaceLevel::TheStreamsTopLevel
                && a_top_level_graph_input_runs(family, declared.pin_type.as_ref())
            {
                running_graph_input_casts.insert(cast.clone());
            }
            family_of_each_interface_node_by_cast.insert(cast.clone(), family);
            direction_of_each_interface_node_by_cast.insert(cast, direction);
        }

        let mut runtime_links = Vec::new();
        let mut links_touching_an_interface_node = Vec::new();
        let mut linked_ports_by_interface_node_cast: HashMap<String, Vec<LinkPortRefOutput>> =
            HashMap::new();
        for (link_position, link) in self.links.iter().enumerate() {
            let source_direction =
                direction_of_each_interface_node_by_cast.get(&cast_of(&link.source.node)?);
            let target_direction =
                direction_of_each_interface_node_by_cast.get(&cast_of(&link.target.node)?);
            if source_direction.is_none() && target_direction.is_none() {
                runtime_links.push(link.clone());
                continue;
            }
            if source_direction == Some(&GraphInterfacePortDirection::Output) {
                return Err(Error::GraphError(format!(
                    "a link leaves `GraphOutput` node `{}`, which has no output: a \
                     `GraphOutput` is where a port leaves the graph",
                    link.source.node
                )));
            }
            if target_direction == Some(&GraphInterfacePortDirection::Input) {
                // A link into a top-level `GraphInput` that runs meets, at its connect, the one
                // rule every link into a setting meets.
                if source_direction.is_none()
                    && running_graph_input_casts.contains(&cast_of(&link.target.node)?)
                {
                    runtime_links.push(link.clone());
                    continue;
                }
                return Err(the_refusal_of_a_link_into_a_setting(
                    &link.target.port,
                    &link.target.node,
                ));
            }
            if source_direction.is_some() && target_direction.is_some() {
                return Err(Error::GraphError(format!(
                    "`GraphInput` node `{}` is linked straight to `GraphOutput` node `{}`; \
                     a graph's interface declares ports of nodes that run",
                    link.source.node, link.target.node
                )));
            }
            let (interface_end, other_end) = if source_direction.is_some() {
                (&link.source, &link.target)
            } else {
                (&link.target, &link.source)
            };
            refuse_an_interface_port_other_than_value(interface_end)?;
            let interface_node_cast = cast_of(&interface_end.node)?;
            let interface_node_runs = running_graph_input_casts.contains(&interface_node_cast);
            linked_ports_by_interface_node_cast
                .entry(interface_node_cast)
                .or_default()
                .push(other_end.clone());
            if interface_node_runs {
                runtime_links.push(link.clone());
                continue;
            }
            links_touching_an_interface_node.push(LinkTouchingAnInterfaceNodeAsDrawn {
                link: link.clone(),
                position_among_the_graphs_links: link_position,
            });
        }

        let mut interface_nodes = Vec::new();
        let mut runtime_nodes = Vec::new();
        let mut interface_node_by_port_cast: HashMap<(String, bool), String> = HashMap::new();
        for (node_position, node) in self.nodes.iter().enumerate() {
            let node_cast = cast_of(&node.name)?;
            let Some(&direction) = direction_of_each_interface_node_by_cast.get(&node_cast) else {
                runtime_nodes.push(node.clone());
                continue;
            };
            let WhatAnInterfaceNodeDeclares {
                port,
                family: _,
                pin_type,
            } = what_an_interface_nodes_settings_declare(node)?;
            let states_what_it_carries =
                pin_type.is_some() || node_names_a_family(node);
            let family = family_of_each_interface_node_by_cast
                .get(&node_cast)
                .copied()
                .flatten();
            let runs_as_a_source_node = running_graph_input_casts.contains(&node_cast);
            if runs_as_a_source_node {
                runtime_nodes.push(match (states_what_it_carries, family) {
                    (false, Some(resolved_family)) => {
                        node_with_the_family_a_link_resolved(node, resolved_family)
                    }
                    _ => node.clone(),
                });
            }
            let port_cast = cast_of(&port)?;
            let is_an_output = direction == GraphInterfacePortDirection::Output;
            if let Some(first) = interface_node_by_port_cast
                .insert((port_cast.clone(), is_an_output), node.name.clone())
            {
                return Err(Error::GraphError(format!(
                    "`{first}` and `{}` both declare the graph's {} `{port_cast}`, and a graph \
                     declares each port once",
                    node.name,
                    if is_an_output { "Output" } else { "Input" }
                )));
            }
            let linked_ports = linked_ports_by_interface_node_cast
                .remove(&node_cast)
                .unwrap_or_default();
            if let (None, None, Some(linked_port)) = (family, &pin_type, linked_ports.first()) {
                return Err(Error::GraphError(format!(
                    "`{}` node `{}` names no family, and `{}/{}` linked to it states none for \
                     it to take; name one in its config's `{GRAPH_INTERFACE_FAMILY_CONFIG_KEY}`",
                    if is_an_output {
                        "GraphOutput"
                    } else {
                        "GraphInput"
                    },
                    node.name,
                    linked_port.node,
                    linked_port.port
                )));
            }
            let is_waiting_for_a_link_to_resolve_its_family =
                family.is_none() && pin_type.is_none();
            if is_an_output
                && linked_ports.len() != 1
                && !is_waiting_for_a_link_to_resolve_its_family
            {
                return Err(Error::GraphError(format!(
                    "`GraphOutput` node `{}` declares the Output `{port}` from {} links; link \
                     exactly one port into its `{GRAPH_INTERFACE_VALUE_PORT_NAME}` input",
                    node.name,
                    linked_ports.len()
                )));
            }
            interface_nodes.push(GraphInterfaceNodeAsDrawn {
                node: node.clone(),
                position_among_the_graphs_nodes: node_position,
                declared_port: GraphInterfacePort {
                    interface_node: node.name.clone(),
                    port,
                    family,
                    pin_type,
                    direction,
                    linked_ports,
                    runs_as_a_source_node,
                },
            });
        }

        Ok(GraphSnapshotWithInterfaceResolved {
            graph_the_runtime_runs: GraphSnapshot {
                asset: self.asset,
                display_name: self.display_name.clone(),
                stream: self.stream.clone(),
                clock: self.clock,
                nodes: runtime_nodes,
                links: runtime_links,
                groups: self.groups.clone(),
                studio: self.studio.clone(),
            },
            interface_as_drawn: GraphInterfaceAsDrawn {
                interface_nodes,
                links_touching_an_interface_node,
            },
        })
    }
}

impl GraphSnapshot {
    /// The other end of the first link that meets the interface node cast `interface_node_cast`
    /// from its `direction`'s side: a `GraphInput`'s first reader, a `GraphOutput`'s source.
    fn the_first_port_linked_to_the_interface_node(
        &self,
        interface_node_cast: &str,
        direction: GraphInterfacePortDirection,
    ) -> Result<Option<LinkPortRefOutput>> {
        for link in &self.links {
            let (interface_end, other_end) = match direction {
                GraphInterfacePortDirection::Input => (&link.source, &link.target),
                GraphInterfacePortDirection::Output => (&link.target, &link.source),
            };
            if cast_of(&interface_end.node)? == interface_node_cast {
                return Ok(Some(other_end.clone()));
            }
        }
        Ok(None)
    }
}

/// The family the port `linked_port` of `graph` carries to or from an interface node as
/// `node_types` declare it: a reader's input family, or a source's output family resolved
/// against its node's config and followed upstream through an output whose family follows one
/// of its inputs; `None` when nothing states one.
pub fn the_family_a_port_linked_to_an_interface_node_carries(
    node_types: &NodeTypesOneStreamResolves,
    graph: &GraphSnapshot,
    linked_port: &LinkPortRefOutput,
    direction_of_the_interface_port: GraphInterfacePortDirection,
) -> Option<PortFamily> {
    let mut port_to_read = linked_port.clone();
    let mut direction_of_the_port_to_read = direction_of_the_interface_port;
    let mut ports_read = std::collections::HashSet::new();
    loop {
        if !ports_read.insert((port_to_read.node.clone(), port_to_read.port.clone())) {
            return None;
        }
        let node_cast = cast_of(&port_to_read.node).ok()?;
        let node = graph
            .nodes
            .iter()
            .find(|node| cast_of(&node.name).is_ok_and(|cast| cast == node_cast))?;
        if is_a_graph_interface_node_type(node.node_type.as_str()) {
            let declared = what_an_interface_nodes_settings_declare(node).ok()?;
            if declared.states_what_the_port_carries()
                || node.node_type.as_str() != GRAPH_INPUT_NODE_TYPE
            {
                return declared.family;
            }
            port_to_read = graph
                .the_first_port_linked_to_the_interface_node(
                    &node_cast,
                    GraphInterfacePortDirection::Input,
                )
                .ok()??;
            direction_of_the_port_to_read = GraphInterfacePortDirection::Input;
            continue;
        }
        let (inputs, outputs) =
            the_ports_of_a_graph_document_node(node_types, &port_to_read.node, node).ok()??;
        let port_cast = cast_of(&port_to_read.port).ok()?;
        if direction_of_the_port_to_read == GraphInterfacePortDirection::Input {
            return inputs
                .into_iter()
                .find(|port| port.name == port_cast)
                .map(|port| port.family.family);
        }
        let output = outputs.into_iter().find(|port| port.name == port_cast)?;
        let Some(followed_input) = output.family.family_follows_input_port.clone() else {
            let config = config_of_a_node(node_types, &node.node_type, &node.input_values);
            let values_read =
                the_values_port_facts_read(Some(&config), &inputs, &node.input_values);
            return output_port_family_resolved_against_config(
                &output.family,
                values_read.as_ref(),
            )
            .ok();
        };
        let followed_input_cast = cast_of(&followed_input).ok()?;
        port_to_read = graph
            .links
            .iter()
            .find(|link| {
                cast_of(&link.target.node).is_ok_and(|cast| cast == node_cast)
                    && cast_of(&link.target.port).is_ok_and(|cast| cast == followed_input_cast)
            })?
            .source
            .clone();
    }
}

/// Whether `node`, an interface node, names a family in its `family` setting.
fn node_names_a_family(node: &GraphSnapshotNode) -> bool {
    node.input_values
        .get(GRAPH_INTERFACE_FAMILY_CONFIG_KEY)
        .is_some_and(|family| !family.is_null())
}

/// `node`, a top-level `GraphInput` whose config names no family, with `resolved_family` in its
/// config, so the node it runs as carries the family a link resolved.
fn node_with_the_family_a_link_resolved(
    node: &GraphSnapshotNode,
    resolved_family: PortFamily,
) -> GraphSnapshotNode {
    let mut node_with_the_family = node.clone();
    node_with_the_family.input_values.insert(
        GRAPH_INTERFACE_FAMILY_CONFIG_KEY.to_string(),
        serde_json::Value::String(resolved_family.wire_name().to_string()),
    );
    node_with_the_family
}

impl GraphInterfaceAsDrawn {
    /// Every port the interface declares, in the order its nodes were drawn.
    pub fn declared_ports(&self) -> impl Iterator<Item = &GraphInterfacePort> {
        self.interface_nodes
            .iter()
            .map(|interface_node| &interface_node.declared_port)
    }

    /// The interface node `node_name` names, if it names one.
    pub fn interface_node_named(&self, node_name: &str) -> Option<&GraphInterfaceNodeAsDrawn> {
        let cast = cast_of(node_name).ok()?;
        self.interface_nodes.iter().find(|interface_node| {
            cast_of(&interface_node.node.name).is_ok_and(|name| name == cast)
        })
    }

    /// Refuse a port the interface declares whose linked port is not a port of
    /// the right direction on a node of `graph_the_runtime_runs`, or that the
    /// link rules refuse between it and a port of the declared family.
    pub fn validate(
        &self,
        graph_the_runtime_runs: &GraphSnapshot,
        node_types: &NodeTypesOneStreamResolves,
    ) -> Result<()> {
        for declared in self.declared_ports() {
            for linked_port in &declared.linked_ports {
                let linked_cast = cast_of(&linked_port.node)?;
                let Some(linked_node) = graph_the_runtime_runs
                    .nodes
                    .iter()
                    .find(|node| cast_of(&node.name).is_ok_and(|cast| cast == linked_cast))
                else {
                    return Err(Error::GraphError(format!(
                        "interface node `{}` is linked to node `{}`, which the graph does not \
                         hold",
                        declared.interface_node, linked_port.node
                    )));
                };
                declared.refuse_a_linked_port_that_does_not_meet_it(
                    node_types,
                    linked_node,
                    linked_port,
                    &config_of_a_node(
                        node_types,
                        &linked_node.node_type,
                        &linked_node.input_values,
                    ),
                )?;
            }
        }
        Ok(())
    }

    /// Refuse a config update for `node` under which a port the interface
    /// declares would no longer meet the port of `node` linked to it.
    pub fn refuse_a_config_update_that_breaks_a_declared_port(
        &self,
        node_types: &NodeTypesOneStreamResolves,
        node: &GraphSnapshotNode,
        config_to_apply: &serde_json::Value,
    ) -> Result<()> {
        let node_cast = cast_of(&node.name)?;
        for declared in self.declared_ports() {
            for linked_port in &declared.linked_ports {
                if cast_of(&linked_port.node)? != node_cast {
                    continue;
                }
                declared
                    .refuse_a_linked_port_that_does_not_meet_it(
                        node_types,
                        node,
                        linked_port,
                        config_to_apply,
                    )
                    .map_err(|refusal| {
                        Error::Config(format!(
                            "config update for {} refused: {refusal}",
                            node.name
                        ))
                    })?;
            }
        }
        Ok(())
    }

    /// Render the interface into `graph_response`, a render of the graph the
    /// runtime runs: each interface node and each drawn link with an end on
    /// one back at its drawn position.
    pub fn render_into(&self, graph_response: &mut GraphResponse) {
        for drawn in &self.links_touching_an_interface_node {
            let position = drawn
                .position_among_the_graphs_links
                .min(graph_response.links.len());
            let components = GraphLinkComponentsOnTheWire {
                graph_interface: self
                    .port_a_link_meets(&drawn.link)
                    .map(GraphInterfacePort::as_component),
                ..GraphLinkComponentsOnTheWire::default()
            };
            graph_response.links.insert(
                position,
                LinkOutput {
                    source: LinkPortRefOutput {
                        node: cast_or_as_written(&drawn.link.source.node),
                        port: drawn.link.source.port.clone(),
                        member: drawn.link.source.member.clone(),
                    },
                    target: LinkPortRefOutput {
                        node: cast_or_as_written(&drawn.link.target.node),
                        port: drawn.link.target.port.clone(),
                        member: drawn.link.target.member.clone(),
                    },
                    components,
                    ..LinkOutput::default()
                },
            );
        }
        for interface_node in &self.interface_nodes {
            if interface_node.declared_port.runs_as_a_source_node {
                interface_node.render_over_its_running_node_in(graph_response);
                continue;
            }
            let position = interface_node
                .position_among_the_graphs_nodes
                .min(graph_response.nodes.len());
            graph_response
                .nodes
                .insert(position, interface_node.rendered_as_a_node());
        }
    }

    fn port_a_link_meets(&self, link: &GraphSnapshotLink) -> Option<&GraphInterfacePort> {
        let source_cast = cast_or_as_written(&link.source.node);
        let target_cast = cast_or_as_written(&link.target.node);
        self.declared_ports().find(|declared| {
            let interface_cast = cast_or_as_written(&declared.interface_node);
            interface_cast == source_cast || interface_cast == target_cast
        })
    }
}

impl GraphInterfacePort {
    /// Refuse `linked_port` of `linked_node`, configured as `linked_node_config`,
    /// unless it is a port of the right direction the link rules let meet a
    /// port of this one's family: a `GraphOutput`'s source must write it, and
    /// each reader of a `GraphInput` must take it.
    fn refuse_a_linked_port_that_does_not_meet_it(
        &self,
        node_types: &NodeTypesOneStreamResolves,
        linked_node: &GraphSnapshotNode,
        linked_port: &LinkPortRefOutput,
        linked_node_config: &serde_json::Value,
    ) -> Result<()> {
        let direction_of_the_linked_port = match self.direction {
            GraphInterfacePortDirection::Output => PortDirection::Output,
            GraphInterfacePortDirection::Input => PortDirection::Input,
        };
        refuse_a_port_the_node_does_not_have(
            node_types,
            &linked_port.node,
            linked_node,
            &linked_port.port,
            direction_of_the_linked_port,
        )?;
        let port_cast = cast_of(&linked_port.port)?;
        let Some((inputs, outputs)) =
            the_ports_of_a_graph_document_node(node_types, &linked_port.node, linked_node)?
        else {
            return Ok(());
        };
        let linked_declaration = match self.direction {
            GraphInterfacePortDirection::Output => &outputs,
            GraphInterfacePortDirection::Input => &inputs,
        }
        .iter()
        .find(|port| port.name == port_cast)
        .map(|port| port.family.clone())
        .map(|declaration| match self.direction {
            GraphInterfacePortDirection::Output => declaration,
            GraphInterfacePortDirection::Input => {
                the_input_without_what_only_the_graphs_producer_can_meet(declaration)
            }
        });
        if let Some(refusal) =
            self.refusal_of_a_linked_port_by_the_chosen_type(&inputs, &outputs, linked_port)
        {
            return Err(Error::GraphError(format!(
                "`{}` node `{}` chooses {} for `{}`, and the link rules refuse it: {refusal}",
                self.interface_node_kind(),
                self.interface_node,
                self.pin_type
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                self.port
            )));
        }
        let (Some(linked_declaration), Some(family)) = (linked_declaration, self.family) else {
            return Ok(());
        };
        let declared_family = PortFamilyDeclaration::of_family(family);
        let interface_end = PortFamilyLinkEndpoint {
            node_name: &self.interface_node,
            port_name: GRAPH_INTERFACE_VALUE_PORT_NAME,
            declaration: &declared_family,
        };
        let linked_end = PortFamilyLinkEndpoint {
            node_name: &linked_port.node,
            port_name: &linked_port.port,
            declaration: &linked_declaration,
        };
        let refusal = match self.direction {
            GraphInterfacePortDirection::Output => refusal_of_a_link_between_port_families(
                linked_end,
                Some(linked_node_config),
                interface_end,
            ),
            GraphInterfacePortDirection::Input => {
                refusal_of_a_link_between_port_families(interface_end, None, linked_end)
            }
        };
        match refusal {
            Some(refusal) => Err(Error::GraphError(format!(
                "`{}` node `{}` declares `{}` as {}, and the link rules refuse it: {refusal}",
                match self.direction {
                    GraphInterfacePortDirection::Output => "GraphOutput",
                    GraphInterfacePortDirection::Input => "GraphInput",
                },
                self.interface_node,
                self.port,
                family
            ))),
            None => Ok(()),
        }
    }

    /// `GraphInput` or `GraphOutput`, the kind of node that declares this port.
    fn interface_node_kind(&self) -> &'static str {
        match self.direction {
            GraphInterfacePortDirection::Output => "GraphOutput",
            GraphInterfacePortDirection::Input => "GraphInput",
        }
    }

    /// The pin type this port carries: the one its `pin_type` setting chooses, else the one its
    /// family states exactly, as an Image port is a Texture; `None` for any other port.
    pub fn the_pin_type_it_carries(&self) -> Option<PinType> {
        self.pin_type.clone().or_else(|| {
            self.family
                .map(PortFamilyDeclaration::of_family)
                .and_then(|family| family.exact_pin_type())
        })
    }

    /// Why the link rules refuse `linked_port`, among `inputs` and `outputs`, the ports of its
    /// node, against the type this port chooses; `None` when it chooses none, the linked port
    /// states no one type exactly, or the link connects as it is.
    fn refusal_of_a_linked_port_by_the_chosen_type(
        &self,
        inputs: &[crate::core::graph::PortInfo],
        outputs: &[crate::core::graph::PortInfo],
        linked_port: &LinkPortRefOutput,
    ) -> Option<String> {
        let chosen = self.pin_type.as_ref()?;
        let port_cast = cast_of(&linked_port.port).ok()?;
        let linked = match self.direction {
            GraphInterfacePortDirection::Output => outputs,
            GraphInterfacePortDirection::Input => inputs,
        }
        .iter()
        .find(|port| port.name == port_cast)
        .filter(|port| crate::core::graph::pin_type_resolution::states_one_type_exactly(port))?;
        let linked_pin = linked.pin.as_ref()?;
        let linked_end = PinLinkEnd {
            node_name: &linked_port.node,
            port_name: &linked_port.port,
            member_name: linked_port.member.as_deref(),
            pin_type: &linked_pin.pin_type,
            meaning: linked.meaning.as_deref(),
        };
        let interface_end = PinLinkEnd {
            node_name: &self.interface_node,
            port_name: GRAPH_INTERFACE_VALUE_PORT_NAME,
            member_name: None,
            pin_type: chosen,
            meaning: None,
        };
        let verdict = match self.direction {
            GraphInterfacePortDirection::Output => pin_link_verdict(
                linked_end,
                PinLinkInputEnd {
                    end: interface_end,
                    input_mode: None,
                },
                &[],
            ),
            GraphInterfacePortDirection::Input => pin_link_verdict(
                interface_end,
                PinLinkInputEnd {
                    end: linked_end,
                    input_mode: linked.input_mode,
                },
                &[],
            ),
        };
        verdict
            .refusal_of_the_link_made_as_it_is()
            .map(ToString::to_string)
    }

    /// Why `output` is not this port's own output, the `value` of a
    /// `GraphInput` and the one output an interface node has; `None` when it is.
    pub fn refusal_of_an_output_it_does_not_have(
        &self,
        output: &LinkPortRefOutput,
    ) -> Option<String> {
        let is_the_value_output = self.direction == GraphInterfacePortDirection::Input
            && cast_of(&output.port).is_ok_and(|port| port == GRAPH_INTERFACE_VALUE_PORT_NAME);
        (!is_the_value_output).then(|| {
            format!(
                "`{}/{}` is no output of `{}`, and an interface node's one output is a \
                 `GraphInput`'s `{GRAPH_INTERFACE_VALUE_PORT_NAME}`",
                output.node, output.port, self.interface_node
            )
        })
    }

    /// A port named `port_name` of the family this port declares, as `graph` renders a port:
    /// typed as a node's port of that family is.
    pub fn rendered_as_a_port_named(&self, port_name: &str) -> GraphNodePortOnTheWire {
        let family = self.family.map(PortFamilyDeclaration::of_family);
        let pin = match &self.pin_type {
            Some(pin_type) => Some(ResolvedPin {
                pin_type: pin_type.clone(),
                flow: pin_type.default_flow(),
            }),
            None => family.clone().and_then(|family| {
                crate::core::graph::PortInfo::declaring_only_a_family(
                    port_name,
                    String::new(),
                    family,
                )
                .pin
            }),
        };
        GraphNodePortOnTheWire {
            name: port_name.to_string(),
            description: String::new(),
            port_kind: PortKindOnTheWire::default(),
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            stream_facts: None,
            values_checked: None,
            family,
            pin_default: None,
            pin,
            value_read: None,
        }
    }

    /// The port as `graph` renders it under `graph_interface` on its node and each link with an
    /// end on it.
    fn as_component(&self) -> GraphInterfacePortOnTheWire {
        GraphInterfacePortOnTheWire {
            direction: match self.direction {
                GraphInterfacePortDirection::Input => GraphInterfacePortDirectionOnTheWire::Input,
                GraphInterfacePortDirection::Output => GraphInterfacePortDirectionOnTheWire::Output,
            },
            port: cast_or_as_written(&self.port),
            family: self.family.map(|family| family.wire_name().to_string()),
            pin_type: self.the_pin_type_it_carries(),
            current_value: None,
        }
    }
}

/// `input_declaration` with no required names or codec: a `GraphInput` states
/// only a family, and the producer whose names and codec would meet them is
/// outside the graph.
fn the_input_without_what_only_the_graphs_producer_can_meet(
    input_declaration: PortFamilyDeclaration,
) -> PortFamilyDeclaration {
    PortFamilyDeclaration {
        requires: Vec::new(),
        requires_codec: Vec::new(),
        ..input_declaration
    }
}

fn refuse_an_interface_port_other_than_value(end: &LinkPortRefOutput) -> Result<()> {
    if cast_exposed_name_to_url_safe(&end.port)
        .is_ok_and(|port_cast| port_cast == GRAPH_INTERFACE_VALUE_PORT_NAME)
    {
        return Ok(());
    }
    Err(Error::GraphError(format!(
        "a link meets interface node `{}` at the port `{}`; an interface node's one port is \
         `{GRAPH_INTERFACE_VALUE_PORT_NAME}`",
        end.node, end.port
    )))
}

fn cast_of(name: &str) -> Result<String> {
    Ok(cast_exposed_name_to_url_safe(name)?.into_owned())
}

fn cast_or_as_written(name: &str) -> String {
    cast_exposed_name_to_url_safe(name)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_else(|_| name.to_string())
}

mod interface_node_config;

#[cfg(test)]
mod tests;
