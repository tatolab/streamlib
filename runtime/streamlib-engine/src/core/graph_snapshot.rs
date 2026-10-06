// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph as data: the spec a runtime loads, in the one shape `graph`
//! renders.
//!
//! A [`GraphSnapshot`] is the spec keys of a `graph` document — `stream`,
//! `nodes[].name` / `type` / `config`, `links[].source` / `target` and
//! `exposed` — and nothing else, so `graph`'s own output reads into one with
//! its live keys skipped, and any other key is refused by name. Loading one is
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
///
/// Read only through [`Self::from_json_str`] and [`Self::from_graph_document`],
/// which refuse a key the runtime does not read.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GraphSnapshot {
    /// The stream this graph is. Absent on a graph no stream was loaded as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,

    /// Every node, each under the name links and exposures name it by.
    pub nodes: Vec<GraphSnapshotNode>,

    /// Every link, each end a node's port.
    pub links: Vec<GraphSnapshotLink>,

    /// The output ports the stream exposes.
    pub exposed: Vec<ExposedOutputPortOutput>,
}

/// The spec keys a graph document is read through, before its keys are checked.
#[derive(Deserialize)]
struct GraphSnapshotAsWritten {
    #[serde(default)]
    stream: Option<String>,
    nodes: Vec<GraphSnapshotNode>,
    #[serde(default)]
    links: Vec<GraphSnapshotLink>,
    #[serde(default)]
    exposed: Vec<ExposedOutputPortOutput>,
}

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
    spec_keys: &["stream", "nodes", "links", "exposed"],
    live_keys: &["extensions", "runtime_name"],
};

const GRAPH_NODE_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "a node",
    spec_keys: &["name", "type", "config"],
    live_keys: &["id", "config_checksum", "ports", "components"],
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

const GRAPH_EXPOSURE_KEYS: GraphDocumentObjectKeys = GraphDocumentObjectKeys {
    object_kind: "an exposure",
    spec_keys: &["node", "port"],
    live_keys: &[],
};

/// One node of a [`GraphSnapshot`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSnapshotNode {
    /// The node's name, unique in the graph once cast.
    pub name: String,

    /// The import path of the class the node is.
    #[serde(rename = "type")]
    pub processor_type: ProcessorClassImportPath,

    /// The node's config. Absent reads as `{}`, the config `add_node` takes
    /// when it is given none.
    #[serde(default = "a_config_naming_no_setting")]
    pub config: serde_json::Value,
}

