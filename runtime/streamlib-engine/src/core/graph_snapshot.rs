// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph as data: the spec a runtime loads, in the one shape `graph`
//! renders.
//!
//! A [`GraphSnapshot`] is the spec keys of a `graph` document — `stream`,
//! `nodes[].name` / `type` / `config`, `links[].source` / `target` and
//! `exposed` — and nothing else, so `graph`'s own output deserializes into one
//! with its live keys ignored. Loading one is
//! [`Runner::load_graph_snapshot`](crate::core::runtime::Runner::load_graph_snapshot);
//! there is no saver, because the render is the export.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{cast_exposed_name_to_url_safe, node_names_listed_for_a_refusal};
use crate::core::json_schema::{ExposedOutputPortOutput, LinkPortRefOutput};
use crate::core::processors::PROCESSOR_REGISTRY;
use crate::core::{Error, PortDirection, Result};

/// A graph a runtime runs: its nodes by class import path, config and name,
/// the links between their ports, and the output ports the stream exposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSnapshot {
    /// The stream this graph is. Absent on a graph no stream was loaded as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,

    /// Every node, each under the name links and exposures name it by.
    pub nodes: Vec<GraphSnapshotNode>,

    /// Every link, each end a node's port or a port on another runtime.
    #[serde(default)]
    pub links: Vec<GraphSnapshotLink>,

    /// The output ports the stream exposes.
    #[serde(default)]
    pub exposed: Vec<ExposedOutputPortOutput>,
}

/// One node of a [`GraphSnapshot`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSnapshotNode {
    /// The node's name, unique in the graph once cast.
    pub name: String,

    /// The import path of the class the node is.
    #[serde(rename = "type")]
    pub processor_type: ProcessorClassImportPath,

    /// The node's config.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// One link of a [`GraphSnapshot`], from an output port to an input port.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSnapshotLink {
    /// The output port the link carries from.
    pub source: LinkPortRefOutput,

    /// The input port the link carries into.
    pub target: LinkPortRefOutput,
}

impl GraphSnapshot {
    /// Read a graph from JSON — a `graph` document or the spec keys alone.
    pub fn from_json_str(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| Error::GraphError(format!("the graph does not parse: {e}")))
    }

    /// Read a graph from a parsed `graph` document, its live keys ignored.
    pub fn from_graph_document(graph_document: serde_json::Value) -> Result<Self> {
        serde_json::from_value(graph_document)
            .map_err(|e| Error::GraphError(format!("the graph does not parse: {e}")))
    }

    /// Serialize the graph as pretty-printed JSON.
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| Error::GraphError(format!("the graph does not serialize: {e}")))
    }

    /// Check the graph without loading it: every `type` registered, names
    /// unique once cast, every link end an address the mesh carries or a port
    /// of the right direction on a node the graph holds, and every exposure an
    /// output port a node has, named once.
    pub fn validate(&self) -> Result<()> {
        let mut nodes_by_cast_name: HashMap<String, &GraphSnapshotNode> = HashMap::new();
        for node in &self.nodes {
            if PROCESSOR_REGISTRY.port_info(&node.processor_type).is_none() {
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
                if end.mesh_port_address().transpose()?.is_some() {
                    continue;
                }
                let Some(node) =
                    nodes_by_cast_name.get(cast_exposed_name_to_url_safe(end.node())?.as_ref())
                else {
                    return Err(Error::GraphError(format!(
                        "a link names node `{}`, which the graph does not hold. The graph \
                         holds: {}",
                        end.node(),
                        the_names_the_graph_holds()
                    )));
                };
                refuse_a_port_the_node_does_not_have(end.node(), node, end.port(), direction)?;
            }
        }

        let mut exposures_seen: HashSet<(String, String)> = HashSet::new();
        for exposed in &self.exposed {
            let node_cast = cast_exposed_name_to_url_safe(&exposed.node)?.into_owned();
            let port_cast = cast_exposed_name_to_url_safe(&exposed.port)?.into_owned();
            let Some(node) = nodes_by_cast_name.get(&node_cast) else {
                return Err(Error::GraphError(format!(
                    "the graph exposes `{}/{}`, and holds no node `{}`. The graph holds: {}",
                    exposed.node,
                    exposed.port,
                    exposed.node,
                    the_names_the_graph_holds()
                )));
            };
            refuse_a_port_the_node_does_not_have(
                &exposed.node,
                node,
                &exposed.port,
                PortDirection::Output,
            )?;
            if !exposures_seen.insert((node_cast, port_cast)) {
                return Err(Error::GraphError(format!(
                    "the graph exposes `{}/{}` twice",
                    exposed.node, exposed.port
                )));
            }
        }

        Ok(())
    }
}

