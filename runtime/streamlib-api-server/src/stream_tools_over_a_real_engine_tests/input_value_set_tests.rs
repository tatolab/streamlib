// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `set_node_input_value` over a real engine: one operation sets a value on a typed input and a
//! setting a node type holds in its config, saved or live, alone or inside `transact`; a value
//! the input does not take is refused with its code and its place before anything is applied;
//! and the three operations it replaces record what it records.

use super::*;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, ProjectActionRefusalOnTheWire,
};

/// The one setting [`InputValueTestNode`] holds in its config.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    streamlib::sdk::schemars::JsonSchema,
)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub(super) struct InputValueTestNodeConfig {
    #[serde(default)]
    pub frame_width: Option<u32>,
}

/// A native node taking a config, a typed `gain` input, an Image `video` input that takes only
/// a link, and a typed `level` output a `gain` can be linked from.
#[streamlib::sdk::node(
    execution = manual,
    config = crate::stream_tools_over_a_real_engine_tests::input_value_set_tests::InputValueTestNodeConfig,
    input("gain", delivery_profile = "newest", pin_type = Float),
    input("video", delivery_profile = "newest", family = Image),
    output("level", pin_type = Float),
)]
pub(super) struct InputValueTestNode;

impl ManualNode for InputValueTestNode::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// Register [`InputValueTestNode`] with the node registry, once per test binary.
pub(super) fn register_the_input_value_test_node_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<InputValueTestNode::Node>());
}

pub(super) const MIXER_NODE_UUID: &str = "6b6b6b6b-6b6b-4b6b-8b6b-6b6b6b6b6b6b";

/// A project whose stream `main` holds an [`InputValueTestNode`] `mixer`, by
/// [`MIXER_NODE_UUID`], and a [`StreamToolsTestSink`] `monitor`.
pub(super) fn a_project_whose_main_stream_holds_a_mixer() -> ProjectWithGraphDocuments {
    register_the_input_value_test_node_once();
    register_the_stream_tools_test_sink_once();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": MIXER_NODE_UUID, "name": "mixer",
              "type": InputValueTestNode::node_class_import_path().as_str(), "config": {} },
            { "id": SOURCE_NODE_UUID, "name": "monitor",
              "type": StreamToolsTestSink::node_class_import_path().as_str(), "config": {} },
        ],
        "links": [],
    }))
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// The answer of the tool `tool_name` sent `arguments`, or its refusal as the wire carries it.
    pub(super) async fn the_answer_or_the_refusal_of(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, Box<ProjectActionRefusalOnTheWire>> {
        let tool_result = tool_call_result(
            self.operations_on_the_loaded_streams(),
            tool_name,
            arguments,
        )
        .await;
        if tool_result["isError"] == true {
            return Err(Box::new(crate::mcp::tests::the_refusal_of_a_tool_result(
                &tool_result,
            )));
        }
        Ok(crate::mcp::tests::first_text_block_json(&tool_result))
    }
}

/// The input values `node` holds in `graph_document`; none when it holds none.
pub(super) fn the_input_values_of(graph_document: &Value, node: &str) -> Value {
    graph_document["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|held| held["id"] == node)
        .map(|held| match &held["input_values"] {
            Value::Null => json!({}),
            input_values => input_values.clone(),
        })
        .unwrap_or_else(|| panic!("the document holds no node {node}: {graph_document}"))
}

/// The input `input` of `rendered_node`, a node of a `graph` render.
pub(super) fn the_input_named(rendered_node: &Value, input: &str) -> Value {
    rendered_node["ports"]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|port| port["name"] == input)
        .cloned()
        .unwrap_or_else(|| panic!("the node renders no input `{input}`: {rendered_node}"))
}

/// The pin values `rendered_node`, a node of a `graph` render, holds, which it always renders.
pub(super) fn the_pin_values_of(rendered_node: &Value) -> Value {
    rendered_node
        .get("pins")
        .filter(|pins| pins.is_object())
        .cloned()
        .unwrap_or_else(|| panic!("the node renders no `pins`: {rendered_node}"))
}

