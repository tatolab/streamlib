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
use crate::core::graph_snapshot::{GraphSnapshot, GraphSnapshotLink, GraphSnapshotNode};
use crate::core::json_schema::{
    GraphResponse, LinkOutput, LinkPortRefOutput, ProcessorNodeOutput, ProcessorNodePortsOutput,
};
use crate::core::{Error, Result};
use streamlib_processor_schema::BUILT_IN_NODE_CLASS_MODULE;

/// The short name of the node type that names a variable's value.
pub const SET_VARIABLE_NODE_TYPE_SHORT_NAME: &str = "SetVariable";

/// The short name of the node type that reads a variable.
pub const GET_VARIABLE_NODE_TYPE_SHORT_NAME: &str = "GetVariable";

/// The config key both variable nodes name their variable under.
pub const GRAPH_VARIABLE_NAME_CONFIG_KEY: &str = "variable";

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
    /// The config each node had before a reference naming a get was resolved,
    /// by node name.
    pub configs_naming_a_get_by_node_name: HashMap<String, serde_json::Value>,
}

/// The component key a variable node, and a drawn link with an end on one,
/// renders its variable under.
pub const GRAPH_VARIABLE_COMPONENT_KEY: &str = "graph_variable";

impl GraphVariablesAsDrawn {
    /// Render the variables into `graph_response`, a render of the graph the
    /// runtime runs: each variable node and its drawn links in, each link a
    /// get resolved to out, its live state carried on the get's drawn link,
    /// and each config naming a get as drawn.
    pub fn render_into(&self, graph_response: &mut GraphResponse) {
        if self.variable_nodes.is_empty() {
            return;
        }
        let is_the_runtime_link = |rendered: &LinkOutput, link: &GraphSnapshotLink| {
            rendered.source == link.source && rendered.target == link.target
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
        let variable_by_node_name: HashMap<&str, &str> = self
            .variable_nodes
            .iter()
            .filter_map(|node| {
                node.config
                    .get(GRAPH_VARIABLE_NAME_CONFIG_KEY)
                    .and_then(serde_json::Value::as_str)
                    .map(|variable| (node.name.as_str(), variable))
            })
            .collect();
        for (drawn, drawn_position) in self
            .links_touching_a_variable_node
            .iter()
            .zip(&self.positions_of_links_touching_a_variable_node)
        {
            let variable = variable_by_node_name
                .get(drawn.source.node.as_str())
                .or_else(|| variable_by_node_name.get(drawn.target.node.as_str()))
                .copied()
                .unwrap_or_default();
            let runtime_link = self
                .links_resolved_from_a_variable
                .iter()
                .find(|resolved| {
                    resolved.get_variable_node == drawn.source.node
                        && resolved.link.target == drawn.target
                })
                .and_then(|resolved| {
                    rendered_links_resolved_from_a_variable
                        .iter()
                        .find(|rendered| is_the_runtime_link(rendered, &resolved.link))
                });
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
                    source: drawn.source.clone(),
                    target: drawn.target.clone(),
                    capacity: runtime_link
                        .map(|rendered| rendered.capacity)
                        .unwrap_or_default(),
                    state: runtime_link
                        .map(|rendered| rendered.state.clone())
                        .unwrap_or_default(),
                    error_reason: runtime_link.and_then(|rendered| rendered.error_reason.clone()),
                    components,
                },
            );
        }
        for node in &mut graph_response.nodes {
            if let Some(drawn_config) = self.configs_naming_a_get_by_node_name.get(&node.name) {
                node.config = Some(drawn_config.clone());
            }
        }
        for (variable_node, drawn_position) in self
            .variable_nodes
            .iter()
            .zip(&self.variable_node_positions)
        {
            let mut components = serde_json::Map::new();
            components.insert(
                GRAPH_VARIABLE_COMPONENT_KEY.to_string(),
                serde_json::json!({
                    GRAPH_VARIABLE_NAME_CONFIG_KEY:
                        variable_by_node_name.get(variable_node.name.as_str())
                }),
            );
            let position = (*drawn_position).min(graph_response.nodes.len());
            graph_response.nodes.insert(
                position,
                ProcessorNodeOutput {
                    id: String::new(),
                    processor_type: variable_node.processor_type.clone(),
                    name: variable_node.name.clone(),
                    config: Some(variable_node.config.clone()),
                    config_checksum: 0,
                    ports: ProcessorNodePortsOutput {
                        inputs: Vec::new(),
                        outputs: Vec::new(),
                    },
                    components,
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
}

/// The import path a graph names `SetVariable` by.
pub fn set_variable_node_type() -> ProcessorClassImportPath {
    variable_node_type(SET_VARIABLE_NODE_TYPE_SHORT_NAME)
}

/// The import path a graph names `GetVariable` by.
pub fn get_variable_node_type() -> ProcessorClassImportPath {
    variable_node_type(GET_VARIABLE_NODE_TYPE_SHORT_NAME)
}

fn variable_node_type(short_name: &str) -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(format!("{BUILT_IN_NODE_CLASS_MODULE}:{short_name}"))
        .expect("a variable node's import path is well formed")
}

/// Whether `node_type` is `SetVariable` or `GetVariable`.
pub fn is_a_graph_variable_node_type(node_type: &ProcessorClassImportPath) -> bool {
    *node_type == set_variable_node_type() || *node_type == get_variable_node_type()
}

/// What one `SetVariable` holds.
enum ValueOneSetterHolds {
    PortOutput(LinkPortRefOutput),
    NodeReference(String),
}

struct OneSetter<'graph> {
    node: &'graph GraphSnapshotNode,
    holds: ValueOneSetterHolds,
}

impl GraphSnapshot {
    /// Resolve this graph's variables, refusing by name a get with no setter,
    /// two setters of one name, a setter holding both a port's output and a
    /// node reference or neither, and a get read as the kind of value its
    /// variable does not hold. A graph with no variable node resolves to
    /// itself.
    pub fn with_graph_variables_resolved(&self) -> Result<GraphSnapshotWithVariablesResolved> {
        let set_type = set_variable_node_type();
        let get_type = get_variable_node_type();
        let variable_nodes: Vec<&GraphSnapshotNode> = self
            .nodes
            .iter()
            .filter(|node| is_a_graph_variable_node_type(&node.processor_type))
            .collect();
        if variable_nodes.is_empty() {
            return Ok(GraphSnapshotWithVariablesResolved {
                graph_the_runtime_runs: self.clone(),
                variables_as_drawn: GraphVariablesAsDrawn::default(),
            });
        }

        let variable_node_casts: HashSet<String> = variable_nodes
            .iter()
            .map(|node| cast_of(&node.name))
            .collect::<Result<_>>()?;
        let mut variable_of_each_get_by_cast: HashMap<String, String> = HashMap::new();
        let mut setters_by_variable: HashMap<String, OneSetter<'_>> = HashMap::new();
        for node in &variable_nodes {
            let variable = variable_a_node_names(node)?;
            if node.processor_type == get_type {
                refuse_a_key_a_variable_node_does_not_take(
                    node,
                    &[GRAPH_VARIABLE_NAME_CONFIG_KEY],
                )?;
                variable_of_each_get_by_cast.insert(cast_of(&node.name)?, variable);
                continue;
            }
            debug_assert_eq!(node.processor_type, set_type);
            let setter = OneSetter {
                node,
                holds: value_one_setter_holds(self, node, &variable)?,
            };
            if let Some(first) = setters_by_variable.insert(variable.clone(), setter) {
                return Err(Error::GraphError(format!(
                    "the variable `{variable}` is set by both `{}` and `{}`, and a variable \
                     has one setter. Rename one of the variables, or remove one setter",
                    first.node.name, node.name
                )));
            }
        }

        let setter_of = |get_node: &str, variable: &str| -> Result<&OneSetter<'_>> {
            setters_by_variable.get(variable).ok_or_else(|| {
                Error::GraphError(format!(
                    "`GetVariable` node `{get_node}` reads the variable `{variable}`, which no \
                     `SetVariable` sets. The graph sets: {}",
                    names_listed_for_a_refusal(sorted(setters_by_variable.keys()), "no variable")
                ))
            })
        };

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
            if variable_node_casts.contains(&target_cast) {
                continue;
            }
            let Some(variable) = variable_of_each_get_by_cast.get(&source_cast) else {
                return Err(Error::GraphError(format!(
                    "a link leaves `SetVariable` node `{}`, which has no output: read a \
                     variable through a `GetVariable` node",
                    link.source.node
                )));
            };
            refuse_a_variable_port_other_than_value(&link.source, "a link leaves")?;
            let setter = setter_of(&link.source.node, variable)?;
            let ValueOneSetterHolds::PortOutput(setter_source) = &setter.holds else {
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
            let mut resolved_config = config_object.clone();
            let mut named_a_get = false;
            for (config_key, config_value) in config_object {
                let Some(referenced) = node_name_a_config_value_references(config_value) else {
                    continue;
                };
                let referenced_cast = cast_of(referenced)?;
                let Some(variable) = variable_of_each_get_by_cast.get(&referenced_cast) else {
                    if variable_node_casts.contains(&referenced_cast) {
                        return Err(Error::GraphError(format!(
                            "node `{}`'s config key `{config_key}` names `SetVariable` node \
                             `{referenced}`: read a variable through a `GetVariable` node",
                            node.name
                        )));
                    }
                    continue;
                };
                let setter = setter_of(referenced, variable)?;
                let ValueOneSetterHolds::NodeReference(referenced_node) = &setter.holds else {
                    return Err(Error::GraphError(format!(
                        "node `{}`'s config key `{config_key}` reads `GetVariable` node \
                         `{referenced}`, and its variable `{variable}` holds a port's \
                         output, which a link reads, never a config key",
                        node.name
                    )));
                };
                resolved_config.insert(
                    config_key.clone(),
                    serde_json::json!({ NODE_REFERENCE_NODE_KEY: referenced_node }),
                );
                named_a_get = true;
            }
            if named_a_get {
                configs_naming_a_get_by_node_name.insert(node.name.clone(), node.config.clone());
            }
            runtime_nodes.push(GraphSnapshotNode {
                config: serde_json::Value::Object(resolved_config),
                ..node.clone()
            });
        }

