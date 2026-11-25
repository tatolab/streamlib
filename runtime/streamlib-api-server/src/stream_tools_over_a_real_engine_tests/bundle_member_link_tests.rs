// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link from one member of a bundle output, or into one member of a bundle input, over a real
//! engine: `check_link_candidates` lists each member of a placed bundle port as an end and answers
//! what `check_link` answers for it, a member the bundle does not hold is refused naming the
//! members it has, and a saved link from a member loads from that member.

use streamlib::sdk::descriptors::{BundleMember, NodeClassImportPath, NodeClassShortName, PinDeclaration, PinType, PortDescriptor};
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::*;

const SENSOR_TYPE: &str = "bundle_member_link_test:Sensor";
const METER_TYPE: &str = "bundle_member_link_test:Meter";
const DIAL_TYPE: &str = "bundle_member_link_test:Dial";
const PANEL_TYPE: &str = "bundle_member_link_test:Panel";
const SENSOR_UUID: &str = "5e5e5e5e-5e5e-4e5e-8e5e-5e5e5e5e5e5e";
const METER_UUID: &str = "6d6d6d6d-6d6d-4d6d-8d6d-6d6d6d6d6d6d";
const DIAL_UUID: &str = "7c7c7c7c-7c7c-4c7c-8c7c-7c7c7c7c7c7c";
const PANEL_UUID: &str = "8b8b8b8b-8b8b-4b8b-8b8b-8b8b8b8b8b8b";

/// A reading: a Float `level` and a String `label`.
fn a_reading() -> PinType {
    let member = |name: &str, member_type| BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value: None,
    };
    PinType::Bundle {
        name: None,
        members: vec![
            member("level", PinType::Float { exact_size: None }),
            member("label", PinType::String {}),
        ],
    }
}

/// The node type `node_type` whose one port, `port`, is typed `pin_type`: an output, or, with
/// `is_an_input`, an input.
fn a_node_type_with_one_port(
    node_type: &str,
    port: &str,
    is_an_input: bool,
    pin_type: PinType,
) -> NodeDescriptor {
    let node_type = NodeClassImportPath::new(node_type).expect("a node type");
    let short_name = node_type.as_str().rsplit(':').next().expect("a short name");
    let port = PortDescriptor::new(port, "", true, PinDeclaration::exactly(pin_type))
    .with_delivery_profile("newest");
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).expect("a short name"),
        node_type,
        "a node type of one typed port",
    );
    if is_an_input {
        descriptor.with_input(port)
    } else {
        descriptor.with_output(port)
    }
}

/// Registers, once, a `Sensor` giving a reading on `reading`, a `Meter` taking a Float on
/// `level`, a `Dial` giving a Float on `level` and a `Panel` taking a reading on `reading`.
fn the_bundle_member_link_node_types_registered() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        let a_float = || PinType::Float { exact_size: None };
        for descriptor in [
            a_node_type_with_one_port(SENSOR_TYPE, "reading", false, a_reading()),
            a_node_type_with_one_port(METER_TYPE, "level", true, a_float()),
            a_node_type_with_one_port(DIAL_TYPE, "level", false, a_float()),
            a_node_type_with_one_port(PANEL_TYPE, "reading", true, a_reading()),
        ] {
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("each bundle member link node type is registered here alone");
        }
    });
}

/// A project whose stream `main` holds the nodes `nodes`, each its UUID, name and type, linked by
/// `links`.
fn a_project_whose_main_stream_holds(
    nodes: &[(&str, &str, &str)],
    links: Value,
) -> ProjectWithGraphDocuments {
    the_bundle_member_link_node_types_registered();
    let nodes: Vec<Value> = nodes
        .iter()
        .map(|(id, name, node_type)| json!({ "id": id, "name": name, "type": node_type }))
        .collect();
    ProjectWithGraphDocuments::holding(json!({ "stream": "main", "nodes": nodes, "links": links }))
}

/// A project whose stream `main` holds a `sensor` giving a reading on `reading` and a `meter`
/// taking a Float on `level`, linked by `links`.
fn a_project_whose_main_stream_holds_a_sensor_and_a_meter(
    links: Value,
) -> ProjectWithGraphDocuments {
    a_project_whose_main_stream_holds(
        &[
            (SENSOR_UUID, "sensor", SENSOR_TYPE),
            (METER_UUID, "meter", METER_TYPE),
        ],
        links,
    )
}

/// The link from member `member` of the sensor's reading into the meter's level, in `stream`.
fn the_link_from_the_member(stream: &Value, member: &str) -> Value {
    json!({
        "stream": stream,
        "source": { "node": SENSOR_UUID, "port": "reading", "member": member },
        "target": { "node": METER_UUID, "port": "level" },
    })
}

