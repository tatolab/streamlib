// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A setting set while its stream runs is handed to its node where it runs, with no setup run
//! again: a transaction setting three of them hands the node all three and names no node
//! rebuilt, and one refused partway hands the node back the values it had.

use super::*;

use streamlib::sdk::runtime::a_node_reading_its_settings_at_setup_for_a_test::{
    forget_every_setup_run, the_node_type_reading_its_settings_at_setup,
    the_settings_each_setup_read, the_values_last_handed_to,
};

const BLUR_NODE_UUID: &str = "7c7c7c7c-7c7c-4c7c-8c7c-7c7c7c7c7c7c";

/// The engine running the stream `main` of a project holding one node `blur` reading its
/// settings at setup, its kernel size 3; the project and the stream's id.
fn the_stream_main_running_a_node_reading_its_settings_at_setup(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
) -> (ProjectWithGraphDocuments, String) {
    forget_every_setup_run();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": BLUR_NODE_UUID, "name": "blur",
              "type": the_node_type_reading_its_settings_at_setup().as_str(),
              "input_values": { "kernel_size": 3 } },
        ],
        "links": [],
    }));
    let stream_id = engine.a_stream_started_from_its_graph_document(&project, "main");
    (project, stream_id)
}

fn blur() -> Uuid {
    Uuid::parse_str(BLUR_NODE_UUID).unwrap()
}

/// The kernel size the running `blur` was last handed.
fn the_kernel_size_last_handed_to_the_blur() -> Option<Value> {
    the_values_last_handed_to(blur()).and_then(|values| values.get("kernel_size").cloned())
}

fn set_on_the_blur(stream_asset: Uuid, input: &str, value: Value) -> Value {
    json!({ "operation": "set_node_input_value", "stream": stream_asset, "node": BLUR_NODE_UUID,
            "input": input, "value": value })
}

/// Mental-revert: leaving the node to rebuild for a setting set while its stream runs runs its
/// setup again and names it under `nodes_rebuilt`.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(nodes_reading_their_settings_at_setup)]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_transaction_setting_three_settings_hands_them_to_their_running_node_with_no_setup() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, stream_id) =
        the_stream_main_running_a_node_reading_its_settings_at_setup(&engine);
    let stream_asset = project.stream_asset_of("main");

    let answer = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                set_on_the_blur(stream_asset, "kernel_size", json!(5)),
                set_on_the_blur(stream_asset, "width", json!(1280)),
                set_on_the_blur(stream_asset, "height", json!(720)),
            ] }),
        )
        .await
        .expect("the node takes the three settings");

    assert!(answer.get("nodes_rebuilt").is_none(), "{answer}");
    let setups = the_settings_each_setup_read(blur());
    assert_eq!(setups.len(), 1, "one setup, at the start: {setups:?}");
    assert_eq!(
        the_values_last_handed_to(blur()).map(Value::Object),
        Some(json!({ "kernel_size": 5, "width": 1280, "height": 720 }))
    );
    let blur_as_running = the_node_named(&engine.the_live_graph_of(&stream_id), "blur");
    assert_eq!(
        blur_as_running["components"]["state"], "Running",
        "{blur_as_running}"
    );
}

const LEVEL_NODE_UUID: &str = "8d8d8d8d-8d8d-4d8d-8d8d-8d8d8d8d8d8d";

/// A transaction that sets the running `blur`'s kernel size and then retypes a Float
/// `GraphInput` saving 0.1 within `[0, 1]` to a String, refused partway for the range a String
/// does not take, answers with that refusal within a minute and hands `blur` back its kernel
/// size 3, with no setup run again; the next transaction answers too.
///
/// Mental-revert: leaving the kernel size set live when the transaction is refused hands
/// `blur` nothing back, and it holds 5.
#[test]
#[serial_test::serial(nodes_reading_their_settings_at_setup)]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_retype_refused_after_a_set_answers_and_hands_the_node_back_its_value() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    streamlib_media_builtins::register_the_conversion_node_types();
    forget_every_setup_run();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": BLUR_NODE_UUID, "name": "blur",
              "type": the_node_type_reading_its_settings_at_setup().as_str(),
              "input_values": { "kernel_size": 3 } },
            { "id": LEVEL_NODE_UUID, "name": "level", "type": "tatolab.stream:GraphInput",
              "input_values": { "port": "level", "pin_type": { "type": "float" }, "value": 0.1,
                          "range": [0, 1] } },
        ],
        "links": [],
    }));
    let stream_id = engine.a_stream_started_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let answered_within_a_minute = |operations: Value| {
        engine.the_answer_or_the_refusal_within(
            std::time::Duration::from_secs(60),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
    };

    let refusal = answered_within_a_minute(json!([
        set_on_the_blur(stream_asset, "kernel_size", json!(5)),
        { "operation": "set_node_input_value", "stream": stream_asset, "node": LEVEL_NODE_UUID,
          "input": "pin_type", "value": { "type": "string" } },
    ]))
    .expect("the transaction answers")
    .expect_err("a String holding a range is refused");

    assert!(refusal.runtime_words.contains("`range`"), "{refusal:?}");
    assert_eq!(
        the_kernel_size_last_handed_to_the_blur(),
        Some(json!(3)),
        "`blur` is handed back the kernel size it had"
    );
    assert_eq!(the_settings_each_setup_read(blur()).len(), 1);
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        the_node_named(&live, "blur")["components"]["state"],
        "Running"
    );
    assert_eq!(
        the_node_named(&live, "level")["components"]["graph_interface"]["pin_type"],
        json!({ "type": "float" })
    );

    answered_within_a_minute(json!([set_on_the_blur(
        stream_asset,
        "kernel_size",
        json!(7)
    )]))
    .expect("the next transaction answers")
    .expect("the next kernel size is set");
}