/// Each operation `answer`, a graph edit's answer, reports it applied, without the stream it
/// names, as the project's history records it.
fn the_operations_answered(answer: &Value) -> Vec<Value> {
    answer["operations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|answered| {
            let mut answered = answered.clone();
            answered
                .as_object_mut()
                .expect("an operation is an object")
                .remove("stream");
            answered
        })
        .collect()
}

/// A commit of a typed input's value and of a setting the node type holds in its config is
/// saved, applied to the running node, recorded as one `SetNodeInputValue` each, and undone to
/// what each replaced.
#[tokio::test(flavor = "multi_thread")]
async fn a_committed_input_value_is_saved_applied_recorded_and_undone() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");

    let applied = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                    "value": 0.5, "author": an_agent_named("alice") }),
        )
        .await
        .expect("the value is set");
    assert_eq!(applied["mode"], "commit", "{applied}");
    assert!(applied["transaction"].is_string(), "{applied}");
    assert!(
        applied.get("overridden_by_links").is_none(),
        "no link feeds `gain`: {applied}"
    );
    assert_eq!(
        the_operations_answered(&applied),
        [
            json!({ "operation": "set_node_input_value", "node": MIXER_NODE_UUID, "input": "gain",
                "from": null, "to": 0.5 })
        ]
    );

    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "frame_width",
                    "value": 640, "author": an_agent_named("alice") }),
        )
        .await
        .expect("the setting is set");
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MIXER_NODE_UUID),
        json!({ "gain": 0.5, "frame_width": 640 })
    );
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    assert_eq!(mixer["config"], json!({ "frame_width": 640 }), "{mixer}");
    assert_eq!(the_pin_values_of(&mixer), json!({ "gain": 0.5 }), "{mixer}");

    for _ in 0..2 {
        engine
            .the_answer_or_the_refusal_of(
                "undo",
                json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
            )
            .await
            .expect("the undo applies");
    }
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MIXER_NODE_UUID),
        json!({})
    );
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    assert_eq!(mixer["config"], json!({}), "{mixer}");
}

