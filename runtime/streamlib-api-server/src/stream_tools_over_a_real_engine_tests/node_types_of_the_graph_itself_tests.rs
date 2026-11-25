// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `GraphInput`, `GraphOutput`, `SetVariable` and `GetVariable` over a real engine: listed among
//! the runtime's own node types in the Graph category, described with typed inputs and outputs,
//! and added by their listed names, so an agent builds a stream holding a variable from the
//! listing alone, its `GetVariable` offering the stream's variables.

use super::input_value_set_tests::{MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer};
use super::node_type_reads_tests::a_project_listing;
use super::project_node_type_listing_tests::STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING;
use super::*;

/// Give `project` a venv whose interpreter lists no node type of the project's own.
fn a_venv_listing_no_node_type_of_its_own_in(project: &ProjectWithGraphDocuments) {
    write_an_executable_script_from_a_child_process(
        &project.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    std::fs::write(
        project.path().join("listing.json"),
        json!({ "sources": [] }).to_string(),
    )
    .expect("the listing document");
}

/// The node types the runtime lists as its own in `listing`, a `list_node_types` answer.
fn the_built_in_node_types_of(listing: &Value) -> Vec<Value> {
    listing["sources"]
        .as_array()
        .expect("the sources")
        .iter()
        .find(|source| source["source"] == "built_in")
        .unwrap_or_else(|| panic!("the runtime's own source is listed: {listing}"))["node_types"]
        .as_array()
        .expect("the runtime's own node types")
        .clone()
}

/// The node types of the graph itself are listed among the runtime's own, each filed under
/// Graph, of no kind, and described by the runtime.
///
/// Mental-revert: listing only the registry's node types leaves out all four.
#[tokio::test(flavor = "multi_thread")]
async fn the_four_node_types_of_the_graph_itself_are_listed_in_the_graph_category() {
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

    let of_the_graph: Vec<Value> = the_built_in_node_types_of(&listing)
        .into_iter()
        .filter(|listed| listed["category"] == "Graph")
        .collect();
    assert_eq!(
        of_the_graph
            .iter()
            .map(|listed| listed["node_type"].as_str().expect("a type"))
            .collect::<Vec<_>>(),
        [
            "tatolab.stream:GraphInput",
            "tatolab.stream:GraphOutput",
            "tatolab.stream:SetVariable",
            "tatolab.stream:GetVariable",
        ],
        "{listing}"
    );
    for listed in &of_the_graph {
        assert_eq!(listed["described_from"], "built_in", "{listed}");
        assert!(listed["kind"].is_null(), "no node code runs it: {listed}");
    }
}

/// `SetVariable` and `GetVariable` are described with their settings and their `value` typed:
/// a `value` input of any type that takes a wire on the setter, a `value` output of any type on
/// the getter, and `variable` a String setting read at setup on both.
///
/// Mental-revert: describing only the registry's node types refuses both as not offered.
#[tokio::test(flavor = "multi_thread")]
async fn the_variable_node_types_are_described_with_typed_inputs_and_outputs() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([]));

    let described = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "describe_node_types",
            json!({ "project_directory": project.path(),
                    "node_types": ["tatolab.stream:SetVariable", "tatolab.stream:GetVariable"] }),
        )
        .await,
    )
    .expect("the variable node types are described");
    let [set, get] = described["node_types"]
        .as_array()
        .expect("the node types")
        .as_slice()
    else {
        panic!("two node types were named: {described}");
    };

    let inputs_by_name = |description: &Value| -> Vec<(String, Value, Value)> {
        description["inputs"]
            .as_array()
            .expect("inputs")
            .iter()
            .map(|input| {
                (
                    input["name"].as_str().expect("a name").to_string(),
                    input["pin_type"].clone(),
                    input["mode"].clone(),
                )
            })
            .collect()
    };
    assert_eq!(set["category"], "Graph", "{set}");
    assert_eq!(
        inputs_by_name(set),
        [
            (
                "variable".to_string(),
                json!({ "kind": "is", "pin_type": { "type": "string" } }),
                json!("setting_only")
            ),
            (
                "node_reference".to_string(),
                json!({ "kind": "is", "pin_type": {
                    "type": "node_reference", "one_referrer_per_referenced_node": false } }),
                json!("setting_only")
            ),
            (
                "value".to_string(),
                json!({ "kind": "any" }),
                json!("wire_only")
            ),
        ],
        "{set}"
    );
    assert_eq!(set["outputs"], json!([]), "{set}");
    assert_eq!(
        inputs_by_name(get),
        [(
            "variable".to_string(),
            json!({ "kind": "is", "pin_type": { "type": "string" } }),
            json!("setting_only")
        )],
        "{get}"
    );
    assert_eq!(
        get["outputs"]
            .as_array()
            .expect("outputs")
            .iter()
            .map(|output| (output["name"].clone(), output["pin_type"].clone()))
            .collect::<Vec<_>>(),
        [(json!("value"), json!({ "kind": "any" }))],
        "{get}"
    );
}