        for exposed in &self.exposed {
            if variable_node_casts.contains(&cast_of(&exposed.node)?) {
                return Err(Error::GraphError(format!(
                    "the graph exposes `{}/{}`, a variable node's port; expose the port \
                     its `SetVariable` takes",
                    exposed.node, exposed.port
                )));
            }
        }

        Ok(GraphSnapshotWithVariablesResolved {
            graph_the_runtime_runs: GraphSnapshot {
                stream: self.stream.clone(),
                clock: self.clock,
                nodes: runtime_nodes,
                links: runtime_links,
                exposed: self.exposed.clone(),
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
) -> Result<ValueOneSetterHolds> {
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
            if graph.nodes.iter().any(|node| {
                node.processor_type == get_variable_node_type()
                    && cast_of(&node.name).ok() == cast_of(&link.source.node).ok()
            }) {
                return Err(Error::GraphError(format!(
                    "`SetVariable` node `{}` takes its value from `GetVariable` node `{}`; \
                     link it from the port that variable's setter takes",
                    setter.name, link.source.node
                )));
            }
            Ok(ValueOneSetterHolds::PortOutput(link.source.clone()))
        }
        ([], Some(reference)) => node_name_a_config_value_references(reference)
            .map(|referenced| ValueOneSetterHolds::NodeReference(referenced.to_string()))
            .ok_or_else(|| {
                Error::GraphError(format!(
                    "`SetVariable` node `{}`'s `{SET_VARIABLE_NODE_REFERENCE_CONFIG_KEY}` \
                     holds {reference}, and it holds a reference to a node, written \
                     {{\"node\": \"<name>\"}}. Pass the reference `add` returned",
                    setter.name
                ))
            }),
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
    if end.port == GRAPH_VARIABLE_VALUE_PORT_NAME {
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
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(graph: serde_json::Value) -> Result<GraphSnapshotWithVariablesResolved> {
        GraphSnapshot::from_graph_document(graph)
            .expect("the graph parses")
            .with_graph_variables_resolved()
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
                 `SetVariable` sets. The graph sets: camera"
            ),
            "{refusal}"
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
                .configs_naming_a_get_by_node_name["camera-track"],
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
}
