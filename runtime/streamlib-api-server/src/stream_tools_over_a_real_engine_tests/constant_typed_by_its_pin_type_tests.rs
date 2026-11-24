// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `Constant` over a real engine: listed once among the runtime's own node types, its
//! `pin_type` a choice among the small types; placed holding a String, its value reaches a node
//! reading it while the stream plays; and a Float Constant saving 0.1 that chooses an Integer
//! saves 0, cast in the same edit, one undo putting back both; and a Core Nodes `Constant` saved
//! with its link loads as the runtime's own, holding the Float it held.

use std::time::Instant;

use parking_lot::Mutex;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::nodes::ReactiveNode;

use super::graph_input_retyped_through_transact_tests::transact_by_alice;
use super::input_value_set_tests::{InputValueTestNode, register_the_input_value_test_node_once};
use super::node_type_reads_tests::a_project_listing;
use super::*;
use streamlib_project_repository::CONSTANT_NODE_TYPE;

/// The UUID the graph document gives its `Constant`.
const CONSTANT_NODE_UUID: &str = "c0c0c0c0-c0c0-4c0c-8c0c-c0c0c0c0c0c0";

/// The UUID the graph document gives its `reader`.
const READER_NODE_UUID: &str = "c3ec3ec3-ec3e-4c3e-8c3e-c3ec3ec3ec3e";

/// Every Records message a [`StreamToolsTestConstantReader`] has read, by any reader: a String
/// travels as the record holding it under `value`.
static STRINGS_READ: Mutex<Vec<Value>> = Mutex::new(Vec::new());

/// How long a Constant's value may take to reach a node reading it once its stream plays.
const ARRIVES_WITHIN: Duration = Duration::from_secs(10);

/// A native reader of Strings on its input `word`, which records each one it reads.
#[streamlib::sdk::node(
    execution = reactive,
    input("word", delivery_profile = "newest", pin_type = String, flow = Stream)
)]
pub struct StreamToolsTestConstantReader;

impl ReactiveNode for StreamToolsTestConstantReader::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw("word")? {
            let word: Value =
                rmp_serde::from_slice(message.payload.bytes()).expect("a String message decodes");
            STRINGS_READ.lock().push(word);
        }
        Ok(())
    }
}

/// A project whose stream `main` holds the `Constant` `greeting` of `pin_type` saving `value`,
/// linked into the `word` of a [`StreamToolsTestConstantReader`] `reader` when `read`.
fn a_project_whose_main_stream_holds_a_constant(
    pin_type: Value,
    value: Value,
    read: bool,
) -> ProjectWithGraphDocuments {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<StreamToolsTestConstantReader::Node>());
    streamlib_media_builtins::register_the_conversion_node_types();
    let mut nodes = vec![json!({
        "id": CONSTANT_NODE_UUID, "name": "greeting", "type": CONSTANT_NODE_TYPE,
        "input_values": { "pin_type": pin_type, "value": value },
    })];
    let mut links = Vec::new();
    if read {
        nodes.push(json!({
            "id": READER_NODE_UUID, "name": "reader",
            "type": StreamToolsTestConstantReader::node_class_import_path().as_str(),
        }));
        links.push(json!({ "source": { "node": "greeting", "port": "value" },
                           "target": { "node": "reader", "port": "word" } }));
    }
    ProjectWithGraphDocuments::holding(json!({ "stream": "main", "nodes": nodes, "links": links }))
}

/// The value `greeting` saves and the pin type it chooses, as `stream_id`'s live graph and
/// `project`'s graph document of `main` hold them, with the pin type its `value` output carries.
fn what_the_constant_holds(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    project: &ProjectWithGraphDocuments,
    stream_id: &str,
) -> (Value, Value, Value, Value) {
    let live = the_node_named(&engine.the_live_graph_of(stream_id), "greeting");
    let saved = super::input_value_set_tests::the_input_values_of(
        &project.the_graph_document_of("main"),
        CONSTANT_NODE_UUID,
    );
    let output = live["ports"]["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|output| output["name"] == "value")
        .unwrap_or_else(|| panic!("the Constant has a `value` output: {live}"));
    (
        live["input_values"]["value"].clone(),
        saved["value"].clone(),
        saved["pin_type"].clone(),
        output["pin_type"].clone(),
    )
}

/// `Constant` is listed once among the runtime's own node types, filed under Math, its
/// `pin_type` a choice among the eight small types holding a Float until one is chosen.
///
/// Mental-revert: listing the Core Nodes `Constant` beside it, or none, fails the count.
#[tokio::test(flavor = "multi_thread")]
async fn the_constant_is_listed_once_its_pin_type_choosing_among_the_small_types() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([]));

    let listing = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_node_types",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the node types are listed");
    let constants: Vec<&Value> = listing["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|source| source["node_types"].as_array().into_iter().flatten())
        .filter(|listed| {
            listed["node_type"]
                .as_str()
                .is_some_and(|node_type| node_type.ends_with(":Constant"))
        })
        .collect();
    assert_eq!(constants.len(), 1, "{listing}");
    assert_eq!(constants[0]["node_type"], CONSTANT_NODE_TYPE, "{listing}");
    assert_eq!(constants[0]["category"], "Math", "{listing}");

    let described = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "describe_node_types",
            json!({ "project_directory": project.path(), "node_types": [CONSTANT_NODE_TYPE] }),
        )
        .await,
    )
    .expect("the Constant is described");
    let pin_type = described["node_types"][0]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|input| input["name"] == "pin_type")
        .unwrap_or_else(|| panic!("the Constant has a `pin_type`: {described}"));
    let among = pin_type["pin_type"]["pin_type"]["among"]
        .as_array()
        .unwrap_or_else(|| panic!("`pin_type` is a choice: {pin_type}"));
    assert_eq!(among.len(), 8, "{pin_type}");
    assert_eq!(among[0], json!({ "type": "float" }), "{pin_type}");
    assert!(among.contains(&json!({ "type": "color" })), "{pin_type}");
}

