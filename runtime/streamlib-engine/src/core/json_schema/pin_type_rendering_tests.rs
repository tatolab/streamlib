// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::core::graph::Graph;
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::test_support::{
    MockChannelsSourceNode, MockPinTypedNode, ensure_test_mocks_registered,
};

/// The node `graph` renders for one node of `node_type`, placed alone.
fn rendered_node_of_type(
    node_type: streamlib_node_schema::NodeClassImportPath,
) -> serde_json::Value {
    ensure_test_mocks_registered();
    let mut graph = Graph::new();
    let added = graph
        .traversal_mut()
        .add_v(NodeSpec::new(node_type, serde_json::Value::Null))
        .expect("the node is named");
    let node = added.first().expect("the mock is registered");
    serde_json::to_value(GraphNodeOutput::from(node)).expect("a node renders")
}

/// The ports the node catalog serves for `node_type`, inputs then outputs.
fn catalog_ports_of_type(
    node_type: streamlib_node_schema::NodeClassImportPath,
) -> Vec<serde_json::Value> {
    ensure_test_mocks_registered();
    let descriptor = NODE_REGISTRY
        .descriptor(&node_type)
        .expect("the mock is registered");
    descriptor
        .inputs
        .iter()
        .chain(&descriptor.outputs)
        .map(|port| serde_json::to_value(PortDescriptorOutput::from(port)).expect("renders"))
        .collect()
}

#[test]
fn graph_serves_a_rust_ports_declared_pin_type_resolved_beside_its_family() {
    let node = rendered_node_of_type(MockPinTypedNode::node_class_import_path());

    let gain = &node["ports"]["inputs"][0];
    assert_eq!(gain["name"], "gain");
    assert_eq!(gain["pin_type"], json!({"type": "float"}));
    assert_eq!(gain["flow"], "value");
    assert_eq!(gain["family"], json!({"family": "channels"}));

    let frames = &node["ports"]["outputs"][0];
    assert_eq!(frames["name"], "frames");
    assert_eq!(frames["pin_type"], json!({"type": "texture"}));
    assert_eq!(frames["flow"], "stream");
    assert_eq!(frames["family"], json!({"family": "image"}));
}

#[test]
fn the_catalog_serves_a_rust_ports_declared_pin_type_beside_its_family() {
    let ports = catalog_ports_of_type(MockPinTypedNode::node_class_import_path());

    assert_eq!(
        ports[0]["pin_type"],
        json!({"kind": "is", "pin_type": {"type": "float"}})
    );
    assert_eq!(ports[0]["flow"], "value");
    assert_eq!(ports[0]["family"], json!({"family": "channels"}));
    assert_eq!(
        ports[1]["pin_type"],
        json!({"kind": "is", "pin_type": {"type": "texture"}})
    );
    assert_eq!(ports[1]["flow"], "stream");
    assert_eq!(ports[1]["family"], json!({"family": "image"}));
}

/// Mock node whose typed inputs state no delivery profile, except one, and
/// whose ports state a flow other than their type's default.
#[crate::node(
    execution = reactive,
    input("gain", pin_type = Float),
    input("levels", pin_type = Float, flow = Stream),
    input("audio", pin_type = Audio),
    input("frames", family = Image),
    input("every_frame", pin_type = Texture, delivery_profile = "ordered"),
    output("still", pin_type = Texture, flow = Value),
)]
struct MockNodeReadingByItsPins;

impl crate::core::ReactiveNode for MockNodeReadingByItsPins::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Each port's name, flow and delivery profile, as `ports` renders them.
fn flows_and_deliveries(ports: &[serde_json::Value]) -> Vec<(String, String, Option<String>)> {
    ports
        .iter()
        .map(|port| {
            (
                port["name"].as_str().unwrap().to_string(),
                port["flow"].as_str().unwrap().to_string(),
                port["delivery_profile"].as_str().map(str::to_string),
            )
        })
        .collect()
}

#[test]
fn graph_and_the_catalog_serve_the_flow_a_port_states_and_the_delivery_its_pin_defaults_to() {
    let read = |name: &str, flow: &str, delivery: Option<&str>| {
        (
            name.to_string(),
            flow.to_string(),
            delivery.map(str::to_string),
        )
    };
    let expected = vec![
        read("gain", "value", Some("newest")),
        read("levels", "stream", Some("ordered")),
        read("audio", "stream", Some("ordered")),
        read("frames", "stream", Some("newest")),
        read("every_frame", "stream", Some("ordered")),
        read("still", "value", None),
    ];

    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<MockNodeReadingByItsPins::Node>());
    let node = rendered_node_of_type(MockNodeReadingByItsPins::node_class_import_path());
    let graph_ports: Vec<serde_json::Value> = node["ports"]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .chain(node["ports"]["outputs"].as_array().unwrap())
        .cloned()
        .collect();
    assert_eq!(flows_and_deliveries(&graph_ports), expected);

    let catalog_ports = catalog_ports_of_type(MockNodeReadingByItsPins::node_class_import_path());
    assert_eq!(flows_and_deliveries(&catalog_ports), expected);
}

#[test]
fn a_port_with_no_exact_pin_type_serves_neither_field_in_graph_or_the_catalog() {
    let node = rendered_node_of_type(MockChannelsSourceNode::node_class_import_path());
    let audio = &node["ports"]["outputs"][0];
    assert_eq!(audio["family"], json!({"family": "channels"}));
    assert!(audio.get("pin_type").is_none(), "{audio}");
    assert!(audio.get("flow").is_none(), "{audio}");

    let catalog_audio = &catalog_ports_of_type(MockChannelsSourceNode::node_class_import_path())[0];
    assert!(catalog_audio.get("pin_type").is_none(), "{catalog_audio}");
    assert!(catalog_audio.get("flow").is_none(), "{catalog_audio}");
}

#[test]
fn a_graph_interface_port_of_the_image_family_is_a_texture_and_of_another_family_untyped() {
    use crate::core::graph::graph_interface_resolution::{
        GraphInterfacePort, GraphInterfacePortDirection,
    };
    let rendered = |family| {
        let declared = GraphInterfacePort {
            interface_node: "camera".to_string(),
            port: "frames".to_string(),
            family,
            direction: GraphInterfacePortDirection::Input,
            linked_ports: Vec::new(),
            runs_as_a_source_node: false,
        };
        serde_json::to_value(declared.rendered_as_a_port_named("value")).expect("renders")
    };

    let image = rendered(Some(crate::core::descriptors::PortFamily::Image));
    assert_eq!(image["pin_type"], json!({"type": "texture"}));
    assert_eq!(image["flow"], "stream");

    let channels = rendered(Some(crate::core::descriptors::PortFamily::Channels));
    assert!(channels.get("pin_type").is_none(), "{channels}");
    let unknown = rendered(None);
    assert!(unknown.get("pin_type").is_none(), "{unknown}");
}
