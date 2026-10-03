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

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::json_schema::{ExposedOutputPortOutput, LinkPortRefOutput};
use crate::core::processors::PROCESSOR_REGISTRY;
use crate::core::{Error, Result};

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

    /// Check the graph without loading it: names unique once cast, every
    /// local link end on a node the graph holds, every exposure an output
    /// port a node has, and every `type` registered.
    pub fn validate(&self) -> Result<()> {
        let mut node_names: HashSet<String> = HashSet::new();
        for node in &self.nodes {
            let cast = cast_exposed_name_to_url_safe(&node.name)?;
            if !node_names.insert(cast.to_string()) {
                return Err(Error::NodeNameTaken {
                    name: node.name.clone(),
                    cast: cast.into_owned(),
                });
            }
        }

        for link in &self.links {
            for end in [&link.source, &link.target] {
                if let Some(node) = end.node_on_this_runtime()
                    && !node_names.contains(cast_exposed_name_to_url_safe(node)?.as_ref())
                {
                    return Err(Error::GraphError(format!(
                        "a link names node `{node}`, which the graph does not hold. The graph \
                         holds: {}",
                        names_listed(&node_names)
                    )));
                }
            }
        }

        for exposed in &self.exposed {
            self.refuse_an_exposure_naming_no_output_port(exposed, &node_names)?;
        }

        for node in &self.nodes {
            if PROCESSOR_REGISTRY.port_info(&node.processor_type).is_none() {
                return Err(Error::UnknownProcessorType {
                    ident: node.processor_type.clone(),
                });
            }
        }

        Ok(())
    }

    fn refuse_an_exposure_naming_no_output_port(
        &self,
        exposed: &ExposedOutputPortOutput,
        node_names: &HashSet<String>,
    ) -> Result<()> {
        let node_cast = cast_exposed_name_to_url_safe(&exposed.node)?;
        let Some(node) = self.nodes.iter().find(|node| {
            cast_exposed_name_to_url_safe(&node.name).is_ok_and(|cast| cast == node_cast)
        }) else {
            return Err(Error::GraphError(format!(
                "the graph exposes `{}/{}`, and holds no node `{}`. The graph holds: {}",
                exposed.node,
                exposed.port,
                exposed.node,
                names_listed(node_names)
            )));
        };
        let port_cast = cast_exposed_name_to_url_safe(&exposed.port)?;
        let output_port_names: Vec<String> = PROCESSOR_REGISTRY
            .port_info(&node.processor_type)
            .map(|(_, outputs)| outputs.into_iter().map(|port| port.name).collect())
            .unwrap_or_default();
        if output_port_names.iter().any(|name| *name == port_cast) {
            return Ok(());
        }
        Err(Error::GraphError(format!(
            "the graph exposes `{}/{}`, and node `{}` has no output port `{}`. Its output ports \
             are: {}",
            exposed.node,
            exposed.port,
            exposed.node,
            exposed.port,
            if output_port_names.is_empty() {
                "none".to_string()
            } else {
                output_port_names.join(", ")
            }
        )))
    }
}

fn names_listed(node_names: &HashSet<String>) -> String {
    let mut sorted: Vec<&str> = node_names.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    if sorted.is_empty() {
        "no node".to_string()
    } else {
        sorted.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_CAMERA_CLASS: &str = "my_app.nodes:CameraNode";

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
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [
                {"name": "FrontCam", "type": A_CAMERA_CLASS},
                {"name": "frontcam", "type": A_CAMERA_CLASS}
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
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "camera", "type": A_CAMERA_CLASS}],
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
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "camera", "type": A_CAMERA_CLASS}],
            "exposed": [{"node": "display", "port": "video"}]
        }))
        .unwrap();

        let refusal = graph.validate().unwrap_err().to_string();

        assert!(refusal.contains("holds no node `display`"), "{refusal}");
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
