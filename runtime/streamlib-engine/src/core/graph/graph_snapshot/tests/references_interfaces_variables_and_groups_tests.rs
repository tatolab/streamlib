// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Node references under config keys, links to other streams, wide integers, interface and variable nodes, labelled groups and config-field inputs.

use super::*;
use crate::core::descriptors::{PinDeclaration, PinType};

const GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS: &str = "graph_snapshot_tests.nodes:Gateway";

const TRACK_CLASS_FOR_THE_REFERENCE_TESTS: &str = "graph_snapshot_tests.nodes:Track";

const SEND_CLASS_FOR_THE_REFERENCE_TESTS: &str = "graph_snapshot_reference_tests:Send";

/// An input of `name` holding a reference to a node of `referenced_node_type`.
fn an_input_holding_a_node_reference(
    name: &str,
    referenced_node_type: &str,
    one_referrer_per_referenced_node: bool,
) -> crate::core::descriptors::PortDescriptor {
    use crate::core::descriptors::PortDescriptor;
    use crate::core::descriptors::{PinDeclaration, PinType};
    PortDescriptor::new(
        name,
        "",
        false,
        PinDeclaration::exactly(PinType::NodeReference {
            referenced_node_type: Some(NodeClassImportPath::new(referenced_node_type).unwrap()),
            one_referrer_per_referenced_node,
        }),
    )
}

/// A stream that described a gateway type, a track type whose input `gateway` takes a reference
/// to a gateway, and a send type whose input `track` takes a reference to a track no other send
/// names.
fn node_types_with_a_track_naming_its_gateway_by_reference() -> NodeTypesOneStreamResolves {
    let node_types = NodeTypesOneStreamResolves::new();
    let descriptor_of = |short_name: &str, import_path: &str| {
        crate::core::descriptors::NodeDescriptor::new(
            crate::core::descriptors::NodeClassShortName::new(short_name).unwrap(),
            NodeClassImportPath::new(import_path).unwrap(),
            "graph snapshot reference test",
        )
    };
    for descriptor in [
        descriptor_of("Gateway", GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS),
        descriptor_of("Track", TRACK_CLASS_FOR_THE_REFERENCE_TESTS)
            .with_input(an_input_holding_a_node_reference(
                "gateway",
                GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS,
                false,
            ))
            .with_config_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "track": {"type": "string"},
                    "gateway_or_its_name": {"anyOf": [
                        {streamlib_node_schema::NODE_REFERENCE_SCHEMA_KEYWORD: {}},
                        {"type": "string"}
                    ]}
                }
            })),
        descriptor_of("Send", SEND_CLASS_FOR_THE_REFERENCE_TESTS).with_input(
            an_input_holding_a_node_reference("track", TRACK_CLASS_FOR_THE_REFERENCE_TESTS, true),
        ),
    ] {
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                descriptor,
                crate::core::nodes::HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                    Box::new(|_node| Err(Error::NotSupported("never constructed".into()))),
                ),
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
                "input_values": {"gateway": gateway_config_value, "track": "camera"}
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
        reread.nodes[2].input_values["gateway"],
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
                 "input_values": {"gateway": {"node": "cloudflare"}}},
                {"name": "screen-track", "type": TRACK_CLASS_FOR_THE_REFERENCE_TESTS,
                 "input_values": {"gateway": {"node": "cloudflare"}}},
                {"name": "camera-send", "type": SEND_CLASS_FOR_THE_REFERENCE_TESTS,
                 "input_values": {"track": {"node": "camera-track"}}},
                {"name": "second-send", "type": SEND_CLASS_FOR_THE_REFERENCE_TESTS,
                 "input_values": {"track": {"node": second_send_track}}}
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
        refusal.contains("input `gateway` of node `camera-track` names node `nowhere`")
            && refusal.contains("cloudflare"),
        "{refusal}"
    );
}