/// A Constant holding a String, linked into a node reading Strings, hands it the String once
/// its stream plays.
///
/// Mental-revert: a Constant run by no node, or typed a Float whatever its `pin_type`, leaves the
/// reader without the String, the second refusing the link into `reader.word`.
#[tokio::test(flavor = "multi_thread")]
async fn a_constant_holding_a_string_hands_it_to_the_node_reading_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let word = json!(format!("hello-{}", Uuid::new_v4()));
    let project = a_project_whose_main_stream_holds_a_constant(
        json!({ "type": "string" }),
        word.clone(),
        true,
    );

    engine.playing(&project).await;

    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    let record = json!({ "value": word });
    while !STRINGS_READ.lock().contains(&record) {
        assert!(Instant::now() < gave_up_at, "no reader read {word}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}

/// A Float Constant saving 0.1 that chooses an Integer saves 0 in the same edit, cast by the
/// registered `FloatToInteger`; `graph` shows its `value` output an Integer; and one undo puts
/// back the Float and 0.1.
///
/// Mental-revert: without the cast the Integer is refused for the 0.1 it holds; without the
/// Constant's ports resolved from its `pin_type`, its output stays a Float.
#[tokio::test(flavor = "multi_thread")]
async fn a_float_constant_choosing_an_integer_casts_its_value_and_one_undo_puts_both_back() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project =
        a_project_whose_main_stream_holds_a_constant(json!({ "type": "float" }), json!(0.1), false);
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([{ "operation": "set_node_input_value", "stream": stream,
                 "node": CONSTANT_NODE_UUID, "input": "pin_type",
                 "value": { "type": "integer" } }]),
    )
    .await
    .expect("the retype casts the saved value");
    assert_eq!(
        retyped["saved_values_retyped"],
        json!([{ "stream": stream_id, "node": CONSTANT_NODE_UUID, "value_before": 0.1,
                 "value": 0, "cast_by": "tatolab.stream:FloatToInteger" }]),
        "{retyped}"
    );
    let (live, saved, chosen, output) = what_the_constant_holds(&engine, &project, &stream_id);
    assert_eq!(
        (live, saved, chosen),
        (json!(0), json!(0), json!({ "type": "integer" }))
    );
    assert_eq!(output["type"], "integer", "{output}");

    engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the retype undoes");
    let (live, saved, chosen, output) = what_the_constant_holds(&engine, &project, &stream_id);
    assert_eq!(
        (live, saved, chosen),
        (json!(0.1), json!(0.1), json!({ "type": "float" })),
        "one undo puts back both the type and the value"
    );
    assert_eq!(output["type"], "float", "{output}");
}

/// A Core Nodes `Constant` saved holding 0.25, linked from its `result` into a Float input,
/// loads as the runtime's own Constant holding the Float 0.25, its link from `value`.
///
/// Mental-revert: without the rename the type is no node type the stream offers; without
/// `result` as the former name of its output, the saved link names no port of the Constant.
#[tokio::test(flavor = "multi_thread")]
async fn a_core_nodes_constant_saved_with_its_link_loads_as_the_runtimes_own() {
    register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": CONSTANT_NODE_UUID, "name": "greeting",
              "type": "imports/core-nodes/core_nodes.math_nodes:Constant",
              "input_values": { "value": 0.25 } },
            { "id": READER_NODE_UUID, "name": "warmth",
              "type": InputValueTestNode::node_class_import_path().as_str() },
        ],
        "links": [{ "source": { "node": "greeting", "port": "result" },
                    "target": { "node": "warmth", "port": "gain" } }],
    }));
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let live_graph = engine.the_live_graph_of(&stream_id);
    let constant = the_node_named(&live_graph, "greeting");
    assert_eq!(constant["type"], CONSTANT_NODE_TYPE, "{constant}");
    assert_eq!(
        constant["input_values"],
        json!({ "pin_type": { "type": "float" }, "value": 0.25 }),
        "{constant}"
    );
    let links = live_graph["links"].as_array().expect("links");
    assert_eq!(links.len(), 1, "{live_graph}");
    assert_eq!(links[0]["source"]["port"], "value", "{live_graph}");
}