/// A live set on a loaded stream that is not started reaches its node and leaves the document
/// and the history as they were; a live set with no value takes the live value off.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_input_value_on_a_stopped_stream_reaches_its_node_and_is_not_saved() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let saved_before = project.the_graph_document_of("main");

    let applied = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                    "value": 0.25, "mode": "live" }),
        )
        .await
        .expect("the live value is set");
    assert_eq!(applied["mode"], "live", "{applied}");
    assert!(applied.get("transaction").is_none(), "{applied}");
    assert_eq!(project.the_graph_document_of("main"), saved_before);
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    let gain = the_input_named(&mixer, "gain");
    assert_eq!(
        (&gain["value"], &gain["value_comes_from"]),
        (&json!(0.25), &json!("live")),
        "{gain}"
    );
    assert_eq!(
        the_pin_values_of(&mixer),
        json!({}),
        "pins stay saved: {mixer}"
    );

    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "frame_width",
                    "value": 320, "mode": "live" }),
        )
        .await
        .expect("the live setting is set");
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    assert_eq!(mixer["config"], json!({ "frame_width": 320 }), "{mixer}");

    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                    "mode": "live" }),
        )
        .await
        .expect("the live value is taken off");
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    assert_ne!(
        the_input_named(&mixer, "gain")["value_comes_from"],
        "live",
        "{mixer}"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// Each value the input does not take is refused with its code, naming the node, the input and
/// the place of the value in the params.
#[tokio::test(flavor = "multi_thread")]
async fn a_value_the_input_does_not_take_is_refused_with_its_code_and_place() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let saved_before = project.the_graph_document_of("main");

    for (input, value, refusal_code) in [
        (
            "loudness",
            json!(1),
            ControlOperationRefusalCode::PortNotFound,
        ),
        (
            "video",
            json!(1),
            ControlOperationRefusalCode::InputTakesNoValue,
        ),
        (
            "gain",
            json!("loud"),
            ControlOperationRefusalCode::InputValueNotValid,
        ),
        (
            "frame_width",
            json!("wide"),
            ControlOperationRefusalCode::InputValueNotValid,
        ),
    ] {
        let refusal = engine
            .the_answer_or_the_refusal_of(
                "set_node_input_value",
                json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": input,
                        "value": value, "author": an_agent_named("alice") }),
            )
            .await
            .expect_err("the value is refused");
        assert_eq!(refusal.refusal_code, refusal_code, "{input}: {refusal:?}");
        assert_eq!(
            refusal.refused_node.as_deref(),
            Some(MIXER_NODE_UUID),
            "{refusal:?}"
        );
        assert_eq!(refusal.refused_field.as_deref(), Some(input), "{refusal:?}");
        assert_eq!(
            refusal.refused_params_pointer.as_deref(),
            Some("/value"),
            "{refusal:?}"
        );
        assert!(
            refusal.runtime_words.contains("mixer") && refusal.runtime_words.contains(input),
            "{refusal:?}"
        );
    }
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// Inside `transact`, every value is checked before the first operation is applied, so a
/// value refused after an operation that is refused only as it is applied is the one named:
/// a value of `set_node_input_value`, of `set_node_config`, and one set on a node an earlier
/// `add_node` names by its `as`, and one on a node inside a placed stream. Nothing is saved or
/// set.
#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_of_input_values_is_checked_whole_before_any_is_applied() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let set = |node: &str, input: &str, value: Value| {
        json!({ "operation": "set_node_input_value", "stream": stream_asset,
                "node": node, "input": input, "value": value })
    };
    let renaming_a_node_the_stream_does_not_hold = json!({ "operation": "rename_node",
        "stream": stream_asset, "node": "00000000-0000-4000-8000-000000000000", "name": "nobody" });
    let saved_before = project.the_graph_document_of("main");

    for (operations, refused_index, refused_params_pointer) in [
        (
            json!([
                renaming_a_node_the_stream_does_not_hold,
                set(MIXER_NODE_UUID, "frame_width", json!(800)),
                set(MIXER_NODE_UUID, "gain", json!(true)),
            ]),
            2,
            "/operations/2/value",
        ),
        (
            json!([
                renaming_a_node_the_stream_does_not_hold,
                { "operation": "set_node_config", "stream": stream_asset,
                  "node": MIXER_NODE_UUID, "config": { "frame_width": "wide" } },
            ]),
            1,
            "/operations/1/config/frame_width",
        ),
        (
            json!([
                renaming_a_node_the_stream_does_not_hold,
                { "operation": "add_node", "stream": stream_asset, "name": "second",
                  "type": InputValueTestNode::node_class_import_path().as_str(), "as": "second" },
                set("second", "gain", json!("loud")),
            ]),
            2,
            "/operations/2/value",
        ),
    ] {
        let refusal = engine
            .the_answer_or_the_refusal_of(
                "transact",
                json!({ "operations": operations, "author": an_agent_named("alice") }),
            )
            .await
            .expect_err("a value is refused");
        assert_eq!(
            refusal.refusal_code,
            ControlOperationRefusalCode::InputValueNotValid,
            "{operations}: {refusal:?}"
        );
        assert_eq!(
            refusal.refused_operation_index,
            Some(refused_index),
            "{operations}: {refusal:?}"
        );
        assert_eq!(
            refusal.refused_params_pointer.as_deref(),
            Some(refused_params_pointer),
            "{operations}: {refusal:?}"
        );
        assert_eq!(project.the_graph_document_of("main"), saved_before);
        let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
        assert_eq!(mixer["config"], json!({}), "{mixer}");
        assert_eq!(the_pin_values_of(&mixer), json!({}), "{mixer}");
    }

    let applied = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [set(MIXER_NODE_UUID, "frame_width", json!(800)),
                                   set(MIXER_NODE_UUID, "gain", json!(0.75))],
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("both values are set");
    assert_eq!(applied["operations"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MIXER_NODE_UUID),
        json!({ "frame_width": 800, "gain": 0.75 })
    );

    let collapsed = engine
        .the_answer_or_the_refusal_of(
            "collapse_selection_into_subgraph",
            json!({ "stream": stream_asset, "nodes": [MIXER_NODE_UUID], "name": "sub",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the mixer collapses into `sub`");
    let subgraph = collapsed["added_nodes"][0]["node"]
        .as_str()
        .expect("the subgraph node")
        .to_owned();
    let mixer_inside = the_node_named(&project.the_graph_document_of("sub"), "mixer");
    let mixer_drawn =
        the_uuid_a_held_node_takes_under(&subgraph, mixer_inside["id"].as_str().unwrap());
    let main_before = project.the_graph_document_of("main");
    let sub_before = project.the_graph_document_of("sub");
    let refusal = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [
                renaming_a_node_the_stream_does_not_hold,
                { "operation": "set_node_input_value", "stream": stream_asset,
                  "level": [subgraph], "node": mixer_drawn, "input": "gain",
                  "value": "loud" },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a value inside a placed stream is refused before any operation applies");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputValueNotValid,
        "{refusal:?}"
    );
    assert_eq!(refusal.refused_operation_index, Some(1), "{refusal:?}");
    assert_eq!(project.the_graph_document_of("main"), main_before);
    assert_eq!(project.the_graph_document_of("sub"), sub_before);
}

/// A value set on an input a link feeds is accepted and kept, and the answer names the link
/// that overrides it; the set sent again under its key answers so even once the link is gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_value_set_on_a_linked_input_is_kept_and_named_overridden_by_its_link() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_asset, "name": "feeder",
                  "type": InputValueTestNode::node_class_import_path().as_str(), "as": "feeder" },
                { "operation": "connect", "stream": stream_asset,
                  "source": { "node": "feeder", "port": "level" },
                  "target": { "node": MIXER_NODE_UUID, "port": "gain" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("the feeder is linked to `gain`");
    let feeder = the_node_named(&engine.the_live_graph_of(&stream_id), "feeder");

    let applied = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                    "value": 0.5, "author": an_agent_named("alice"),
                    "idempotency_key": "set-gain-under-its-link" }),
        )
        .await
        .expect("a value under a link is accepted");
    let overridden = json!([{ "stream": stream_id, "node": MIXER_NODE_UUID, "input": "gain",
                              "from_node": feeder["id"], "from_output": "level" }]);
    assert_eq!(applied["overridden_by_links"], overridden, "{applied}");
    engine
        .the_answer_or_the_refusal_of(
            "disconnect",
            json!({ "stream": stream_asset,
                    "source": { "node": feeder["id"], "port": "level" },
                    "target": { "node": MIXER_NODE_UUID, "port": "gain" },
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the link is taken off");
    let answered_again = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                    "value": 0.5, "author": an_agent_named("alice"),
                    "idempotency_key": "set-gain-under-its-link" }),
        )
        .await
        .expect("the set sent again answers as it first did");
    assert_eq!(answered_again["transaction"], applied["transaction"]);
    assert_eq!(
        answered_again["overridden_by_links"], overridden,
        "{answered_again}"
    );
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MIXER_NODE_UUID),
        json!({ "gain": 0.5 })
    );
}