fn a_config_naming_no_setting() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
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
            stream,
            nodes,
            links,
            exposed,
        } = GraphSnapshotAsWritten::deserialize(&graph_document)
            .map_err(|e| Error::GraphError(format!("the graph does not parse: {e}")))?;
        Ok(Self {
            stream,
            nodes,
            links,
            exposed,
        })
    }

    /// Serialize the graph as pretty-printed JSON.
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| Error::GraphError(format!("the graph does not serialize: {e}")))
    }

    /// Check the graph without loading it: every `type` one this runtime has
    /// on this floor, every config one its type takes, names unique once cast,
    /// every link end a port of the right direction on a node the graph holds,
    /// and every exposure an output port a node has, named once.
    pub fn validate(&self) -> Result<()> {
        let mut nodes_by_cast_name: HashMap<String, &GraphSnapshotNode> = HashMap::new();
        for node in &self.nodes {
            PROCESSOR_REGISTRY.refuse_a_node_this_runtime_cannot_add(
                &node.name,
                &node.processor_type,
                &node.config,
            )?;
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
                let Some(node) =
                    nodes_by_cast_name.get(cast_exposed_name_to_url_safe(&end.node)?.as_ref())
                else {
                    return Err(Error::GraphError(format!(
                        "a link names node `{}`, which the graph does not hold. The graph \
                         holds: {}",
                        &end.node,
                        the_names_the_graph_holds()
                    )));
                };
                refuse_a_port_the_node_does_not_have(&end.node, node, &end.port, direction)?;
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

/// Refuse the first key of `graph_document` the runtime does not read — at the
/// top, on a node, a link, a link end or an exposure — naming it and where it
/// sits, before the spec is read, so a misspelled key is named rather than
/// reported as the key it stands in for being missing. An object the walk
/// expects and does not find is left for the read to refuse.
fn refuse_a_key_this_runtime_does_not_read(graph_document: &serde_json::Value) -> Result<()> {
    type GraphDocumentObject = serde_json::Map<String, serde_json::Value>;
    fn text_at<'o>(object: &'o GraphDocumentObject, key: &str) -> &'o str {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
    }
    fn port_address_of(object: &GraphDocumentObject) -> String {
        format!("{}/{}", text_at(object, "node"), text_at(object, "port"))
    }
    fn port_address_at(link: &GraphDocumentObject, end_key: &str) -> String {
        link.get(end_key)
            .and_then(serde_json::Value::as_object)
            .map_or_else(|| "?".to_string(), port_address_of)
    }
    let objects_of = |key: &str| {
        graph_document
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_object)
    };

    if let Some(graph_object) = graph_document.as_object() {
        refuse_a_key_no_such_object_holds(graph_object, &GRAPH_DOCUMENT_KEYS, || {
            "the graph".to_string()
        })?;
    }
    for node in objects_of("nodes") {
        refuse_a_key_no_such_object_holds(node, &GRAPH_NODE_KEYS, || {
            format!("node `{}`", text_at(node, "name"))
        })?;
    }
    for link in objects_of("links") {
        let link_named = || {
            format!(
                "the link `{}` → `{}`",
                port_address_at(link, "source"),
                port_address_at(link, "target")
            )
        };
        refuse_a_key_no_such_object_holds(link, &GRAPH_LINK_KEYS, link_named)?;
        for end_key in ["source", "target"] {
            let Some(link_end) = link.get(end_key).and_then(serde_json::Value::as_object) else {
                continue;
            };
            if let Some(runtime_name) = link_end.get("runtime_name") {
                return Err(Error::GraphError(format!(
                    "the link end `{}` names the runtime `{}`, and a link end on another \
                     runtime is not something a graph holds: both ends of a link are \
                     `{{node, port}}` on the runtime that loads it",
                    port_address_of(link_end),
                    runtime_name.as_str().unwrap_or("?")
                )));
            }
            refuse_a_key_no_such_object_holds(link_end, &GRAPH_LINK_END_KEYS, || {
                format!("the {end_key} end of {}", link_named())
            })?;
        }
    }
    for exposure in objects_of("exposed") {
        refuse_a_key_no_such_object_holds(exposure, &GRAPH_EXPOSURE_KEYS, || {
            format!("the exposure `{}`", port_address_of(exposure))
        })?;
    }
    Ok(())
}

