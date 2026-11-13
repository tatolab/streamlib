// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph variables: `SetVariable` names a value and any number of
//! `GetVariable` nodes read it with no wire drawn.
//!
//! A variable holds nothing at run time. A port's output resolves each get's
//! links to links from the setter's source port, so the variable keeps that
//! port's family and its readers are checked as any link is. A node reference
//! resolves each config value naming a get to the node the setter names, a
//! reference edge checked against the type the reader's key takes.

use std::collections::{HashMap, HashSet};

use streamlib_processor_schema::{NODE_REFERENCE_NODE_KEY, node_name_a_config_value_references};

use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{cast_exposed_name_to_url_safe, names_listed_for_a_refusal};
use crate::core::graph_fragment_flattening::{
    GRAPH_FRAGMENT_NODE_PATH_SEPARATOR, GraphFragmentsAsDrawn,
};
use crate::core::graph_snapshot::{GraphSnapshot, GraphSnapshotLink, GraphSnapshotNode};
use crate::core::json_schema::{
    GraphResponse, LinkOutput, LinkPortRefOutput, PortInfoOutput, PortKindOutput,
    ProcessorNodeOutput, ProcessorNodePortsOutput,
};
use crate::core::utils::compute_json_checksum;
use crate::core::{Error, Result};

pub use streamlib_project_repository::{
    GET_VARIABLE_NODE_TYPE, GRAPH_VARIABLE_NAME_CONFIG_KEY, SET_VARIABLE_NODE_TYPE,
};

/// The config key a `SetVariable` holding a node reference holds it under.
pub const SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY: &str = "node_reference";

/// The port a `SetVariable` takes a port's output on and a `GetVariable`
/// hands it on.
pub const GRAPH_VARIABLE_VALUE_PORT_NAME: &str = "value";

/// What a graph's variables resolved to: the graph the runtime runs, with no
/// variable node, and what was drawn, so `graph` renders the variables back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphVariablesAsDrawn {
    /// Every `SetVariable` and `GetVariable` node, as the graph wrote it.
    pub variable_nodes: Vec<GraphSnapshotNode>,
    /// Where each of [`Self::variable_nodes`] sat among the graph's nodes.
    pub variable_node_positions: Vec<usize>,
    /// Every link with an end on a variable node, as the graph wrote it.
    pub links_touching_a_variable_node: Vec<GraphSnapshotLink>,
    /// Where each of [`Self::links_touching_a_variable_node`] sat among the
    /// graph's links.
    pub positions_of_links_touching_a_variable_node: Vec<usize>,
    /// Every link the runtime runs that a get resolved to.
    pub links_resolved_from_a_variable: Vec<LinkResolvedFromAGraphVariable>,
    /// Each config that named a get, as drawn and as resolved, by the cast of
    /// its node's name.
    pub configs_naming_a_get_by_node_name: HashMap<String, ConfigNamingAGet>,
}

/// A node's config that named a get: as drawn, and as the runtime runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigNamingAGet {
    /// The config as the graph wrote it, naming the get.
    pub as_drawn: serde_json::Value,
    /// The config the runtime runs, naming the setter's node.
    pub as_resolved: serde_json::Value,
}

/// The component key a variable node, and a drawn link with an end on one,
/// renders its variable under.
pub const GRAPH_VARIABLE_COMPONENT_KEY: &str = "graph_variable";

