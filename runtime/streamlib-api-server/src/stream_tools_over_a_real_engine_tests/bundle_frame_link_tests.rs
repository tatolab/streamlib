// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A bundle holding a Texture linked into an input that states no pin type, as a relay's send
//! node's `content` is, over a real engine: `connect` refuses it with a code, the link and the
//! Texture member named, and `check_link` answers that same refusal; a bundle of small members
//! links there as before.

use streamlib::sdk::descriptors::{
    BundleMember, NodeClassImportPath, NodeClassShortName, PinDeclaration, PinType, PortDescriptor,
    PortFamily, PortFamilyDeclaration,
};
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::*;

const FRAMED_SOURCE_TYPE: &str = "bundle_frame_link_test:FramedSource";
const READING_SOURCE_TYPE: &str = "bundle_frame_link_test:ReadingSource";
const SEND_TYPE: &str = "bundle_frame_link_test:Send";
const FRAMED_SOURCE_UUID: &str = "1a1a1a1a-1a1a-4a1a-8a1a-1a1a1a1a1a1a";
const READING_SOURCE_UUID: &str = "2b2b2b2b-2b2b-4b2b-8b2b-2b2b2b2b2b2b";
const SEND_UUID: &str = "3c3c3c3c-3c3c-4c3c-8c3c-3c3c3c3c3c3c";

fn a_member(name: &str, member_type: PinType) -> BundleMember {
    BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value: None,
    }
}

/// A node type giving `pin_type` on its one output, `port`.
fn a_source_of(node_type: &str, port: &str, pin_type: PinType) -> NodeDescriptor {
    let node_type = NodeClassImportPath::new(node_type).expect("a node type");
    let short_name = node_type.as_str().rsplit(':').next().expect("a short name");
    NodeDescriptor::new(
        NodeClassShortName::new(short_name).expect("a short name"),
        node_type,
        "a node type of one typed output",
    )
    .with_output(
        PortDescriptor::new(
            port,
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Records),
        )
        .with_delivery_profile("newest")
        .with_pin_declaration(PinDeclaration::exactly(pin_type)),
    )
}

/// Registers, once, a `FramedSource` giving a picture and its region on `framed`, a
/// `ReadingSource` giving a Float and a String on `reading`, and a `Send` whose `content` states
/// no pin type and takes Packets, Records, Channels or Array, as a relay's send node does.
fn the_bundle_frame_link_node_types_registered() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        let send = NodeClassImportPath::new(SEND_TYPE).expect("a node type");
        for descriptor in [
            a_source_of(
                FRAMED_SOURCE_TYPE,
                "framed",
                PinType::Bundle {
                    members: vec![
                        a_member(
                            "picture",
                            PinType::Texture {
                                pixel_formats: Vec::new(),
                            },
                        ),
                        a_member("region", PinType::Rectangle {}),
                    ],
                },
            ),
            a_source_of(
                READING_SOURCE_TYPE,
                "reading",
                PinType::Bundle {
                    members: vec![
                        a_member("level", PinType::Float { exact_size: None }),
                        a_member("label", PinType::String {}),
                    ],
                },
            ),
            NodeDescriptor::new(
                NodeClassShortName::new("Send").expect("a short name"),
                send,
                "a node type whose one input states no pin type",
            )
            .with_input(PortDescriptor::new(
                "content",
                "",
                true,
                PortFamilyDeclaration {
                    also_takes_families: vec![
                        PortFamily::Records,
                        PortFamily::Channels,
                        PortFamily::Array,
                    ],
                    ..PortFamilyDeclaration::of_family(PortFamily::Packets)
                },
            )),
        ] {
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("each bundle frame link node type is registered here alone");
        }
    });
}

fn a_project_whose_main_stream_holds_two_sources_and_a_send() -> ProjectWithGraphDocuments {
    the_bundle_frame_link_node_types_registered();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": FRAMED_SOURCE_UUID, "name": "frames", "type": FRAMED_SOURCE_TYPE, "config": {} },
            { "id": READING_SOURCE_UUID, "name": "readings", "type": READING_SOURCE_TYPE, "config": {} },
            { "id": SEND_UUID, "name": "send", "type": SEND_TYPE, "config": {} },
        ],
        "links": [],
    }))
}

/// Mental-revert: judged by its families alone, the bundle's Records family is one `content`
/// takes, so `connect` makes the link and `check_link` answers `connects`.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_holding_a_texture_into_an_untyped_input_is_refused_by_connect_and_check_link_alike()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_two_sources_and_a_send();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));
    let link = json!({
        "stream": stream,
        "source": { "node": FRAMED_SOURCE_UUID, "port": "framed" },
        "target": { "node": SEND_UUID, "port": "content" },
    });

    let checked = engine
        .the_answer_or_the_refusal_of("check_link", link.clone())
        .await
        .expect("check_link answers");
    let mut connect = link;
    connect["author"] = an_agent_named("alice");
    let refused = engine
        .the_answer_or_the_refusal_of("connect", connect)
        .await
        .expect_err("a Bundle frame does not enter an input that states no pin type");

    assert_eq!(
        refused.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refused:?}"
    );
    assert!(refused.refused_link.is_some(), "{refused:?}");
    assert!(
        refused
            .runtime_words
            .contains("member `picture` is a Texture"),
        "{refused:?}"
    );
    assert_eq!(checked["outcome"], "refused", "{checked}");
    assert_eq!(
        checked["refusal"],
        serde_json::to_value(&*refused).unwrap(),
        "check_link answers the refusal connect answers"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_of_small_members_into_an_untyped_input_connects_as_before() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_two_sources_and_a_send();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));

    engine
        .the_answer_or_the_refusal_of(
            "connect",
            json!({
                "stream": stream,
                "source": { "node": READING_SOURCE_UUID, "port": "reading" },
                "target": { "node": SEND_UUID, "port": "content" },
                "author": an_agent_named("alice"),
            }),
        )
        .await
        .expect("a bundle of small members travels as one Records message");
}
