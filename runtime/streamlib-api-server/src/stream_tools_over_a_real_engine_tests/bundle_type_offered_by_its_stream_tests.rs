// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The `bundle_type` of a placed `MakeBundle` or `BreakBundle`, over a real engine: `graph`
//! serves it as a choice listing exactly the bundles its stream's node types declare, each by
//! its name, and none while they declare none; a bundle it does not list is refused, and one it
//! lists is set.

use streamlib::sdk::descriptors::BundleMember;
use streamlib_media_builtins::{BreakBundle, MakeBundle};
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::input_value_set_tests::the_input_values_of;
use super::*;

const THERMOMETER_TYPE: &str = "nodes.weather:Thermometer";
const LOGBOOK_TYPE: &str = "nodes.weather:Logbook";
const SYNTH_TYPE: &str = "nodes.music:Synth";

/// A bundle the dataclass `name` declares, of a Float member for each of `members`.
fn a_bundle_named(name: &str, members: &[&str]) -> PinType {
    PinType::Bundle {
        name: Some(name.to_owned()),
        members: members
            .iter()
            .map(|member| BundleMember {
                name: (*member).to_owned(),
                member_type: PinType::Float { exact_size: None },
                meaning: None,
                default_value: None,
            })
            .collect(),
    }
}

fn a_reading() -> PinType {
    a_bundle_named("Reading", &["level", "humidity"])
}

fn a_tone() -> PinType {
    a_bundle_named("Tone", &["pitch"])
}

/// `stream_id` resolving, as its interpreter described them, a `Thermometer` giving a reading,
/// a `Logbook` taking a List of readings and a `Synth` taking a tone.
fn the_stream_describing_three_node_types_of_two_bundles(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
) {
    let loaded = engine
        .engine
        .loaded_stream_of_the_id(stream_id)
        .expect("the stream is loaded");
    let node_types = loaded.node_types_this_stream_resolves();
    for descriptor in [
        a_python_function_node_type(
            THERMOMETER_TYPE,
            Vec::new(),
            vec![a_typed_port("reading", a_reading())],
        ),
        a_python_function_node_type(
            LOGBOOK_TYPE,
            vec![a_typed_port(
                "readings",
                PinType::ListOf {
                    element_type: Box::new(a_reading()),
                },
            )],
            Vec::new(),
        ),
        a_python_function_node_type(SYNTH_TYPE, vec![a_typed_port("tone", a_tone())], Vec::new()),
    ] {
        node_types
            .register_a_described_type_that_runs_no_node_for_a_test(descriptor, None)
            .expect("the stream resolves the node type");
    }
}

/// A loaded stream `main` holding a `make` of `MakeBundle` and a `break` of `BreakBundle`; its
/// id, the project and the two nodes' UUIDs.
async fn a_stream_holding_a_make_and_a_break_bundle(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
) -> (String, ProjectWithGraphDocuments, String, String) {
    streamlib_media_builtins::register_media_builtin_node_types();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main", "nodes": [], "links": [],
    }));
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let added = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": project.stream_asset_of("main"),
                      "type": MakeBundle::node_class_import_path().as_str(), "name": "make" },
                    { "operation": "add_node", "stream": project.stream_asset_of("main"),
                      "type": BreakBundle::node_class_import_path().as_str(), "name": "break" },
                ],
            }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("the two nodes are placed: {refusal:?}"));
    let the_uuid_of = |index: usize| {
        added["added_nodes"][index]["node"]
            .as_str()
            .expect("an added node's UUID")
            .to_owned()
    };
    let (make, break_) = (the_uuid_of(0), the_uuid_of(1));
    (stream_id, project, make, break_)
}

/// The pin type `graph` serves on the `bundle_type` of the node `node_name` of `stream_id`.
async fn the_bundle_type_served_on(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
    node_name: &str,
) -> Value {
    let graph = engine
        .the_answer_or_the_refusal_of("graph", json!({ "stream": stream_id }))
        .await
        .unwrap_or_else(|refusal| panic!("graph answers: {refusal:?}"));
    let node = the_node_named(&graph, node_name);
    node["ports"]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|port| port["name"] == "bundle_type")
        .map(|port| port["pin_type"].clone())
        .unwrap_or_else(|| panic!("`{node_name}` has a `bundle_type`: {node}"))
}