impl GraphVariablesAsDrawn {
    /// Render the variables into `graph_response`, a render of the graph the
    /// runtime runs: each variable node and each drawn link whose other end is
    /// still in the graph in, each link a get resolved to out, its live state
    /// carried on the get's drawn link, and each config naming a get as drawn
    /// while the runtime still runs it as resolved.
    pub fn render_into(&self, graph_response: &mut GraphResponse) {
        if self.variable_nodes.is_empty() {
            return;
        }
        let cast = |name: &str| {
            cast_exposed_name_to_url_safe(name)
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_else(|_| name.to_string())
        };
        let same_end = |left: &LinkPortRefOutput, right: &LinkPortRefOutput| {
            cast(&left.node) == cast(&right.node) && cast(&left.port) == cast(&right.port)
        };
        let is_the_runtime_link = |rendered: &LinkOutput, link: &GraphSnapshotLink| {
            same_end(&rendered.source, &link.source) && same_end(&rendered.target, &link.target)
        };
        let mut rendered_links_resolved_from_a_variable = Vec::new();
        graph_response.links.retain(|rendered| {
            let resolved_from_a_variable = self
                .links_resolved_from_a_variable
                .iter()
                .any(|resolved| is_the_runtime_link(rendered, &resolved.link));
            if resolved_from_a_variable {
                rendered_links_resolved_from_a_variable.push(rendered.clone());
            }
            !resolved_from_a_variable
        });
        let casts_of_the_running_nodes: HashSet<String> = graph_response
            .nodes
            .iter()
            .map(|node| cast(&node.name))
            .collect();
        let variable_by_node_cast: HashMap<String, &str> = self
            .variable_nodes
            .iter()
            .filter_map(|node| {
                node.config
                    .get(GRAPH_VARIABLE_NAME_CONFIG_KEY)
                    .and_then(serde_json::Value::as_str)
                    .map(|variable| (cast(&node.name), variable))
            })
            .collect();
        for (drawn, drawn_position) in self
            .links_touching_a_variable_node
            .iter()
            .zip(&self.positions_of_links_touching_a_variable_node)
        {
            let source_cast = cast(&drawn.source.node);
            let target_cast = cast(&drawn.target.node);
            let end_is_in_the_graph = |end_cast: &String| {
                variable_by_node_cast.contains_key(end_cast)
                    || casts_of_the_running_nodes.contains(end_cast)
            };
            if !end_is_in_the_graph(&source_cast) || !end_is_in_the_graph(&target_cast) {
                continue;
            }
            let runtime_link = self
                .links_resolved_from_a_variable
                .iter()
                .find(|resolved| {
                    cast(&resolved.get_variable_node) == source_cast
                        && same_end(&resolved.link.target, &drawn.target)
                })
                .and_then(|resolved| {
                    rendered_links_resolved_from_a_variable
                        .iter()
                        .find(|rendered| is_the_runtime_link(rendered, &resolved.link))
                });
            if variable_by_node_cast.contains_key(&source_cast) && runtime_link.is_none() {
                continue;
            }
            let variable = variable_by_node_cast
                .get(&source_cast)
                .or_else(|| variable_by_node_cast.get(&target_cast))
                .copied()
                .unwrap_or_default();
            let mut components = runtime_link
                .map(|rendered| rendered.components.clone())
                .unwrap_or_default();
            components.insert(
                GRAPH_VARIABLE_COMPONENT_KEY.to_string(),
                serde_json::json!({ GRAPH_VARIABLE_NAME_CONFIG_KEY: variable }),
            );
            let position = (*drawn_position).min(graph_response.links.len());
            graph_response.links.insert(
                position,
                LinkOutput {
                    id: runtime_link
                        .map(|rendered| rendered.id.clone())
                        .unwrap_or_default(),
                    source: LinkPortRefOutput {
                        node: source_cast,
                        port: drawn.source.port.clone(),
                    },
                    target: LinkPortRefOutput {
                        node: target_cast,
                        port: drawn.target.port.clone(),
                    },
                    capacity: runtime_link
                        .map(|rendered| rendered.capacity)
                        .unwrap_or_default(),
                    state: runtime_link
                        .map(|rendered| rendered.state)
                        .unwrap_or_default(),
                    error_reason: runtime_link.and_then(|rendered| rendered.error_reason.clone()),
                    components,
                },
            );
        }
        for node in &mut graph_response.nodes {
            let Some(config_naming_a_get) = self
                .configs_naming_a_get_by_node_name
                .get(&cast(&node.name))
            else {
                continue;
            };
            if node.config.as_ref() == Some(&config_naming_a_get.as_resolved) {
                node.config_checksum = compute_json_checksum(&config_naming_a_get.as_drawn);
                node.config = Some(config_naming_a_get.as_drawn.clone());
            }
        }
        for (variable_node, drawn_position) in self
            .variable_nodes
            .iter()
            .zip(&self.variable_node_positions)
        {
            let variable_node_cast = cast(&variable_node.name);
            let mut components = serde_json::Map::new();
            components.insert(
                GRAPH_VARIABLE_COMPONENT_KEY.to_string(),
                serde_json::json!({
                    GRAPH_VARIABLE_NAME_CONFIG_KEY: variable_by_node_cast.get(&variable_node_cast)
                }),
            );
            let value_port = PortInfoOutput {
                name: GRAPH_VARIABLE_VALUE_PORT_NAME.to_string(),
                description: String::new(),
                port_kind: PortKindOutput::default(),
                delivery_profile: None,
                borrow_quota: None,
                audio_window: None,
                stream_facts: None,
                values_checked: None,
                family: None,
                pin_default: None,
            };
            let is_a_setter = variable_node.processor_type.as_str() == SET_VARIABLE_NODE_TYPE;
            let position = (*drawn_position).min(graph_response.nodes.len());
            graph_response.nodes.insert(
                position,
                ProcessorNodeOutput {
                    id: variable_node
                        .id
                        .map(|node_uuid| node_uuid.to_string())
                        .unwrap_or_default(),
                    processor_type: variable_node.processor_type.clone(),
                    kind: None,
                    language: None,
                    name: variable_node_cast,
                    config: Some(variable_node.config.clone()),
                    config_checksum: compute_json_checksum(&variable_node.config),
                    pins: variable_node.pins.clone(),
                    exposed_config_fields: variable_node.exposed_config_fields.clone(),
                    studio: variable_node.studio.clone(),
                    inputs_waiting_for_a_value: Vec::new(),
                    ports: ProcessorNodePortsOutput {
                        inputs: if is_a_setter {
                            vec![value_port.clone()]
                        } else {
                            Vec::new()
                        },
                        outputs: if is_a_setter {
                            Vec::new()
                        } else {
                            vec![value_port]
                        },
                    },
                    components,
                    fragment: None,
                },
            );
        }
    }
}

/// One link the runtime runs in place of a get's drawn link.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkResolvedFromAGraphVariable {
    /// The link the runtime runs, from the setter's source port.
    pub link: GraphSnapshotLink,
    /// The variable the link resolved through.
    pub variable: String,
    /// The `GetVariable` node the drawn link left from.
    pub get_variable_node: String,
}

/// A graph with its variables resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphSnapshotWithVariablesResolved {
    /// The graph the runtime runs: no variable node, each get's links and
    /// references resolved.
    pub graph_the_runtime_runs: GraphSnapshot,
    /// What was drawn, for rendering.
    pub variables_as_drawn: GraphVariablesAsDrawn,
    /// Every `SetVariable` and its value, by the setter's name.
    pub values_the_set_variables_hold: Vec<SetVariableAndTheValueItHolds>,
}

/// Whether `node_type` is `SetVariable` or `GetVariable`.
pub fn is_a_graph_variable_node_type(node_type: &ProcessorClassImportPath) -> bool {
    matches!(
        node_type.as_str(),
        SET_VARIABLE_NODE_TYPE | GET_VARIABLE_NODE_TYPE
    )
}

/// What one `SetVariable` holds.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueASetVariableHolds {
    /// The output port linked into its `value` input.
    PortOutput(LinkPortRefOutput),
    /// The node its `node_reference` config names.
    NodeReference(String),
}

/// One `SetVariable` node and the value it holds, checked against the graph
/// whether or not a get reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct SetVariableAndTheValueItHolds {
    /// The `SetVariable` node's name, as the graph wrote it.
    pub set_variable_node: String,
    /// What it holds.
    pub value: ValueASetVariableHolds,
}

struct OneSetter<'graph> {
    node: &'graph GraphSnapshotNode,
    holds: ValueASetVariableHolds,
}