/// Refuse `object`'s first key that is neither a spec key nor a live key of its
/// kind, naming it where `object_named` says it sits.
fn refuse_a_key_no_such_object_holds(
    object: &serde_json::Map<String, serde_json::Value>,
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
                 "target": {"node": "display", "port": "video"}}
            ],
            "exposed": [{"node": "camera", "port": "video"}],
            "extensions": [],
            "runtime_name": "rig-desk-a1b2"
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

    /// An end spelled `{runtime_name, node, port}` names a port on a runtime,
    /// and a graph holds no link with an end anywhere but on the runtime that
    /// loads it. Read as `{node, port}` with the runtime dropped, it would load
    /// as a local end on whichever node happens to share the name.
    #[test]
    fn a_link_end_naming_a_runtime_is_refused_naming_that_runtime() {
        let camera_class = a_registered_camera_class();
        for (source, target) in [
            (
                serde_json::json!({"runtime_name": "studio-cam-9f3c", "node": "front", "port": "video"}),
                serde_json::json!({"node": "back", "port": "frames_in"}),
            ),
            (
                serde_json::json!({"node": "front", "port": "video"}),
                serde_json::json!({"runtime_name": "studio-cam-9f3c", "node": "back", "port": "frames_in"}),
            ),
        ] {
            let refusal = GraphSnapshot::from_graph_document(serde_json::json!({
                "nodes": [{"name": "front", "type": camera_class},
                          {"name": "back", "type": camera_class}],
                "links": [{"source": source, "target": target}]
            }))
            .expect_err("a link end on another runtime is not something a graph holds")
            .to_string();

            assert!(refusal.contains("`studio-cam-9f3c`"), "{refusal}");
            assert!(refusal.contains("another runtime"), "{refusal}");
        }
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

    /// A stream names no runtime version, so the refusal names this runtime's
    /// own beside the type it lacks.
    #[test]
    fn a_built_in_type_this_runtime_lacks_is_refused_naming_it_and_this_runtimes_version() {
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "newthing", "type": "tatolab.stream:NewThing"}]
        }))
        .unwrap();

        let refusal = graph.validate().unwrap_err().to_string();

        assert_eq!(
            refusal,
            format!(
                "this runtime ({}) has no node type `tatolab.stream:NewThing`",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn a_built_in_this_floor_compiles_out_is_refused_naming_this_floor_and_where_it_runs() {
        let compiled_out_here =
            ProcessorClassImportPath::of_built_in_node("GraphSnapshotTestCompiledOutHere");
        PROCESSOR_REGISTRY
            .register_built_in_node_type_absent_on_this_floor(compiled_out_here.clone(), "Plan 9");
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{"name": "elsewhere", "type": compiled_out_here.as_str()}]
        }))
        .unwrap();

        let refusal = graph.validate().unwrap_err();

        assert!(
            matches!(&refusal, Error::BuiltInNodeTypeAbsentOnThisFloor { processor_type, .. } if *processor_type == compiled_out_here),
            "{refusal:?}"
        );
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("`tatolab.stream:GraphSnapshotTestCompiledOutHere`"),
            "{refusal}"
        );
        assert!(refusal.contains("runs on Plan 9 only"), "{refusal}");
        #[cfg(target_os = "macos")]
        assert!(refusal.contains("on macOS"), "{refusal}");
        #[cfg(target_os = "linux")]
        assert!(refusal.contains("on Linux"), "{refusal}");
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

        match graph.validate() {
            Err(Error::NodeConfigRefused {
                node_name,
                processor_type,
                refusal,
            }) => {
                assert_eq!(node_name, "Front");
                assert_eq!(processor_type, source_type);
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

        graph.validate().unwrap();
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
                    "`stream`, `nodes`, `links` and `exposed`",
                ],
            ),
            (
                serde_json::json!({"nodes": [with(a_node.clone(), "confg")]}),
                [
                    "node `camera` carries the key `confg`",
                    "`name`, `type` and `config`",
                ],
            ),
            (
                serde_json::json!({"nodes": [{"name": "camera", "typ": A_CAMERA_CLASS}]}),
                [
                    "node `camera` carries the key `typ`",
                    "`name`, `type` and `config`",
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
            (
                serde_json::json!({"nodes": [a_node.clone()],
                                   "exposed": [{"node": "camera", "port": "video", "level": "public"}]}),
                [
                    "the exposure `camera/video` carries the key `level`",
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
            (
                &definitions["ExposedOutputPortOutput"],
                &GRAPH_EXPOSURE_KEYS,
            ),
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

    #[test]
    fn an_empty_graph_is_valid() {
        let graph = GraphSnapshot::from_json_str(r#"{"nodes": []}"#).unwrap();

        assert!(graph.validate().is_ok());
        assert!(graph.links.is_empty() && graph.exposed.is_empty());
    }
}
