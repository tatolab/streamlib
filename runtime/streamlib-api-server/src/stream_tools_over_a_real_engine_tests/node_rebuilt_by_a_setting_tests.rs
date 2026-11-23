// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A setting a node reads at setup, set while its stream runs, rebuilds that one node: a
//! transaction setting three of its settings rebuilds it once and names it under
//! `nodes_rebuilt`, and one setting a value the node cannot build with is refused in the node's
//! words, the node built again from the value it had and the document left holding it.

use super::input_value_set_tests::the_input_values_of;
use super::*;

use streamlib::sdk::runtime::a_node_reading_its_settings_at_setup_for_a_test::{
    A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH, forget_every_setup_run,
    the_node_type_reading_its_settings_at_setup, the_settings_each_setup_read,
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

fn set_on_the_blur(stream_asset: Uuid, input: &str, value: Value) -> Value {
    json!({ "operation": "set_node_input_value", "stream": stream_asset, "node": BLUR_NODE_UUID,
            "input": input, "value": value })
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(nodes_reading_their_settings_at_setup)]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_transaction_setting_three_settings_read_at_setup_rebuilds_their_node_once_and_names_it()
{
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
        .expect("the node builds with the three settings");

    assert_eq!(
        answer["nodes_rebuilt"],
        json!([{ "stream": stream_id, "node": BLUR_NODE_UUID }]),
        "{answer}"
    );
    let setups = the_settings_each_setup_read(blur());
    assert_eq!(
        setups.len(),
        2,
        "one setup at the start, one rebuild: {setups:?}"
    );
    assert_eq!(
        Value::Object(setups[1].clone()),
        json!({ "kernel_size": 5, "width": 1280, "height": 720 })
    );
    let blur_as_running = the_node_named(&engine.the_live_graph_of(&stream_id), "blur");
    assert_eq!(
        blur_as_running["components"]["state"], "Running",
        "{blur_as_running}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(nodes_reading_their_settings_at_setup)]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_setting_its_node_cannot_build_with_is_refused_and_the_node_runs_on_with_its_old_value() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, stream_id) =
        the_stream_main_running_a_node_reading_its_settings_at_setup(&engine);
    let stream_asset = project.stream_asset_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": BLUR_NODE_UUID, "input": "kernel_size",
                    "value": A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH,
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a kernel size the node cannot build with is refused");

    assert!(
        refusal.runtime_words.contains("`blur` was rebuilt")
            && refusal.runtime_words.contains("blurs nothing"),
        "{refusal:?}"
    );
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), BLUR_NODE_UUID),
        json!({ "kernel_size": 3 }),
        "the document keeps the value the node builds with"
    );
    let blur_as_running = the_node_named(&engine.the_live_graph_of(&stream_id), "blur");
    assert_eq!(
        blur_as_running["components"]["state"], "Running",
        "{blur_as_running}"
    );
    assert_eq!(
        the_settings_each_setup_read(blur())
            .last()
            .and_then(|settings| settings.get("kernel_size").cloned()),
        Some(json!(3)),
        "the node is built again from the kernel size it had"
    );
}

const LEVEL_NODE_UUID: &str = "8d8d8d8d-8d8d-4d8d-8d8d-8d8d8d8d8d8d";

/// A transaction that sets the running `blur`'s kernel size, which leaves it to rebuild while
/// the transaction's batch is open, and then retypes a Float `GraphInput` saving 0.1 within
/// `[0, 1]` to a String, refused partway for the range a String does not take, answers with
/// that refusal within a minute: the reversal of the retype's first set leaves the rebuild to
/// the refusal, which takes the batch back and builds `blur` again from its kernel size 3; the
/// next transaction answers too.
///
/// Mental-revert: rebuilding `blur` in the retype's own reversal, while the batch still holds
/// the stream's one writer, waits for ever, holding the project's lock, and the transaction
/// does not answer within the minute.
#[test]
#[serial_test::serial(nodes_reading_their_settings_at_setup)]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_retype_refused_after_a_set_left_a_node_to_rebuild_answers_and_builds_it_back() {
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
        the_settings_each_setup_read(blur())
            .last()
            .and_then(|settings| settings.get("kernel_size").cloned()),
        Some(json!(3)),
        "`blur` is built again from the kernel size it had"
    );
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