impl GraphSnapshot {
    /// Resolve this graph's variables, refusing by name a get with no setter,
    /// two setters of one name on one level, a setter holding both a port's
    /// output and a node reference or neither, and a get read as the kind of
    /// value its variable does not hold. A variable is set on the level of its
    /// setter, the stream or one of `fragments_flattened`, and a get reads the
    /// setter on its own level or else the nearest enclosing one. A graph with
    /// no variable node resolves to itself.
    pub fn with_graph_variables_resolved(
        &self,
        fragments_flattened: &GraphFragmentsAsDrawn,
    ) -> Result<GraphSnapshotWithVariablesResolved> {
        let variable_nodes: Vec<&GraphSnapshotNode> = self
            .nodes
            .iter()
            .filter(|node| is_a_graph_variable_node_type(&node.processor_type))
            .collect();
        if variable_nodes.is_empty() {
            return Ok(GraphSnapshotWithVariablesResolved {
                graph_the_runtime_runs: self.clone(),
                variables_as_drawn: GraphVariablesAsDrawn::default(),
                values_the_set_variables_hold: Vec::new(),
            });
        }

        let mut type_of_each_node_by_cast: HashMap<String, &str> = HashMap::new();
        for node in &self.nodes {
            let cast = cast_of(&node.name)?;
            if type_of_each_node_by_cast
                .insert(cast.clone(), node.processor_type.as_str())
                .is_some()
            {
                return Err(Error::NodeNameTaken {
                    name: node.name.clone(),
                    cast,
                });
            }
        }
        let is_a_get =
            |cast: &str| type_of_each_node_by_cast.get(cast) == Some(&GET_VARIABLE_NODE_TYPE);
        let is_a_set =
            |cast: &str| type_of_each_node_by_cast.get(cast) == Some(&SET_VARIABLE_NODE_TYPE);
        let variable_node_casts: HashSet<String> = variable_nodes
            .iter()
            .map(|node| cast_of(&node.name))
            .collect::<Result<_>>()?;
        let levels = GraphLevelsOfFlattenedFragments::of(fragments_flattened);
        let mut variable_of_each_get_by_cast: HashMap<String, String> = HashMap::new();
        let mut setters_by_level_and_variable: HashMap<(String, String), OneSetter<'_>> =
            HashMap::new();
        for node in &variable_nodes {
            let variable = variable_a_node_names(node)?;
            if node.processor_type.as_str() == GET_VARIABLE_NODE_TYPE {
                refuse_a_key_a_variable_node_does_not_take(
                    node,
                    &[GRAPH_VARIABLE_NAME_CONFIG_KEY],
                )?;
                variable_of_each_get_by_cast.insert(cast_of(&node.name)?, variable);
                continue;
            }
            let setter = OneSetter {
                node,
                holds: value_one_setter_holds(self, node, &variable, &is_a_get, &is_a_set)?,
            };
            if let Some(first) = setters_by_level_and_variable.insert(
                (levels.level_of(&node.name).to_string(), variable.clone()),
                setter,
            ) {
                return Err(Error::GraphError(format!(
                    "the variable `{variable}` is set by both `{}` and `{}`, and a variable \
                     has one setter on its level. Rename one of the variables, or remove one \
                     setter",
                    first.node.name, node.name
                )));
            }
        }

        let setter_of = |get_node: &str, variable: &str| -> Result<&OneSetter<'_>> {
            let mut level = levels.level_of(get_node);
            let mut variables_the_get_can_read = Vec::new();
            loop {
                if let Some(setter) =
                    setters_by_level_and_variable.get(&(level.to_string(), variable.to_string()))
                {
                    return Ok(setter);
                }
                variables_the_get_can_read.extend(
                    setters_by_level_and_variable
                        .keys()
                        .filter(|(setter_level, _)| setter_level == level)
                        .map(|(_, set_variable)| set_variable.clone()),
                );
                if level.is_empty() {
                    break;
                }
                level = levels.level_of(level);
            }
            Err(Error::GraphError(format!(
                "`GetVariable` node `{get_node}` reads the variable `{variable}`, which no \
                 `SetVariable` on its level or an enclosing one sets. It can read: {}",
                names_listed_for_a_refusal(
                    sorted(variables_the_get_can_read.iter()),
                    "no variable"
                )
            )))
        };

        for get_node in variable_nodes
            .iter()
            .filter(|node| node.processor_type.as_str() == GET_VARIABLE_NODE_TYPE)
        {
            setter_of(&get_node.name, &variable_a_node_names(get_node)?)?;
        }

        let mut runtime_links = Vec::new();
        let mut links_touching_a_variable_node = Vec::new();
        let mut positions_of_links_touching_a_variable_node = Vec::new();
        let mut links_resolved_from_a_variable = Vec::new();
        for (link_position, link) in self.links.iter().enumerate() {
            let source_cast = cast_of(&link.source.node)?;
            let target_cast = cast_of(&link.target.node)?;
            if !variable_node_casts.contains(&source_cast)
                && !variable_node_casts.contains(&target_cast)
            {
                runtime_links.push(link.clone());
                continue;
            }
            links_touching_a_variable_node.push(link.clone());
            positions_of_links_touching_a_variable_node.push(link_position);
            if is_a_set(&source_cast) {
                return Err(Error::GraphError(format!(
                    "a link leaves `SetVariable` node `{}`, which has no output: read a \
                     variable through a `GetVariable` node",
                    link.source.node
                )));
            }
            if is_a_get(&target_cast) {
                return Err(Error::GraphError(format!(
                    "a link enters `GetVariable` node `{}`, which has no input: set a \
                     variable by linking into a `SetVariable` node",
                    link.target.node
                )));
            }
            if is_a_set(&target_cast) {
                continue;
            }
            let Some(variable) = variable_of_each_get_by_cast.get(&source_cast) else {
                continue;
            };
            refuse_a_variable_port_other_than_value(&link.source, "a link leaves")?;
            let setter = setter_of(&link.source.node, variable)?;
            let ValueASetVariableHolds::PortOutput(setter_source) = &setter.holds else {
                return Err(Error::GraphError(format!(
                    "`GetVariable` node `{}` is linked to `{}/{}`, and its variable \
                     `{variable}` holds a node reference, which a config key reads, never \
                     a link",
                    link.source.node, link.target.node, link.target.port
                )));
            };
            let resolved = GraphSnapshotLink {
                source: setter_source.clone(),
                target: link.target.clone(),
            };
            runtime_links.push(resolved.clone());
            links_resolved_from_a_variable.push(LinkResolvedFromAGraphVariable {
                link: resolved,
                variable: variable.clone(),
                get_variable_node: link.source.node.clone(),
            });
        }

        let mut configs_naming_a_get_by_node_name = HashMap::new();
        let mut runtime_nodes = Vec::new();
        for node in &self.nodes {
            if is_a_graph_variable_node_type(&node.processor_type) {
                continue;
            }
            let Some(config_object) = node.config.as_object() else {
                runtime_nodes.push(node.clone());
                continue;
            };
            let mut resolved_config = serde_json::Value::Object(config_object.clone());
            let mut named_a_get = false;
            resolve_each_config_value_naming_a_get(
                &mut resolved_config,
                "config",
                &mut |config_key_path, referenced| {
                    let Ok(referenced_cast) = cast_of(referenced) else {
                        return Ok(None);
                    };
                    if is_a_set(&referenced_cast) {
                        return Err(Error::GraphError(format!(
                            "node `{}`'s `{config_key_path}` names `SetVariable` node \
                             `{referenced}`: read a variable through a `GetVariable` node",
                            node.name
                        )));
                    }
                    let Some(variable) = variable_of_each_get_by_cast.get(&referenced_cast) else {
                        return Ok(None);
                    };
                    let setter = setter_of(referenced, variable)?;
                    let ValueASetVariableHolds::NodeReference(referenced_node) = &setter.holds
                    else {
                        return Err(Error::GraphError(format!(
                            "node `{}`'s `{config_key_path}` reads `GetVariable` node \
                             `{referenced}`, and its variable `{variable}` holds a port's \
                             output, which a link reads, never a config value",
                            node.name
                        )));
                    };
                    named_a_get = true;
                    Ok(Some(referenced_node.clone()))
                },
            )?;
            if named_a_get {
                configs_naming_a_get_by_node_name.insert(
                    cast_of(&node.name)?,
                    ConfigNamingAGet {
                        as_drawn: node.config.clone(),
                        as_resolved: resolved_config.clone(),
                    },
                );
            }
            runtime_nodes.push(GraphSnapshotNode {
                config: resolved_config,
                ..node.clone()
            });
        }

