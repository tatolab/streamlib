// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::core::graph::Graph;
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::test_support::{MockPinTypedNode, ensure_test_mocks_registered};

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
fn graph_serves_a_rust_ports_declared_pin_type_resolved() {
    let node = rendered_node_of_type(MockPinTypedNode::node_class_import_path());

    let gain = &node["ports"]["inputs"][0];
    assert_eq!(gain["name"], "gain");
    assert_eq!(gain["pin_type"], json!({"type": "float"}));
    assert_eq!(gain["flow"], "value");
    assert!(gain.get("family").is_none(), "{gain}");

    let frames = &node["ports"]["outputs"][0];
    assert_eq!(frames["name"], "frames");
    assert_eq!(
        frames["pin_type"],
        json!({"type": "texture", "pixel_formats": ["rgba8_unorm"]})
    );
    assert_eq!(frames["flow"], "stream");
    assert!(frames.get("family").is_none(), "{frames}");
}

#[test]
fn the_catalog_serves_a_rust_ports_declared_pin_type_and_no_family() {
    let ports = catalog_ports_of_type(MockPinTypedNode::node_class_import_path());

    assert_eq!(
        ports[0]["pin_type"],
        json!({"kind": "is", "pin_type": {"type": "float"}})
    );
    assert_eq!(ports[0]["flow"], "value");
    assert!(ports[0].get("family").is_none(), "{}", ports[0]);
    assert_eq!(
        ports[1]["pin_type"],
        json!({"kind": "is", "pin_type": {"type": "texture", "pixel_formats": ["rgba8_unorm"]}})
    );
    assert_eq!(ports[1]["flow"], "stream");
    assert!(ports[1].get("family").is_none(), "{}", ports[1]);
}

/// Mock node whose typed inputs state no delivery profile, except one, and
/// whose ports state a flow other than their type's default.
#[crate::node(
    execution = reactive,
    input("gain", pin_type = Float),
    input("levels", pin_type = Float, flow = Stream),
    input("audio", pin_type = Audio),
    input("frames", pin_type = Texture),
    input("every_frame", pin_type = Texture, delivery_profile = "ordered"),
    output("still", pin_type = Texture, formats = ["rgba8_unorm"], flow = Value),
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

/// The type of a node whose one output, `port`, declares no pin type; registered once under
/// `short_name`.
fn a_node_type_whose_one_output_declares_no_pin_type(
    registered: &'static std::sync::Once,
    short_name: &str,
    port: &str,
) -> streamlib_node_schema::NodeClassImportPath {
    use crate::core::descriptors::{NodeClassShortName, NodeDescriptor, PortDescriptor};
    let node_type = streamlib_node_schema::NodeClassImportPath::new(format!(
        "{}::{short_name}",
        module_path!()
    ))
    .expect("a test's node type is a valid import path");
    registered.call_once(|| {
        NODE_REGISTRY
            .register_descriptor_only(
                NodeDescriptor::new(
                    NodeClassShortName::new(short_name).expect("a valid short name"),
                    node_type.clone(),
                    "A node whose one output declares no pin type",
                )
                .with_output(PortDescriptor {
                    pin_declaration: None,
                    ..PortDescriptor::new(
                        port,
                        "",
                        false,
                        crate::core::descriptors::PinDeclaration::exactly(
                            crate::core::descriptors::PinType::Float { exact_size: None },
                        ),
                    )
                }),
            )
            .expect("the node type registers");
    });
    node_type
}

#[test]
fn a_port_with_no_exact_pin_type_serves_neither_field_in_graph_or_the_catalog() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    let node_type = a_node_type_whose_one_output_declares_no_pin_type(
        &REGISTERED,
        "MockUntypedChannelsSourceNode",
        "audio",
    );
    let node = rendered_node_of_type(node_type.clone());
    let audio = &node["ports"]["outputs"][0];
    assert!(audio.get("family").is_none(), "{audio}");
    assert!(audio.get("pin_type").is_none(), "{audio}");
    assert!(audio.get("flow").is_none(), "{audio}");

    let catalog_audio = &catalog_ports_of_type(node_type)[0];
    assert!(catalog_audio.get("pin_type").is_none(), "{catalog_audio}");
    assert!(catalog_audio.get("flow").is_none(), "{catalog_audio}");
}

#[test]
fn a_graph_interface_port_renders_the_pin_type_it_carries_and_none_while_untyped() {
    use crate::core::graph::graph_interface_resolution::{
        GraphInterfacePort, GraphInterfacePortDirection,
    };
    let rendered = |pin_type| {
        let declared = GraphInterfacePort {
            interface_node: "camera".to_string(),
            port: "frames".to_string(),
            pin_type,
            direction: GraphInterfacePortDirection::Input,
            linked_ports: Vec::new(),
            runs_as_a_source_node: false,
        };
        serde_json::to_value(declared.rendered_as_a_port_named("value")).expect("renders")
    };

    let image = rendered(Some(crate::core::descriptors::PinType::Texture {
        pixel_formats: Vec::new(),
    }));
    assert_eq!(image["pin_type"], json!({"type": "texture"}));
    assert_eq!(image["flow"], "stream");

    let unknown = rendered(None);
    assert!(unknown.get("pin_type").is_none(), "{unknown}");
}