/// Refuse `port` unless `node`'s type declares it in `direction`, listing the
/// ports it does declare there.
fn refuse_a_port_the_node_does_not_have(
    node_name: &str,
    node: &GraphSnapshotNode,
    port: &str,
    direction: PortDirection,
) -> Result<()> {
    let port_cast = cast_exposed_name_to_url_safe(port)?;
    let port_names: Vec<String> = PROCESSOR_REGISTRY
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
        if port_names.is_empty() {
            "none".to_string()
        } else {
            port_names.join(", ")
        }
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ))
            .with_output(crate::core::descriptors::PortDescriptor::new(
                "video", "", false,
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
                 "target": {"node": "display", "port": "video"}},
                {"source": {"runtime_name": "rig", "node": "mic", "port": "audio"},
                 "target": {"node": "display", "port": "audio"}}
            ],
            "exposed": [{"node": "camera", "port": "video"}],
            "extensions": [],
            "mesh": {"mesh_name": "default"}
        });

        let graph = GraphSnapshot::from_graph_document(graph_document).unwrap();

        assert_eq!(graph.stream.as_deref(), Some("main"));
        assert_eq!(graph.nodes[0].name, "camera");
        assert_eq!(graph.nodes[0].processor_type.as_str(), A_CAMERA_CLASS);
        assert_eq!(graph.nodes[0].config, serde_json::json!({"fps": 30}));
        assert_eq!(
            graph.links[0].source,
            LinkPortRefOutput::OnThisRuntime {
                node: "camera".into(),
                port: "video".into()
            }
        );
        assert_eq!(
            graph.links[1].source,
            LinkPortRefOutput::OnAnotherRuntime {
                runtime_name: "rig".into(),
                node: "mic".into(),
                port: "audio".into()
            },
            "a remote end must not be read as a local one with its runtime dropped"
        );
        assert_eq!(
            graph.exposed,
            vec![ExposedOutputPortOutput {
                node: "camera".into(),
                port: "video".into()
            }]
        );
    }

    #[test]
    fn a_graph_serializes_exactly_the_spec_keys() {
        let graph = GraphSnapshot {
            stream: None,
            nodes: vec![GraphSnapshotNode {
                name: "camera".into(),
                processor_type: ProcessorClassImportPath::new(A_CAMERA_CLASS).unwrap(),
                config: serde_json::json!({}),
            }],
            links: vec![],
            exposed: vec![],
        };

        let serialized = serde_json::to_value(&graph).unwrap();

        assert_eq!(
            serialized,
            serde_json::json!({
                "nodes": [{"name": "camera", "type": A_CAMERA_CLASS, "config": {}}],
                "links": [],
                "exposed": []
            })
        );
        assert_eq!(
            GraphSnapshot::from_json_str(&graph.to_json_string().unwrap()).unwrap(),
            graph
        );
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

        match graph.validate() {
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

        let refusal = graph.validate().unwrap_err().to_string();

        assert!(refusal.contains("`display`"), "{refusal}");
        assert!(refusal.contains("camera"), "{refusal}");
    }

    #[test]
    fn an_exposure_naming_a_node_the_graph_does_not_hold_is_refused() {
        let camera_class = a_registered_camera_class();
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "camera", "type": camera_class}],
            "exposed": [{"node": "display", "port": "video"}]
        }))
        .unwrap();

        let refusal = graph.validate().unwrap_err().to_string();

        assert!(refusal.contains("holds no node `display`"), "{refusal}");
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

        let refusal = graph.validate().unwrap_err().to_string();

        assert!(refusal.contains("no output port `frames_in`"), "{refusal}");
        assert!(refusal.contains("video"), "{refusal}");
    }

    #[test]
    fn a_remote_end_the_mesh_cannot_address_is_refused_before_anything_loads() {
        let camera_class = a_registered_camera_class();
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "back", "type": camera_class}],
            "links": [{"source": {"runtime_name": "bench/cam", "node": "camera", "port": "video"},
                       "target": {"node": "back", "port": "frames_in"}}]
        }))
        .unwrap();

        let refusal = graph.validate().unwrap_err().to_string();

        assert!(refusal.contains("runtime name"), "{refusal}");
    }

    #[test]
    fn an_unregistered_type_is_refused_as_unknown() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "ghost", "type": "my_app.nodes:NotARegisteredNode"}]
        }))
        .unwrap();

        match graph.validate() {
            Err(Error::UnknownProcessorType { ident }) => {
                assert_eq!(ident.as_str(), "my_app.nodes:NotARegisteredNode");
            }
            other => panic!("expected UnknownProcessorType, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_graph_is_valid() {
        let graph = GraphSnapshot::from_json_str(r#"{"nodes": []}"#).unwrap();

        assert!(graph.validate().is_ok());
        assert!(graph.links.is_empty() && graph.exposed.is_empty());
    }
}