        let mut values_the_set_variables_hold: Vec<SetVariableAndTheValueItHolds> =
            setters_by_level_and_variable
                .values()
                .map(|setter| SetVariableAndTheValueItHolds {
                    set_variable_node: setter.node.name.clone(),
                    value: setter.holds.clone(),
                })
                .collect();
        values_the_set_variables_hold
            .sort_by(|left, right| left.set_variable_node.cmp(&right.set_variable_node));

        Ok(GraphSnapshotWithVariablesResolved {
            values_the_set_variables_hold,
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
            variables_as_drawn: GraphVariablesAsDrawn {
                variable_node_positions: self
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, node)| is_a_graph_variable_node_type(&node.processor_type))
                    .map(|(position, _)| position)
                    .collect(),
                variable_nodes: variable_nodes.into_iter().cloned().collect(),
                links_touching_a_variable_node,
                positions_of_links_touching_a_variable_node,
                links_resolved_from_a_variable,
                configs_naming_a_get_by_node_name,
            },
        })
    }
}

/// What `setter` holds: the one link into its `value` port, or its
/// `node_reference` config, and never both or neither.
fn value_one_setter_holds(
    graph: &GraphSnapshot,
    setter: &GraphSnapshotNode,
    variable: &str,
    is_a_get: &dyn Fn(&str) -> bool,
    is_a_set: &dyn Fn(&str) -> bool,
) -> Result<ValueASetVariableHolds> {
    refuse_a_key_a_variable_node_does_not_take(
        setter,
        &[
            GRAPH_VARIABLE_NAME_CONFIG_KEY,
            SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY,
        ],
    )?;
    let setter_cast = cast_of(&setter.name)?;
    let mut links_in = Vec::new();
    for link in &graph.links {
        if cast_of(&link.target.node)? == setter_cast {
            refuse_a_variable_port_other_than_value(&link.target, "a link enters")?;
            links_in.push(link);
        }
    }
    let node_reference = setter
        .config
        .get(SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY)
        .filter(|value| !value.is_null());
    match (links_in.as_slice(), node_reference) {
        ([link], None) => {
            if is_a_get(&cast_of(&link.source.node)?) {
                return Err(Error::GraphError(format!(
                    "`SetVariable` node `{}` takes its value from `GetVariable` node `{}`; \
                     link it from the port that variable's setter takes",
                    setter.name, link.source.node
                )));
            }
            Ok(ValueASetVariableHolds::PortOutput(link.source.clone()))
        }
        ([], Some(reference)) => {
            let referenced = node_name_a_config_value_references(reference).ok_or_else(|| {
                Error::GraphError(format!(
                    "`SetVariable` node `{}`'s `{SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY}` \
                     holds {reference}, and it holds a reference to a node, written \
                     {{\"node\": \"<name>\"}}. Pass the reference `add` returned",
                    setter.name
                ))
            })?;
            let referenced_cast = cast_of(referenced)?;
            if is_a_get(&referenced_cast) || is_a_set(&referenced_cast) {
                return Err(Error::GraphError(format!(
                    "`SetVariable` node `{}` holds variable node `{referenced}`; a variable \
                     holds a node that runs, never another variable",
                    setter.name
                )));
            }
            Ok(ValueASetVariableHolds::NodeReference(
                referenced.to_string(),
            ))
        }
        ([], None) => Err(Error::GraphError(format!(
            "`SetVariable` node `{}` sets the variable `{variable}` to nothing: link a port \
             into its `{GRAPH_VARIABLE_VALUE_PORT_NAME}` input, or name a node in its \
             `{SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY}` config",
            setter.name
        ))),
        (_, Some(_)) => Err(Error::GraphError(format!(
            "`SetVariable` node `{}` sets the variable `{variable}` both from a link and \
             from its `{SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY}` config; a variable holds \
             one value",
            setter.name
        ))),
        (_, None) => Err(Error::GraphError(format!(
            "`SetVariable` node `{}` sets the variable `{variable}` from {} links; a \
             variable holds one port's output",
            setter.name,
            links_in.len()
        ))),
    }
}