/// `set_node_config` records one `SetNodeInputValue` for each setting it changes and for each
/// it takes off; `set_pin_value` records one; `set_exposed_config_fields` on a node type that
/// takes no config checks the names and records nothing.
#[tokio::test(flavor = "multi_thread")]
async fn the_operations_it_replaces_record_what_it_records() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");

    let answered = engine
        .the_answer_or_the_refusal_of(
            "set_node_config",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID,
                    "config": { "frame_width": 640 }, "author": an_agent_named("alice") }),
        )
        .await
        .expect("the config is set");
    assert_eq!(
        the_operations_answered(&answered),
        [
            json!({ "operation": "set_node_input_value", "node": MIXER_NODE_UUID,
                 "input": "frame_width", "from": null, "to": 640 })
        ]
    );
    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "frame_width",
                    "value": 800, "mode": "live" }),
        )
        .await
        .expect("the running node is set apart from its document");
    let answered = engine
        .the_answer_or_the_refusal_of(
            "set_node_config",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID,
                    "config": { "frame_width": 640 }, "author": an_agent_named("alice") }),
        )
        .await
        .expect("the setting the document already holds is set again");
    assert_eq!(
        the_operations_answered(&answered),
        [
            json!({ "operation": "set_node_input_value", "node": MIXER_NODE_UUID,
                 "input": "frame_width", "from": 640, "to": 640 })
        ]
    );
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    assert_eq!(mixer["config"], json!({ "frame_width": 640 }), "{mixer}");
    let answered = engine
        .the_answer_or_the_refusal_of(
            "set_node_config",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "config": {},
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the config is emptied");
    assert_eq!(
        the_operations_answered(&answered),
        [
            json!({ "operation": "set_node_input_value", "node": MIXER_NODE_UUID,
                 "input": "frame_width", "from": 640, "to": null })
        ]
    );
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    assert_eq!(mixer["config"], json!({}), "{mixer}");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_config",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID,
                    "config": { "frame_width": "wide" }, "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a setting its type refuses is refused");
    assert_eq!(
        refusal.refused_params_pointer.as_deref(),
        Some("/config/frame_width"),
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_field.as_deref(),
        Some("frame_width"),
        "{refusal:?}"
    );

    let answered = engine
        .the_answer_or_the_refusal_of(
            "set_pin_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                    "value": 0.5, "author": an_agent_named("alice") }),
        )
        .await
        .expect("the pin value is set");
    assert_eq!(
        the_operations_answered(&answered)
            .iter()
            .map(|answered| answered["operation"].clone())
            .collect::<Vec<_>>(),
        [json!("set_node_input_value")]
    );

    let unrecorded = engine
        .the_answer_or_the_refusal_of(
            "set_exposed_config_fields",
            json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID,
                    "exposed_config_fields": ["video"], "author": an_agent_named("alice") }),
        )
        .await
        .expect("an exposure of an input on a node type taking no config is accepted");
    assert!(unrecorded.get("transaction").is_none(), "{unrecorded}");
    assert_eq!(unrecorded["operations"], json!([]), "{unrecorded}");
    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_exposed_config_fields",
            json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID,
                    "exposed_config_fields": ["brightness"], "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a name that is none of its inputs is refused");
    assert!(
        refusal.runtime_words.contains("`brightness`"),
        "{refusal:?}"
    );
}

