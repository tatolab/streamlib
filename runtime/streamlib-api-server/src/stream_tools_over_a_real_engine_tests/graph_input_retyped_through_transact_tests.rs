// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `GraphInput` whose type a `transact` changes: a cleared value stays cleared, so a change of
//! type that clears it first takes; a type a client does not write is refused for that reason;
//! and a type the port it is linked into does not take is refused naming that port.

use super::*;

use super::input_value_set_tests::{InputValueTestNode, register_the_input_value_test_node_once};

const WARMTH_BASE_NODE_UUID: &str = "7c7c7c7c-7c7c-4c7c-8c7c-7c7c7c7c7c7c";
const WARMTH_NODE_UUID: &str = "8d8d8d8d-8d8d-4d8d-8d8d-8d8d8d8d8d8d";

/// A project whose stream `main` holds a Float `GraphInput` `warmth_base`, saving 0.1 within
/// `[0, 1]`, by [`WARMTH_BASE_NODE_UUID`]; linked into the Float input `gain` of the
/// [`InputValueTestNode`] `warmth` when `linked`.
fn a_project_whose_main_stream_holds_a_float_input(linked: bool) -> ProjectWithGraphDocuments {
    register_the_input_value_test_node_once();
    let links = if linked {
        json!([{ "source": { "node": "warmth_base", "port": "value" },
                 "target": { "node": "warmth", "port": "gain" } }])
    } else {
        json!([])
    };
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": WARMTH_BASE_NODE_UUID, "name": "warmth_base",
              "type": "tatolab.stream:GraphInput",
              "config": { "port": "warmth_base", "pin_type": { "type": "float" },
                          "value": 0.1, "range": [0, 1] } },
            { "id": WARMTH_NODE_UUID, "name": "warmth",
              "type": InputValueTestNode::node_class_import_path().as_str(), "config": {} },
        ],
        "links": links,
    }))
}

/// The `set_node_input_value` operation setting `input` of `warmth_base` in `stream` to `value`.
fn set_on_warmth_base(stream: Uuid, input: &str, value: Value) -> Value {
    json!({ "operation": "set_node_input_value", "stream": stream,
            "node": WARMTH_BASE_NODE_UUID, "input": input, "value": value })
}

/// A Float `GraphInput` renders its saved value under `config.default`, the key a published Tato
/// Studio reads, and once cleared renders no value, under `value` or under `default`, so the clear
/// holds; and one `transact` clearing its value and range and
/// choosing a String takes.
///
/// Mental-revert: keeping the `default` the config itself holds draws the cleared value back from
/// the rendering, and the String is refused for holding 0.1.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleared_graph_input_value_stays_cleared_and_a_transact_clearing_it_retypes_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_float_input(false);
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");
    let transact = |operations: Value| {
        tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
    };

    let set = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert_eq!(
        set["config"]["default"],
        json!(0.1),
        "a published Studio reads it: {set}"
    );

    tool_answer(&transact(json!([set_on_warmth_base(stream, "value", Value::Null)])).await)
        .expect("the value is cleared");
    let cleared = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert!(cleared["input_values"].get("value").is_none(), "{cleared}");
    assert!(cleared["config"].get("default").is_none(), "{cleared}");
    assert!(cleared["config"].get("value").is_none(), "{cleared}");

    let string = json!({ "type": "string" });
    tool_answer(
        &transact(json!([
            set_on_warmth_base(stream, "value", Value::Null),
            set_on_warmth_base(stream, "range", Value::Null),
            set_on_warmth_base(stream, "pin_type", string.clone()),
        ]))
        .await,
    )
    .expect("a String with no value and no range is taken");
    let retyped = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert_eq!(
        retyped["components"]["graph_interface"]["pin_type"], string,
        "{retyped}"
    );
    assert!(retyped["input_values"].get("value").is_none(), "{retyped}");
}

/// A `GraphInput` choosing a Texture, a type a client does not write, while it holds a range is
/// refused for that reason, not with the list of keys a fragment's Input takes.
///
/// Mental-revert: refusing the range as a key the Input does not take answers "It takes:
/// `port`, `pin_type`, `family`", which leaves out why.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_input_choosing_a_type_a_client_does_not_write_is_refused_for_that_reason() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_float_input(false);
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                set_on_warmth_base(stream, "value", Value::Null),
                set_on_warmth_base(stream, "pin_type", json!({ "type": "texture" })),
            ] }),
        )
        .await,
    )
    .expect_err("a Texture holding a range is refused");
    assert!(
        refusal.contains("which a client does not write, and holds `range`"),
        "{refusal}"
    );
    assert!(
        refusal.contains("a type that holds a value, or array, channels or records"),
        "{refusal}"
    );
    assert!(!refusal.contains("It takes:"), "{refusal}");
}

/// A `GraphInput` linked into a Float input, whose value and range a `transact` clears while it
/// chooses a String, is refused naming the link from `warmth_base.value` into `warmth.gain`.
///
/// Mental-revert: with the cleared value drawn back from the rendering, the change is refused for
/// the 0.1 it held, and the link is never judged.
#[tokio::test(flavor = "multi_thread")]
async fn a_linked_graph_input_retyped_with_its_value_cleared_is_refused_naming_the_link() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_float_input(true);
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                set_on_warmth_base(stream, "value", Value::Null),
                set_on_warmth_base(stream, "range", Value::Null),
                set_on_warmth_base(stream, "pin_type", json!({ "type": "string" })),
            ] }),
        )
        .await,
    )
    .expect_err("a String linked into a Float is refused");
    assert!(refusal.contains("`warmth_base`"), "{refusal}");
    assert!(refusal.contains("`warmth.gain` (Float)"), "{refusal}");
    assert!(!refusal.contains("0.1"), "{refusal}");
    let kept = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert_eq!(
        kept["components"]["graph_interface"]["pin_type"],
        json!({ "type": "float" }),
        "{kept}"
    );
}