#[test]
fn a_reference_to_a_node_of_the_wrong_type_is_refused_naming_both_types() {
    let refusal =
        a_graph_whose_track_names_in_its_gateway_key(serde_json::json!({"node": "other-track"}))
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
            "input_values": {"gateway_or_its_name": "cloudflare"}
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
                "input_values": {"variable": "gateway", "node_reference": {"node": "nowhere"}}
            }
        ]
    }))
    .unwrap();
    let refusal = graph
        .validate(&node_types_with_a_track_naming_its_gateway_by_reference())
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains(
            "input `node_reference` of `SetVariable` node `set-gateway` names node `nowhere`"
        ) && refusal.contains("cloudflare"),
        "{refusal}"
    );
}

#[test]
fn a_node_object_under_a_key_no_schema_marks_is_plain_config() {
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{
            "name": "cloudflare",
            "type": GATEWAY_CLASS_FOR_THE_REFERENCE_TESTS,
            "input_values": {"anything": {"node": "nowhere"}}
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
fn a_link_to_another_stream_as_an_older_runtime_rendered_it_is_refused_naming_the_track_receive() {
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

/// A link end is spelled `{node, port}`, with a `member` on a link from one member of a
/// bundle, and no other key: one carrying the
/// `runtime_name` a retired render wrote is refused, naming the keys a link
/// end is written with.
#[test]
fn a_link_end_carrying_a_key_it_is_not_written_with_is_refused_naming_the_keys_of_a_link_end() {
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
        "`node`, `port` and `member`",
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
        r#"{"nodes": [{"name": "sink", "type": "a:B",
                       "input_values": {"value": 18446744073709551616}}]}"#,
    )
    .expect_err("an integer past u64::MAX is refused");
    assert!(
        refusal
            .to_string()
            .contains("the integer 18446744073709551616 does not fit in 64 bits"),
        "{refusal}"
    );
    let refusal = GraphSnapshot::from_json_str(
        r#"{"nodes": [{"name": "sink", "type": "a:B",
                       "input_values": {"value": -9223372036854775809}}]}"#,
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
        r#"{"nodes": [{"name": "sink", "type": "a:B", "input_values": {
                "largest": 18446744073709551615,
                "smallest": -9223372036854775808,
                "float": 1.8446744073709552e19,
                "digits": "18446744073709551616",
                "escaped": "\"18446744073709551616\""
            }}]}"#,
    )
    .expect("each value fits");
    let config = &graph.nodes[0].input_values;
    assert_eq!(config["largest"], serde_json::json!(u64::MAX));
    assert_eq!(config["smallest"], serde_json::json!(i64::MIN));
    assert_eq!(config["digits"], "18446744073709551616");
}

const IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS: &str = "graph_snapshot_tests:ImageSource";

const PACKETS_SINK_CLASS_FOR_THE_INTERFACE_TESTS: &str = "graph_snapshot_tests:PacketsSink";

fn node_types_with_an_image_source_and_a_packets_sink() -> NodeTypesOneStreamResolves {
    use crate::core::descriptors::PortDescriptor;
    let node_types = NodeTypesOneStreamResolves::new();
    for (short_name, import_path, port) in [
        (
            "ImageSource",
            IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS,
            PortDescriptor::new(
                "video",
                "",
                false,
                PinDeclaration::exactly(PinType::Texture {
                    pixel_formats: Vec::new(),
                }),
            ),
        ),
        (
            "PacketsSink",
            PACKETS_SINK_CLASS_FOR_THE_INTERFACE_TESTS,
            PortDescriptor::new(
                "encoded",
                "",
                false,
                PinDeclaration::exactly(PinType::EncodedVideo { codecs: Vec::new() }),
            ),
        ),
    ] {
        let descriptor = crate::core::descriptors::NodeDescriptor::new(
            crate::core::descriptors::NodeClassShortName::new(short_name).unwrap(),
            NodeClassImportPath::new(import_path).unwrap(),
            "graph snapshot interface test",
        );
        let descriptor = if import_path == IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS {
            descriptor.with_output(port)
        } else {
            descriptor.with_input(port)
        };
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                descriptor,
                crate::core::nodes::HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                    Box::new(|_node| Err(Error::NotSupported("never constructed".into()))),
                ),
            )
            .expect("the type is described into the stream");
    }
    node_types
}