/// What a node holds live alone is kept by a committed transaction refused part-way, whose
/// sets are reversed to the saved value with the live one set live again, and is taken off by a
/// `set_node_config` that replaces the config whole.
///
/// Mental-revert: reversing a set to the live value it replaced saves that value as a pin the
/// document does not hold, and `graph` renders it `saved`.
#[tokio::test(flavor = "multi_thread")]
async fn a_value_held_live_alone_is_kept_by_a_refused_transaction_and_replaced_by_a_whole_config() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    for (input, value) in [("gain", json!(0.25)), ("frame_width", json!(800))] {
        engine
            .the_answer_or_the_refusal_of(
                "set_node_input_value",
                json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": input,
                        "value": value, "mode": "live" }),
            )
            .await
            .expect("the live value is set");
    }
    let saved_before = project.the_graph_document_of("main");

    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [
                { "operation": "set_node_input_value", "stream": stream_asset,
                  "node": MIXER_NODE_UUID, "input": "gain", "value": 0.5 },
                { "operation": "set_node_input_value", "stream": stream_asset,
                  "node": MIXER_NODE_UUID, "input": "frame_width", "value": 1024 },
                { "operation": "rename_node", "stream": stream_asset,
                  "node": "00000000-0000-4000-8000-000000000000", "name": "nobody" },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("the rename of a node the stream does not hold is refused");
    assert_eq!(project.the_graph_document_of("main"), saved_before);
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    let gain = the_input_named(&mixer, "gain");
    assert_eq!(
        (&gain["value"], &gain["value_comes_from"]),
        (&json!(0.25), &json!("live")),
        "the reversal sets the live value live again: {gain}"
    );
    assert_eq!(
        the_pin_values_of(&mixer),
        json!({}),
        "the pins are what the document holds: {mixer}"
    );
    assert_eq!(mixer["config"], json!({ "frame_width": 800 }), "{mixer}");

    engine
        .the_answer_or_the_refusal_of(
            "set_node_config",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "config": {},
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the config is replaced whole");
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    assert_eq!(mixer["config"], json!({}), "{mixer}");
}