/// Mental-revert: listing only whole ports leaves no member among the candidates, and judging a
/// member's end by the bundle's type answers `refused` for `level`.
#[tokio::test(flavor = "multi_thread")]
async fn each_member_of_a_placed_bundle_output_is_a_candidate_checked_as_check_link_checks_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_sensor_and_a_meter(json!([]));
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));

    let answer = engine
        .the_answer_or_the_refusal_of(
            "check_link_candidates",
            json!({
                "stream": stream,
                "from": { "input": { "node": METER_UUID, "port": "level" } },
                "among": "placed_ports",
                "include_refused": true,
            }),
        )
        .await
        .expect("check_link_candidates answers");

    let candidates = answer["candidates"].as_array().expect("candidates");
    let ends: Vec<(&str, Option<&str>)> = candidates
        .iter()
        .filter(|candidate| candidate["end"]["node"] == SENSOR_UUID)
        .map(|candidate| {
            (
                candidate["end"]["port"].as_str().expect("port"),
                candidate["end"]["member"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        ends,
        [
            ("reading", None),
            ("reading", Some("level")),
            ("reading", Some("label")),
        ],
        "{answer}"
    );
    for (candidate, member) in candidates
        .iter()
        .filter(|candidate| candidate["end"]["member"].is_string())
        .zip(["level", "label"])
    {
        let checked = engine
            .the_answer_or_the_refusal_of("check_link", the_link_from_the_member(&stream, member))
            .await
            .expect("check_link answers");
        assert_eq!(candidate["check"], checked, "{member}");
    }
    assert_eq!(candidates[1]["check"]["outcome"], "connects", "{answer}");
    assert_eq!(candidates[2]["check"]["outcome"], "refused", "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_the_bundle_does_not_hold_is_refused_naming_the_members_it_has() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_sensor_and_a_meter(json!([]));
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));

    let mut connect = the_link_from_the_member(&stream, "depth");
    connect["author"] = an_agent_named("alice");
    let refusal = engine
        .the_answer_or_the_refusal_of("connect", connect)
        .await
        .expect_err("the reading holds no member `depth`");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::PortNotFound,
        "{refusal:?}"
    );
    for named in ["`depth`", "`level`", "`label`"] {
        assert!(
            refusal.runtime_words.contains(named),
            "{named}: {refusal:?}"
        );
    }
}

/// Mental-revert: a load that reads no `member` wires the whole reading into the Float input,
/// which its types refuse.
#[tokio::test(flavor = "multi_thread")]
async fn a_saved_link_from_a_member_loads_from_that_member() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_sensor_and_a_meter(json!([{
        "source": { "node": SENSOR_UUID, "port": "reading", "member": "level" },
        "target": { "node": METER_UUID, "port": "level" },
    }]));
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let live_graph = engine.the_live_graph_of(&stream_id);
    let links = live_graph["links"].as_array().expect("links");
    assert_eq!(links.len(), 1, "{live_graph}");
    assert_eq!(links[0]["source"]["member"], "level", "{live_graph}");
}

/// Mental-revert: listing the members of outputs alone leaves no member of the panel's reading
/// among the candidates, and judging a candidate input without its member answers `refused` for
/// the Float into the whole reading.
#[tokio::test(flavor = "multi_thread")]
async fn each_member_of_a_placed_bundle_input_is_a_candidate_checked_as_check_link_checks_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds(
        &[
            (DIAL_UUID, "dial", DIAL_TYPE),
            (PANEL_UUID, "panel", PANEL_TYPE),
        ],
        json!([]),
    );
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));

    let answer = engine
        .the_answer_or_the_refusal_of(
            "check_link_candidates",
            json!({
                "stream": stream,
                "from": { "output": { "node": DIAL_UUID, "port": "level" } },
                "among": "placed_ports",
                "include_refused": true,
            }),
        )
        .await
        .expect("check_link_candidates answers");

    let candidates: Vec<&Value> = answer["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .filter(|candidate| candidate["end"]["node"] == PANEL_UUID)
        .collect();
    let ends: Vec<(&str, Option<&str>)> = candidates
        .iter()
        .map(|candidate| {
            (
                candidate["end"]["port"].as_str().expect("port"),
                candidate["end"]["member"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        ends,
        [
            ("reading", None),
            ("reading", Some("level")),
            ("reading", Some("label")),
        ],
        "{answer}"
    );
    for candidate in &candidates[1..] {
        let checked = engine
            .the_answer_or_the_refusal_of(
                "check_link",
                json!({
                    "stream": stream,
                    "source": { "node": DIAL_UUID, "port": "level" },
                    "target": { "node": PANEL_UUID, "port": "reading", "member": candidate["end"]["member"] },
                }),
            )
            .await
            .expect("check_link answers");
        assert_eq!(candidate["check"], checked, "{candidate}");
    }
    assert_eq!(candidates[1]["check"]["outcome"], "connects", "{answer}");
    assert_eq!(candidates[2]["check"]["outcome"], "refused", "{answer}");
}