fn a_graph_offering_the_source_and_feeding_the_sink(
    output_pin_type: &str,
    input_pin_type: &str,
) -> GraphSnapshot {
    GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [
            {"name": "source", "type": IMAGE_SOURCE_CLASS_FOR_THE_INTERFACE_TESTS},
            {"name": "video", "type": "tatolab.stream:GraphOutput",
             "input_values": {"port": "video", "pin_type": {"type": output_pin_type}}},
            {"name": "encoded", "type": "tatolab.stream:GraphInput",
             "input_values": {"port": "encoded", "pin_type": {"type": input_pin_type}}},
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
fn interface_ports_of_the_pin_type_their_linked_ports_carry_validate() {
    a_graph_offering_the_source_and_feeding_the_sink("texture", "encoded_video")
        .validate(&node_types_with_an_image_source_and_a_packets_sink())
        .expect("a Texture Output, and an EncodedVideo Input into an EncodedVideo sink, validate");
}

#[test]
fn an_interface_port_of_another_pin_type_than_its_linked_port_is_refused_naming_both() {
    let node_types = node_types_with_an_image_source_and_a_packets_sink();
    let output_refusal =
        a_graph_offering_the_source_and_feeding_the_sink("tensor", "encoded_video")
            .validate(&node_types)
            .unwrap_err()
            .to_string();
    assert!(
        output_refusal.contains("`GraphOutput` node `video`")
            && output_refusal.contains("Tensor")
            && output_refusal.contains("Texture"),
        "{output_refusal}"
    );
    let input_refusal = a_graph_offering_the_source_and_feeding_the_sink("texture", "string")
        .validate(&node_types)
        .unwrap_err()
        .to_string();
    assert!(
        input_refusal.contains("`GraphInput` node `encoded`")
            && input_refusal.contains("String")
            && input_refusal.contains("Encoded video"),
        "{input_refusal}"
    );
}

#[test]
fn an_interface_node_linked_to_a_port_its_node_does_not_have_is_refused_naming_the_ports() {
    let mut graph = serde_json::to_value(a_graph_offering_the_source_and_feeding_the_sink(
        "texture",
        "encoded_video",
    ))
    .unwrap();
    graph["links"][0]["source"]["port"] = serde_json::json!("vidoe");
    let refusal = GraphSnapshot::from_graph_document(graph)
        .unwrap()
        .validate(&node_types_with_an_image_source_and_a_packets_sink())
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("node `source` has no output port `vidoe`") && refusal.contains("video"),
        "{refusal}"
    );
}

#[test]
fn a_variable_set_from_a_graph_input_validates() {
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [
            {"name": "encoded", "type": "tatolab.stream:GraphInput",
             "input_values": {"port": "encoded", "pin_type": {"type": "encoded_video"}}},
            {"name": "set-encoded", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "encoded"}},
            {"name": "get-encoded", "type": "tatolab.stream:GetVariable",
             "input_values": {"variable": "encoded"}},
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
             "input_values": {"port": "video", "pin_type": {"type": "texture"}}},
            {"name": "encoded", "type": "tatolab.stream:GraphInput",
             "input_values": {"port": "encoded", "pin_type": {"type": "encoded_video"}}},
            {"name": "set-unread", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "unread"}},
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
             "type": A_CAMERA_CLASS, "studio": {"group": A_GROUP_UUID}},
            {"id": "7f4e2d9a-1c3b-4a5e-8d6f-0e9c8b7a6d54", "name": "look",
             "type": "generators.look:look",
             "fragment": {
                 "nodes": [{"id": "9a8b7c6d-5e4f-4a3b-9c2d-1e0f9a8b7c65", "name": "blur",
                            "type": A_CAMERA_CLASS,
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

const GLOW_CLASS_FOR_THE_EXPOSED_CONFIG_FIELD_TESTS: &str =
    "graph_snapshot_exposed_config_field_tests.nodes:Glow";

/// A stream that described a Python node type `Glow`, reading `video_in` and configured
/// by a number `strength`.
fn node_types_with_a_glow_configured_by_a_strength() -> NodeTypesOneStreamResolves {
    let node_types = NodeTypesOneStreamResolves::new();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            crate::core::compiler::compiler_ops::function_node_island::tests::a_function_node_type(
                GLOW_CLASS_FOR_THE_EXPOSED_CONFIG_FIELD_TESTS,
                &["video_in"],
                &["video_out"],
            )
            .with_config_schema(serde_json::json!({
                "type": "object",
                "properties": {"strength": {"type": "number", "default": 0.5}},
            })),
            crate::core::nodes::HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                Box::new(|_node| Err(Error::NotSupported("never constructed".into()))),
            ),
        )
        .expect("the type is described into the stream");
    node_types
}

fn a_graph_whose_graph_input_drives_the_glows_strength(
    exposed_config_fields: &[&str],
) -> GraphSnapshot {
    GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [
            {"id": Uuid::new_v4(), "name": "glow",
             "type": GLOW_CLASS_FOR_THE_EXPOSED_CONFIG_FIELD_TESTS,
             "exposed_config_fields": exposed_config_fields},
            {"id": Uuid::new_v4(), "name": "glow_strength", "type": "tatolab.stream:GraphInput",
             "input_values": {"port": "glow_strength", "pin_type": {"type": "float"}, "range": [0, 1]}},
        ],
        "links": [
            {"source": {"node": "glow_strength", "port": "value"},
             "target": {"node": "glow", "port": "strength"}},
        ]
    }))
    .unwrap()
}