/// Replace each node reference in `config_value` that `resolve` resolves with
/// the node it names, at any depth, naming each by its key path.
fn resolve_each_config_value_naming_a_get(
    config_value: &mut serde_json::Value,
    key_path: &str,
    resolve: &mut dyn FnMut(&str, &str) -> Result<Option<String>>,
) -> Result<()> {
    if let Some(referenced) = node_name_a_config_value_references(config_value) {
        if let Some(resolved) = resolve(key_path, referenced)? {
            *config_value = serde_json::json!({ NODE_REFERENCE_NODE_KEY: resolved });
        }
        return Ok(());
    }
    match config_value {
        serde_json::Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                resolve_each_config_value_naming_a_get(
                    value,
                    &format!("{key_path}[{key:?}]"),
                    resolve,
                )?;
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                resolve_each_config_value_naming_a_get(
                    item,
                    &format!("{key_path}[{index}]"),
                    resolve,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn variable_a_node_names(node: &GraphSnapshotNode) -> Result<String> {
    node.config
        .get(GRAPH_VARIABLE_NAME_CONFIG_KEY)
        .and_then(serde_json::Value::as_str)
        .filter(|variable| !variable.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::GraphError(format!(
                "variable node `{}` names no variable: its config takes \
                 `{GRAPH_VARIABLE_NAME_CONFIG_KEY}`, a non-empty string",
                node.name
            ))
        })
}

fn refuse_a_key_a_variable_node_does_not_take(
    node: &GraphSnapshotNode,
    keys_it_takes: &[&str],
) -> Result<()> {
    let Some(config) = node.config.as_object() else {
        return Ok(());
    };
    let Some(unread_key) = config
        .keys()
        .find(|key| !keys_it_takes.contains(&key.as_str()))
    else {
        return Ok(());
    };
    Err(Error::GraphError(format!(
        "variable node `{}` carries the config key `{unread_key}`, which it does not take. \
         It takes: {}",
        node.name,
        keys_it_takes
            .iter()
            .map(|key| format!("`{key}`"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

fn refuse_a_variable_port_other_than_value(
    end: &LinkPortRefOutput,
    how_the_link_meets_it: &str,
) -> Result<()> {
    if cast_exposed_name_to_url_safe(&end.port)
        .is_ok_and(|port_cast| port_cast == GRAPH_VARIABLE_VALUE_PORT_NAME)
    {
        return Ok(());
    }
    Err(Error::GraphError(format!(
        "{how_the_link_meets_it} variable node `{}` at the port `{}`; a variable node's one \
         port is `{GRAPH_VARIABLE_VALUE_PORT_NAME}`",
        end.node, end.port
    )))
}

fn cast_of(name: &str) -> Result<String> {
    Ok(cast_exposed_name_to_url_safe(name)?.into_owned())
}

fn sorted<'a>(names: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut names: Vec<String> = names.cloned().collect();
    names.sort();
    names.dedup();
    names
}

/// The levels a flattened graph's nodes run on: the stream's own, named `""`, and each
/// fragment's, named by its path of names.
struct GraphLevelsOfFlattenedFragments {
    fragment_paths_of_names: HashSet<String>,
}

impl GraphLevelsOfFlattenedFragments {
    fn of(fragments_flattened: &GraphFragmentsAsDrawn) -> Self {
        let mut fragment_paths_of_names = HashSet::new();
        let mut fragments_to_visit: Vec<_> = fragments_flattened.fragments.iter().collect();
        while let Some(fragment) = fragments_to_visit.pop() {
            fragment_paths_of_names.insert(fragment.path_of_names.clone());
            fragments_to_visit.extend(&fragment.fragments_it_holds);
        }
        Self {
            fragment_paths_of_names,
        }
    }

    /// The level `node_name` runs on: the innermost fragment whose path of names prefixes
    /// it, or the stream's own. Asked of a level, it answers the level enclosing it.
    fn level_of<'name>(&self, node_name: &'name str) -> &'name str {
        let mut prefix_end = node_name.len();
        while let Some(separator) =
            node_name[..prefix_end].rfind(GRAPH_FRAGMENT_NODE_PATH_SEPARATOR)
        {
            if self
                .fragment_paths_of_names
                .contains(&node_name[..separator])
            {
                return &node_name[..separator];
            }
            prefix_end = separator;
        }
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(graph: serde_json::Value) -> Result<GraphSnapshotWithVariablesResolved> {
        GraphSnapshot::from_graph_document(graph)
            .expect("the graph parses")
            .with_graph_variables_resolved(&GraphFragmentsAsDrawn::default())
    }

    fn refusal_of(graph: serde_json::Value) -> String {
        resolved(graph)
            .expect_err("the graph's variables are refused")
            .to_string()
    }

    fn link(source: (&str, &str), target: (&str, &str)) -> GraphSnapshotLink {
        GraphSnapshotLink {
            source: LinkPortRefOutput {
                node: source.0.to_string(),
                port: source.1.to_string(),
            },
            target: LinkPortRefOutput {
                node: target.0.to_string(),
                port: target.1.to_string(),
            },
        }
    }

    fn a_camera_variable_read_by_two_displays() -> serde_json::Value {
        serde_json::json!({
            "nodes": [
                {"name": "camera", "type": "rig:Camera"},
                {"name": "set-camera", "type": "tatolab.stream:SetVariable", "config": {"variable": "camera"}},
                {"name": "get-camera", "type": "tatolab.stream:GetVariable", "config": {"variable": "camera"}},
                {"name": "get-camera-2", "type": "tatolab.stream:GetVariable", "config": {"variable": "camera"}},
                {"name": "front", "type": "rig:Display"},
                {"name": "back", "type": "rig:Display"}
            ],
            "links": [
                {"source": {"node": "camera", "port": "video"}, "target": {"node": "set-camera", "port": "value"}},
                {"source": {"node": "get-camera", "port": "value"}, "target": {"node": "front", "port": "video_in"}},
                {"source": {"node": "get-camera-2", "port": "value"}, "target": {"node": "back", "port": "video_in"}}
            ]
        })
    }

    #[test]
    fn a_port_variable_read_twice_resolves_to_two_links_from_the_setters_source() {
        let resolution = resolved(a_camera_variable_read_by_two_displays()).unwrap();
        let runtime = &resolution.graph_the_runtime_runs;
        let node_names: Vec<&str> = runtime.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(node_names, ["camera", "front", "back"]);
        assert_eq!(
            runtime.links,
            [
                link(("camera", "video"), ("front", "video_in")),
                link(("camera", "video"), ("back", "video_in")),
            ]
        );
        assert_eq!(resolution.variables_as_drawn.variable_nodes.len(), 3);
        assert_eq!(
            resolution
                .variables_as_drawn
                .links_touching_a_variable_node
                .len(),
            3
        );
    }

    #[test]
    fn a_graph_with_no_variable_resolves_to_itself() {
        let graph = serde_json::json!({
            "nodes": [{"name": "camera", "type": "rig:Camera"}, {"name": "front", "type": "rig:Display"}],
            "links": [{"source": {"node": "camera", "port": "video"}, "target": {"node": "front", "port": "video_in"}}]
        });
        let resolution = resolved(graph.clone()).unwrap();
        assert_eq!(
            resolution.graph_the_runtime_runs,
            GraphSnapshot::from_graph_document(graph).unwrap()
        );
        assert_eq!(
            resolution.variables_as_drawn,
            GraphVariablesAsDrawn::default()
        );
    }

    #[test]
    fn a_get_with_no_setter_is_refused_naming_it_and_the_variables_set() {
        let mut graph = a_camera_variable_read_by_two_displays();
        graph["nodes"][3]["config"]["variable"] = serde_json::json!("microphone");
        let refusal = refusal_of(graph);
        assert!(
            refusal.contains(
                "`GetVariable` node `get-camera-2` reads the variable `microphone`, which no \
                 `SetVariable` on its level or an enclosing one sets. It can read: camera"
            ),
            "{refusal}"
        );
    }

    fn resolved_once_flattened(
        graph: serde_json::Value,
    ) -> Result<GraphSnapshotWithVariablesResolved> {
        let flattening = GraphSnapshot::from_graph_document(graph)
            .expect("the graph parses")
            .with_graph_fragments_flattened(
                &crate::core::processors::NodeTypesOneStreamResolves::new(),
            )
            .expect("the fragments flatten");
        flattening
            .graph_the_runtime_runs
            .with_graph_variables_resolved(&flattening.fragments_as_drawn)
    }

    fn a_fragment_whose_tint_reads_the_variable_shade(
        nodes_beside_the_tint: serde_json::Value,
        links_beside_the_tints: serde_json::Value,
    ) -> serde_json::Value {
        let mut nodes = vec![
            serde_json::json!({"name": "source", "type": "tatolab.stream:GraphInput",
                               "config": {"port": "video", "family": "image"}}),
            serde_json::json!({"name": "get-shade", "type": "tatolab.stream:GetVariable",
                               "config": {"variable": "shade"}}),
            serde_json::json!({"name": "tint", "type": "rig:Tint"}),
            serde_json::json!({"name": "out", "type": "tatolab.stream:GraphOutput",
                               "config": {"port": "video", "family": "image"}}),
        ];
        nodes.extend(nodes_beside_the_tint.as_array().unwrap().iter().cloned());
        let mut links = vec![
            serde_json::json!({"source": {"node": "source", "port": "value"},
                               "target": {"node": "tint", "port": "video_in"}}),
            serde_json::json!({"source": {"node": "get-shade", "port": "value"},
                               "target": {"node": "tint", "port": "shade_in"}}),
            serde_json::json!({"source": {"node": "tint", "port": "video"},
                               "target": {"node": "out", "port": "value"}}),
        ];
        links.extend(links_beside_the_tints.as_array().unwrap().iter().cloned());
        serde_json::json!({"nodes": nodes, "links": links})
    }

    fn a_shade_set_inside() -> serde_json::Value {
        a_fragment_whose_tint_reads_the_variable_shade(
            serde_json::json!([
                {"name": "palette", "type": "rig:Palette"},
                {"name": "set-shade", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "shade"}},
            ]),
            serde_json::json!([
                {"source": {"node": "palette", "port": "colour"},
                 "target": {"node": "set-shade", "port": "value"}},
            ]),
        )
    }

    fn a_camera_through(
        fragments: &[(&str, serde_json::Value)],
        top_level_nodes: serde_json::Value,
        top_level_links: serde_json::Value,
    ) -> serde_json::Value {
        let mut nodes = vec![serde_json::json!({"name": "camera", "type": "rig:Camera"})];
        let mut links = Vec::new();
        let mut previous = ("camera", "video");
        for (fragment_name, fragment) in fragments {
            nodes.push(
                serde_json::json!({"name": fragment_name, "type": "looks.generators:look",
                                          "fragment": fragment}),
            );
            links.push(
                serde_json::json!({"source": {"node": previous.0, "port": previous.1},
                                          "target": {"node": fragment_name, "port": "video"}}),
            );
            previous = (fragment_name, "video");
        }
        nodes.push(serde_json::json!({"name": "display", "type": "rig:Display"}));
        links.push(
            serde_json::json!({"source": {"node": previous.0, "port": previous.1},
                                      "target": {"node": "display", "port": "video_in"}}),
        );
        nodes.extend(top_level_nodes.as_array().unwrap().iter().cloned());
        links.extend(top_level_links.as_array().unwrap().iter().cloned());
        serde_json::json!({"nodes": nodes, "links": links})
    }

    #[test]
    fn a_fragment_placed_twice_reads_each_placements_own_variable() {
        let resolution = resolved_once_flattened(a_camera_through(
            &[
                ("warm", a_shade_set_inside()),
                ("cool", a_shade_set_inside()),
            ],
            serde_json::json!([]),
            serde_json::json!([]),
        ))
        .expect("each placement sets its own `shade`");
        let links = &resolution.graph_the_runtime_runs.links;
        assert!(links.contains(&link(("warm/palette", "colour"), ("warm/tint", "shade_in"))));
        assert!(links.contains(&link(("cool/palette", "colour"), ("cool/tint", "shade_in"))));
    }

    #[test]
    fn a_get_inside_a_fragment_with_no_setter_on_its_level_reads_the_enclosing_one() {
        let resolution = resolved_once_flattened(a_camera_through(
            &[(
                "look",
                a_fragment_whose_tint_reads_the_variable_shade(
                    serde_json::json!([]),
                    serde_json::json!([]),
                ),
            )],
            serde_json::json!([
                {"name": "palette", "type": "rig:Palette"},
                {"name": "set-shade", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "shade"}},
            ]),
            serde_json::json!([
                {"source": {"node": "palette", "port": "colour"},
                 "target": {"node": "set-shade", "port": "value"}},
            ]),
        ))
        .expect("the get reads the stream's `shade`");
        assert!(
            resolution
                .graph_the_runtime_runs
                .links
                .contains(&link(("palette", "colour"), ("look/tint", "shade_in")))
        );
    }

    #[test]
    fn a_setter_inside_a_fragment_shadows_the_enclosing_one_for_its_own_level_only() {
        let resolution = resolved_once_flattened(a_camera_through(
            &[("look", a_shade_set_inside())],
            serde_json::json!([
                {"name": "grey", "type": "rig:Palette"},
                {"name": "set-shade", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "shade"}},
                {"name": "get-shade", "type": "tatolab.stream:GetVariable",
                 "config": {"variable": "shade"}},
                {"name": "swatch", "type": "rig:Display"},
            ]),
            serde_json::json!([
                {"source": {"node": "grey", "port": "colour"},
                 "target": {"node": "set-shade", "port": "value"}},
                {"source": {"node": "get-shade", "port": "value"},
                 "target": {"node": "swatch", "port": "video_in"}},
            ]),
        ))
        .expect("one `shade` per level");
        let links = &resolution.graph_the_runtime_runs.links;
        assert!(links.contains(&link(("look/palette", "colour"), ("look/tint", "shade_in"))));
        assert!(links.contains(&link(("grey", "colour"), ("swatch", "video_in"))));
    }

    #[test]
    fn a_get_outside_a_fragment_never_reads_a_variable_set_inside_it() {
        let refusal = resolved_once_flattened(a_camera_through(
            &[("look", a_shade_set_inside())],
            serde_json::json!([
                {"name": "get-shade", "type": "tatolab.stream:GetVariable",
                 "config": {"variable": "shade"}},
                {"name": "swatch", "type": "rig:Display"},
            ]),
            serde_json::json!([
                {"source": {"node": "get-shade", "port": "value"},
                 "target": {"node": "swatch", "port": "video_in"}},
            ]),
        ))
        .expect_err("the stream sets no `shade`")
        .to_string();
        assert!(
            refusal.contains(
                "`GetVariable` node `get-shade` reads the variable `shade`, which no \
                 `SetVariable` on its level or an enclosing one sets. It can read: no variable"
            ),
            "{refusal}"
        );
    }

    #[test]
    fn a_variable_set_inside_a_fragment_leaves_it_through_a_graph_output() {
        let fragment_handing_out_its_shade = a_fragment_whose_tint_reads_the_variable_shade(
            serde_json::json!([
                {"name": "palette", "type": "rig:Palette"},
                {"name": "set-shade", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "shade"}},
                {"name": "get-shade-out", "type": "tatolab.stream:GetVariable",
                 "config": {"variable": "shade"}},
                {"name": "shade-out", "type": "tatolab.stream:GraphOutput",
                 "config": {"port": "shade", "family": "image"}},
            ]),
            serde_json::json!([
                {"source": {"node": "palette", "port": "colour"},
                 "target": {"node": "set-shade", "port": "value"}},
                {"source": {"node": "get-shade-out", "port": "value"},
                 "target": {"node": "shade-out", "port": "value"}},
            ]),
        );
        let resolution = resolved_once_flattened(a_camera_through(
            &[("look", fragment_handing_out_its_shade)],
            serde_json::json!([{"name": "swatch", "type": "rig:Display"}]),
            serde_json::json!([
                {"source": {"node": "look", "port": "shade"},
                 "target": {"node": "swatch", "port": "video_in"}},
            ]),
        ))
        .expect("the stream reads the fragment's `shade` through its Output");
        assert!(
            resolution
                .graph_the_runtime_runs
                .links
                .contains(&link(("look/palette", "colour"), ("swatch", "video_in")))
        );
    }

    #[test]
    fn two_setters_of_one_variable_are_refused_naming_both() {
        let mut graph = a_camera_variable_read_by_two_displays();
        graph["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(
                {"name": "set-camera-2", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "camera", "node_reference": {"node": "front"}}}
            ));
        let refusal = refusal_of(graph);
        assert!(
            refusal
                .contains("the variable `camera` is set by both `set-camera` and `set-camera-2`"),
            "{refusal}"
        );
    }

    #[test]
    fn a_setter_holding_nothing_or_two_values_is_refused() {
        let mut holding_nothing = a_camera_variable_read_by_two_displays();
        holding_nothing["links"].as_array_mut().unwrap().remove(0);
        assert!(refusal_of(holding_nothing).contains("sets the variable `camera` to nothing"));

        let mut holding_two = a_camera_variable_read_by_two_displays();
        holding_two["nodes"][1]["config"]["node_reference"] = serde_json::json!({"node": "front"});
        assert!(refusal_of(holding_two).contains("both from a link and from its `node_reference`"));
    }

    fn a_gateway_variable_read_by_a_tracks_config() -> serde_json::Value {
        serde_json::json!({
            "nodes": [
                {"name": "cloudflare", "type": "rig:Gateway"},
                {"name": "set-connection", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "connection", "node_reference": {"node": "cloudflare"}}},
                {"name": "get-connection", "type": "tatolab.stream:GetVariable", "config": {"variable": "connection"}},
                {"name": "camera-track", "type": "rig:Track",
                 "config": {"gateway": {"node": "get-connection"}, "track": "camera"}}
            ]
        })
    }

    #[test]
    fn a_node_reference_variable_resolves_each_config_naming_a_get_to_the_setters_node() {
        let resolution = resolved(a_gateway_variable_read_by_a_tracks_config()).unwrap();
        let track = &resolution.graph_the_runtime_runs.nodes[1];
        assert_eq!(track.name, "camera-track");
        assert_eq!(
            track.config,
            serde_json::json!({"gateway": {"node": "cloudflare"}, "track": "camera"})
        );
        assert_eq!(
            resolution
                .variables_as_drawn
                .configs_naming_a_get_by_node_name["camera-track"]
                .as_drawn,
            serde_json::json!({"gateway": {"node": "get-connection"}, "track": "camera"})
        );
    }

    #[test]
    fn a_get_read_as_the_kind_of_value_its_variable_does_not_hold_is_refused() {
        let mut linked_reference = a_gateway_variable_read_by_a_tracks_config();
        linked_reference["links"] = serde_json::json!([
            {"source": {"node": "get-connection", "port": "value"}, "target": {"node": "camera-track", "port": "content"}}
        ]);
        assert!(
            refusal_of(linked_reference)
                .contains("its variable `connection` holds a node reference")
        );

        let mut config_reading_a_port = a_camera_variable_read_by_two_displays();
        config_reading_a_port["nodes"][4]["config"] =
            serde_json::json!({"source": {"node": "get-camera"}});
        assert!(
            refusal_of(config_reading_a_port)
                .contains("its variable `camera` holds a port's output")
        );
    }

    #[test]
    fn a_link_out_of_a_setter_or_onto_another_port_is_refused() {
        let mut out_of_a_setter = a_camera_variable_read_by_two_displays();
        out_of_a_setter["links"][1]["source"]["node"] = serde_json::json!("set-camera");
        assert!(
            refusal_of(out_of_a_setter).contains("a link leaves `SetVariable` node `set-camera`")
        );

        let mut another_port = a_camera_variable_read_by_two_displays();
        another_port["links"][1]["source"]["port"] = serde_json::json!("video");
        assert!(refusal_of(another_port).contains("one port is `value`"));
    }

    #[test]
    fn a_variable_node_naming_no_variable_or_an_unknown_key_is_refused() {
        let mut no_variable = a_camera_variable_read_by_two_displays();
        no_variable["nodes"][2]["config"] = serde_json::json!({});
        assert!(refusal_of(no_variable).contains("`get-camera` names no variable"));

        let mut unknown_key = a_camera_variable_read_by_two_displays();
        unknown_key["nodes"][2]["config"]["colour"] = serde_json::json!("red");
        assert!(refusal_of(unknown_key).contains("carries the config key `colour`"));
    }
    #[test]
    fn a_variable_node_whose_name_casts_like_another_nodes_is_refused() {
        let mut graph = a_camera_variable_read_by_two_displays();
        graph["nodes"][2]["name"] = serde_json::json!("Camera");
        let refusal = resolved(graph).unwrap_err();
        assert!(
            matches!(refusal, Error::NodeNameTaken { ref cast, .. } if cast == "camera"),
            "{refusal}"
        );
    }

    #[test]
    fn a_link_into_a_get_is_refused_naming_where_a_variable_is_set() {
        let mut graph = a_camera_variable_read_by_two_displays();
        graph["links"].as_array_mut().unwrap().push(serde_json::json!(
            {"source": {"node": "camera", "port": "video"}, "target": {"node": "get-camera", "port": "value"}}
        ));
        assert!(refusal_of(graph).contains("a link enters `GetVariable` node `get-camera`"));
    }

    #[test]
    fn a_get_no_link_or_config_reads_yet_still_needs_a_setter() {
        let mut graph = a_camera_variable_read_by_two_displays();
        graph["nodes"].as_array_mut().unwrap().push(serde_json::json!(
            {"name": "get-camra", "type": "tatolab.stream:GetVariable", "config": {"variable": "camra"}}
        ));
        assert!(
            refusal_of(graph)
                .contains("reads the variable `camra`, which no `SetVariable` on its level")
        );
    }

    #[test]
    fn a_node_reference_naming_a_get_at_any_depth_resolves() {
        let mut graph = a_gateway_variable_read_by_a_tracks_config();
        graph["nodes"][3]["config"] =
            serde_json::json!({"routes": [{"gateway": {"node": "get-connection"}}]});
        let resolution = resolved(graph).unwrap();
        assert_eq!(
            resolution.graph_the_runtime_runs.nodes[1].config,
            serde_json::json!({"routes": [{"gateway": {"node": "cloudflare"}}]})
        );
    }

    #[test]
    fn a_setter_holding_another_variable_node_is_refused() {
        let mut graph = a_gateway_variable_read_by_a_tracks_config();
        graph["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(
                {"name": "set-alias", "type": "tatolab.stream:SetVariable",
                 "config": {"variable": "alias", "node_reference": {"node": "get-connection"}}}
            ));
        assert!(refusal_of(graph).contains("holds variable node `get-connection`"));
    }
    fn a_rendered_node(name: &str) -> ProcessorNodeOutput {
        ProcessorNodeOutput {
            id: format!("id-{name}"),
            processor_type: ProcessorClassImportPath::new("rig:Node").unwrap(),
            kind: None,
            language: None,
            name: name.to_string(),
            config: Some(serde_json::json!({})),
            config_checksum: 0,
            ports: ProcessorNodePortsOutput {
                inputs: Vec::new(),
                outputs: Vec::new(),
            },
            components: serde_json::Map::new(),
            pins: serde_json::Map::new(),
            exposed_config_fields: Vec::new(),
            studio: serde_json::Map::new(),
            inputs_waiting_for_a_value: Vec::new(),
            fragment: None,
        }
    }

    fn a_rendered_link(id: &str, source: (&str, &str), target: (&str, &str)) -> LinkOutput {
        let GraphSnapshotLink { source, target } = link(source, target);
        LinkOutput {
            id: id.to_string(),
            source,
            target,
            capacity: 4,
            state: Default::default(),
            error_reason: None,
            components: serde_json::Map::new(),
        }
    }

    #[test]
    fn a_variable_port_spelled_another_way_that_casts_to_value_resolves() {
        let mut graph = a_camera_variable_read_by_two_displays();
        graph["links"][0]["target"]["port"] = serde_json::json!("VALUE");
        graph["links"][1]["source"]["port"] = serde_json::json!("Value");
        let resolution = resolved(graph).unwrap();
        assert_eq!(resolution.graph_the_runtime_runs.links.len(), 2);
    }

    #[test]
    fn a_config_object_naming_no_castable_node_is_left_as_written() {
        let mut graph = a_camera_variable_read_by_two_displays();
        graph["nodes"][4]["config"] = serde_json::json!({"metadata": {"node": ""}});
        let resolution = resolved(graph).unwrap();
        assert_eq!(
            resolution.graph_the_runtime_runs.nodes[1].config,
            serde_json::json!({"metadata": {"node": ""}})
        );
    }

    #[test]
    fn the_render_matches_a_runtime_link_whose_ports_the_runtime_cast() {
        let mut graph = a_camera_variable_read_by_two_displays();
        graph["links"][0]["source"]["port"] = serde_json::json!("VIDEO");
        let resolution = resolved(graph).unwrap();
        let mut graph_response = GraphResponse {
            asset: None,
            display_name: None,
            stream: None,
            clock: Default::default(),
            nodes: vec![a_rendered_node("camera"), a_rendered_node("back")],
            links: vec![a_rendered_link(
                "L2",
                ("camera", "video"),
                ("back", "video_in"),
            )],
            references: Vec::new(),
            machine: "vm".to_string(),
            groups: Vec::new(),
            studio: None,
        };
        resolution
            .variables_as_drawn
            .render_into(&mut graph_response);
        assert!(
            graph_response
                .links
                .iter()
                .all(|rendered| rendered.source.node != "camera"
                    || rendered.target.node == "set-camera"),
            "{:?}",
            graph_response.links
        );
        assert_ne!(graph_response.nodes[1].config_checksum, 0);
    }

    #[test]
    fn the_render_carries_each_runtime_links_id_and_drops_a_drawn_link_whose_reader_is_gone() {
        let resolution = resolved(a_camera_variable_read_by_two_displays()).unwrap();
        let mut graph_response = GraphResponse {
            asset: None,
            display_name: None,
            stream: None,
            clock: Default::default(),
            nodes: vec![a_rendered_node("camera"), a_rendered_node("back")],
            links: vec![a_rendered_link(
                "L2",
                ("camera", "video"),
                ("back", "video_in"),
            )],
            references: Vec::new(),
            machine: "vm".to_string(),
            groups: Vec::new(),
            studio: None,
        };
        resolution
            .variables_as_drawn
            .render_into(&mut graph_response);

        let rendered_links: Vec<(String, String, String)> = graph_response
            .links
            .iter()
            .map(|rendered| {
                (
                    rendered.id.clone(),
                    rendered.source.node.clone(),
                    rendered.target.node.clone(),
                )
            })
            .collect();
        assert_eq!(
            rendered_links,
            [
                (
                    String::new(),
                    "camera".to_string(),
                    "set-camera".to_string()
                ),
                (
                    "L2".to_string(),
                    "get-camera-2".to_string(),
                    "back".to_string()
                ),
            ]
        );
        let rendered_nodes: Vec<&str> = graph_response
            .nodes
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(
            rendered_nodes,
            ["camera", "set-camera", "get-camera", "get-camera-2", "back"]
        );
        assert_eq!(graph_response.nodes[1].ports.inputs[0].name, "value");
        assert_eq!(graph_response.nodes[2].ports.outputs[0].name, "value");
    }
}