/// Mock node whose Texture ports state pixel formats.
#[crate::node(
    execution = reactive,
    input("frames", pin_type = Texture, formats = ["rgba8_unorm", "bgra8_unorm"]),
    output("picture", pin_type = Texture, formats = ["rgba8_unorm"]),
)]
struct MockNodeStatingTextureFormats;

impl crate::core::ReactiveNode for MockNodeStatingTextureFormats::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

#[test]
fn graph_and_the_catalog_serve_the_formats_a_texture_pin_states() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<MockNodeStatingTextureFormats::Node>());
    let frames = json!({"type": "texture", "pixel_formats": ["rgba8_unorm", "bgra8_unorm"]});
    let picture = json!({"type": "texture", "pixel_formats": ["rgba8_unorm"]});

    let catalog_ports =
        catalog_ports_of_type(MockNodeStatingTextureFormats::node_class_import_path());
    assert_eq!(
        catalog_ports[0]["pin_type"],
        json!({"kind": "is", "pin_type": frames})
    );
    assert_eq!(
        catalog_ports[1]["pin_type"],
        json!({"kind": "is", "pin_type": picture})
    );

    let node = rendered_node_of_type(MockNodeStatingTextureFormats::node_class_import_path());
    assert_eq!(node["ports"]["inputs"][0]["pin_type"], frames);
    assert_eq!(node["ports"]["outputs"][0]["pin_type"], picture);
}

#[test]
fn graph_serves_a_typed_ports_stream_facts_under_its_pin_type() {
    use crate::core::descriptors::{ImageStreamFacts, ImageStreamFrameRate};

    ensure_test_mocks_registered();
    let mut graph = Graph::new();
    let added = graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            MockPinTypedNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named");
    added
        .first()
        .expect("the mock is registered")
        .stream_facts_of_output_ports()
        .replace_facts_of(
            "frames",
            ImageStreamFacts {
                frame_rate: Some(ImageStreamFrameRate {
                    numerator: 60,
                    denominator: 1,
                }),
                ..ImageStreamFacts::default()
            }
            .into(),
        )
        .expect("an Image port carries Image facts");

    let rendered = serde_json::to_value(graph.to_graph_response("desk".to_string())).unwrap();
    assert_eq!(
        rendered["nodes"][0]["ports"]["outputs"][0]["stream_facts"],
        json!({
            "type": "texture",
            "frame_rate": {"numerator": 60, "denominator": 1},
        })
    );
}

#[test]
fn graph_names_encoded_stream_facts_by_the_ports_encoded_type_and_an_untyped_ports_by_none() {
    use crate::core::descriptors::{PacketsStreamFacts, PinFlow, PinType, ResolvedPin};

    static REGISTERED: std::sync::Once = std::sync::Once::new();
    let node_type = a_node_type_whose_one_output_declares_no_pin_type(
        &REGISTERED,
        "MockNodeWritingEncodedUnits",
        "encoded",
    );
    let mut graph = Graph::new();
    let added = graph
        .traversal_mut()
        .add_v(NodeSpec::new(node_type, serde_json::Value::Null))
        .expect("the node is named");
    let node = added.first().expect("the mock is registered");
    let node_id = node.id.clone();
    node.stream_facts_of_output_ports()
        .replace_facts_of(
            "encoded",
            PacketsStreamFacts {
                rfc6381_codec: Some("avc1.64001f".to_string()),
                ..PacketsStreamFacts::default()
            }
            .into(),
        )
        .expect("a Packets port carries Packets facts");
    let stream_facts_rendered = |graph: &Graph| {
        serde_json::to_value(graph.to_graph_response("desk".to_string())).unwrap()["nodes"][0]
            ["ports"]["outputs"][0]["stream_facts"]
            .clone()
    };

    assert_eq!(
        stream_facts_rendered(&graph),
        serde_json::Value::Null,
        "a port with no pin type carries no facts `graph` names"
    );

    graph
        .traversal_mut()
        .v(&node_id)
        .first_mut()
        .expect("the node is in the graph")
        .ports
        .outputs[0]
        .pin = Some(ResolvedPin {
        pin_type: PinType::EncodedVideo { codecs: Vec::new() },
        flow: PinFlow::Stream,
    });
    assert_eq!(
        stream_facts_rendered(&graph),
        json!({"type": "encoded_video", "rfc6381_codec": "avc1.64001f"})
    );
}