/// A setting a node's type's config schema lists takes no wire, whether or not the node was
/// saved exposing it.
///
/// Mental-revert: reading the saved exposure as an input port validates the link.
#[test]
fn a_link_into_a_config_field_is_refused_naming_its_inputs_though_the_node_exposes_it() {
    let refusal = a_graph_whose_graph_input_drives_the_glows_strength(&["strength"])
        .validate(&node_types_with_a_glow_configured_by_a_strength())
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("node `glow` has no input port `strength`")
            && refusal.contains("video_in"),
        "{refusal}"
    );
}

/// A node saved exposing config fields before its type was typed opens exposing none: on a
/// type that takes no config, each was an input of its own or merged into one, and a link into
/// one that is an input ends at that input.
///
/// Mental-revert: keeping the exposures refuses the whole document, as a saved Solid Color
/// exposing `alpha` was on the rig.
#[test]
fn a_node_of_a_type_taking_no_config_opens_exposing_none_of_the_fields_it_was_saved_exposing() {
    crate::core::test_support::ensure_test_mocks_registered();
    let typed = crate::core::test_support::MockPinTypedNode::node_class_import_path();
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [
            {"id": Uuid::new_v4(), "name": "mixer", "type": typed.as_str(),
             "exposed_config_fields": ["gain", "alpha"]},
            {"id": Uuid::new_v4(), "name": "level", "type": "tatolab.stream:GraphInput",
             "input_values": {"port": "level", "pin_type": {"type": "float"}, "range": [0, 1]}},
        ],
        "links": [
            {"source": {"node": "level", "port": "value"},
             "target": {"node": "mixer", "port": "gain"}},
        ]
    }))
    .unwrap();
    let node_types = NodeTypesOneStreamResolves::new();

    graph
        .validate(&node_types)
        .expect("the saved exposures no longer refuse the document");
    let resolved = graph.with_graph_only_nodes_resolved(&node_types).unwrap();
    let mixer = resolved
        .graph_the_runtime_runs
        .nodes
        .iter()
        .find(|node| node.name == "mixer")
        .unwrap();
    assert!(mixer.exposed_config_fields.is_empty());
    assert_eq!(resolved.graph_the_runtime_runs.links.len(), 1);
}