/// An agent reads the variable node types' names from the listing, adds a `SetVariable` and a
/// `GetVariable` by them, links the mixer's `level` into the setter and the getter into the
/// mixer's `gain`, and the stream's graph shows the getter's `variable` as a menu of the stream's
/// variables, one set later by a setter added with no name, which takes its type's name, among
/// them, and its `value` of the Float its setter holds.
///
/// Mental-revert: rendering the getter's `variable` as the String its type declares offers no
/// variable to pick, and its `value` renders untyped.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_builds_a_stream_with_a_variable_from_the_listing_alone() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    a_venv_listing_no_node_type_of_its_own_in(&project);
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");
    let listing = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_node_types",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the node types are listed");
    let listed_type = |short_name: &str| -> String {
        the_built_in_node_types_of(&listing)
            .iter()
            .map(|listed| listed["node_type"].as_str().expect("a type").to_string())
            .find(|node_type| node_type.ends_with(&format!(":{short_name}")))
            .unwrap_or_else(|| panic!("`{short_name}` is listed: {listing}"))
    };
    let set_on = |node: &str, input: &str, value: Value| {
        json!({ "operation": "set_node_input_value", "stream": stream, "node": node,
                "input": input, "value": value })
    };
    let transact = |operations: Value| {
        tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
    };

    tool_answer(
        &transact(json!([
            { "operation": "add_node", "stream": stream, "as": "set_level", "name": "set_level",
              "type": listed_type("SetVariable") },
            set_on("set_level", "variable", json!("level")),
            { "operation": "add_node", "stream": stream, "as": "get_level", "name": "get_level",
              "type": listed_type("GetVariable") },
            set_on("get_level", "variable", json!("level")),
            { "operation": "connect", "stream": stream,
              "source": { "node": MIXER_NODE_UUID, "port": "level" },
              "target": { "node": "set_level", "port": "value" } },
            { "operation": "connect", "stream": stream,
              "source": { "node": "get_level", "port": "value" },
              "target": { "node": MIXER_NODE_UUID, "port": "gain" } },
        ]))
        .await,
    )
    .expect("a stream holding a variable is built from the listed names");

    let graph = engine.the_live_graph_of(&stream_id);
    let get = the_node_named(&graph, "get_level");
    let variable = &get["ports"]["inputs"][0];
    assert_eq!(variable["name"], "variable", "{get}");
    assert_eq!(
        variable["pin_type"],
        json!({ "type": "menu", "option_names": ["level"] }),
        "{get}"
    );
    let value = &get["ports"]["outputs"][0];
    assert_eq!(value["name"], "value", "{get}");
    assert_eq!(value["pin_type"], json!({ "type": "float" }), "{get}");
    assert!(
        graph["links"]
            .as_array()
            .expect("links")
            .iter()
            .any(|link| link["target"]["port"] == "gain"),
        "the getter's drawn link renders: {graph}"
    );

    tool_answer(
        &transact(json!([
            { "operation": "add_node", "stream": stream, "as": "set_monitor",
              "type": listed_type("SetVariable") },
            set_on("set_monitor", "variable", json!("monitor")),
            set_on("set_monitor", "node_reference", json!({ "node": "monitor" })),
        ]))
        .await,
    )
    .expect("a second variable is set");
    let graph = engine.the_live_graph_of(&stream_id);
    the_node_named(&graph, "setvariable");
    let get = the_node_named(&graph, "get_level");
    assert_eq!(
        get["ports"]["inputs"][0]["pin_type"],
        json!({ "type": "menu", "option_names": ["level", "monitor"] }),
        "the menu offers the variable set since: {get}"
    );
}