/// Both nodes, placed before the stream's node types are described and after, offer exactly the
/// two bundles they declare, each once and by its name, a List's element among them.
///
/// Mental-revert: serving the setting as its node type declares it lists no bundle, and reading
/// the list when a node is placed leaves out the bundles described after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_placed_make_or_break_bundle_offers_exactly_the_bundles_its_stream_declares() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, _, _, _) = a_stream_holding_a_make_and_a_break_bundle(&engine).await;

    let none_on_offer = json!({
        "type": "pin_type_choice",
        "among_the_bundles_its_stream_declares": true,
    });
    for node in ["make", "break"] {
        assert_eq!(
            the_bundle_type_served_on(&engine, &stream_id, node).await,
            none_on_offer,
            "a stream whose node types declare no bundle offers none"
        );
    }

    the_stream_describing_three_node_types_of_two_bundles(&engine, &stream_id);

    let both_on_offer = json!({
        "type": "pin_type_choice",
        "among": [a_reading(), a_tone()],
        "among_the_bundles_its_stream_declares": true,
    });
    for node in ["make", "break"] {
        let served = the_bundle_type_served_on(&engine, &stream_id, node).await;
        assert_eq!(served, both_on_offer, "{node}");
        assert_eq!(served["among"][0]["name"], "Reading", "{served}");
    }
}

/// A bundle the stream's node types do not declare is refused naming the bundles on offer, and
/// nothing is saved; one on offer is set, and types the node's ports by it.
///
/// Mental-revert: a choice among any type takes the bundle no node type declares.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_its_stream_does_not_declare_is_refused_and_one_it_declares_is_set() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, project, make, _) = a_stream_holding_a_make_and_a_break_bundle(&engine).await;
    the_stream_describing_three_node_types_of_two_bundles(&engine, &stream_id);
    let stream_asset = project.stream_asset_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": make, "input": "bundle_type",
                    "value": a_bundle_named("Elsewhere", &["level"]),
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a bundle no node type of the stream declares is refused");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputValueNotValid,
        "{refusal:?}"
    );
    assert!(
        refusal.runtime_words.contains("`Reading`") && refusal.runtime_words.contains("`Tone`"),
        "the refusal names the bundles on offer: {refusal:?}"
    );
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), &make),
        json!({})
    );

    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": make, "input": "bundle_type",
                    "value": a_tone(), "author": an_agent_named("alice") }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("a bundle on offer is set: {refusal:?}"));
    let made = the_node_named(&engine.the_live_graph_of(&stream_id), "make");
    let bundle_output = made["ports"]["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|port| port["name"] == "bundle")
        .unwrap_or_else(|| panic!("`make` has a `bundle` output: {made}"));
    assert_eq!(bundle_output["pin_type"], json!(a_tone()), "{made}");
}

const MAKE_NODE_UUID: &str = "5a5a5a5a-5a5a-4a5a-8a5a-5a5a5a5a5a5a";

/// A Make Bundle saved before bundles were named holds its reading unnamed; setting a named
/// bundle over it, then undoing and redoing the set, each applies, and the undo puts the unnamed
/// reading back.
///
/// Mental-revert: matching a chosen bundle to an offered one whole refuses the undo, since the
/// unnamed reading the undo puts back is no bundle on offer.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_saved_unnamed_is_put_back_by_an_undo_of_a_set_over_it() {
    streamlib_media_builtins::register_media_builtin_node_types();
    let PinType::Bundle { members, .. } = a_reading() else {
        unreachable!("a reading is a bundle")
    };
    let an_unnamed_reading = json!(PinType::Bundle {
        name: None,
        members
    });
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": MAKE_NODE_UUID, "name": "make",
              "type": MakeBundle::node_class_import_path().as_str(),
              "input_values": { "bundle_type": an_unnamed_reading } },
        ],
        "links": [],
    }));
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    the_stream_describing_three_node_types_of_two_bundles(&engine, &stream_id);
    let alice =
        || json!({ "project_directory": project.path(), "author": an_agent_named("alice") });

    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "node": MAKE_NODE_UUID,
                    "input": "bundle_type", "value": a_tone(),
                    "author": an_agent_named("alice") }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("a bundle on offer is set: {refusal:?}"));
    engine
        .the_answer_or_the_refusal_of("undo", alice())
        .await
        .unwrap_or_else(|refusal| panic!("the undo puts the unnamed reading back: {refusal:?}"));
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MAKE_NODE_UUID),
        json!({ "bundle_type": an_unnamed_reading })
    );
    engine
        .the_answer_or_the_refusal_of("redo", alice())
        .await
        .unwrap_or_else(|refusal| panic!("the redo sets the tone again: {refusal:?}"));
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MAKE_NODE_UUID),
        json!({ "bundle_type": a_tone() })
    );
}
