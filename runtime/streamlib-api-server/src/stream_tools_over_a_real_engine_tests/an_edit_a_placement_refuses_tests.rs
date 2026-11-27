// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An edit inside a placed stream that a loaded stream placing it refuses is refused as a whole
//! edit: the placing stream, the placed stream's document and the running graph are left as
//! they were, and the caller gets the placement's own refusal.

use super::*;
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

const GAIN_NODE_UUID: &str = "6f6f6f6f-6f6f-4f6f-8f6f-6f6f6f6f6f6f";

/// A loaded stream `main`, holding a source, placing the stream `sub`, which holds a node `gain`
/// whose `gain` input holds 1.0: the project, `main`'s stream id and the UUID of the node
/// placing `sub`.
async fn main_placing_sub(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
) -> (ProjectWithGraphDocuments, String, String) {
    input_value_test_node::register_the_input_value_test_node_once();
    let project = a_project_whose_main_stream_holds_one_source();
    project.write_the_graph_document(json!({
        "stream": "sub",
        "nodes": [
            { "id": GAIN_NODE_UUID, "name": "gain",
              "type": input_value_test_node::InputValueTestNode::node_class_import_path().as_str(),
              "input_values": { "gain": 1.0 } },
        ],
        "links": [],
    }));
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let main = project.stream_asset_of("main");
    let placed = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "add_node", "stream": main, "type": STREAM_FRAGMENT_NODE_TYPE,
                  "name": "sub", "as": "sub" },
                { "operation": "set_node_input_value", "stream": main, "node": "sub",
                  "input": "stream", "value": { "asset": project.stream_asset_of("sub") } },
            ] }),
        )
        .await
        .expect("main places sub");
    let placing_node = placed["added_nodes"][0]["node"]
        .as_str()
        .expect("the node placing sub")
        .to_owned();
    (project, stream_id, placing_node)
}

/// The nodes and links `graph` holds, and those the fragment of its node `placing_node`
/// places, by UUID, as [`the_nodes_and_links_held_in`] reads them.
fn the_nodes_and_links_with_those_placed_by(graph: &Value, placing_node: &str) -> Value {
    let placing = graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["id"] == placing_node)
        .unwrap_or_else(|| panic!("the graph holds no node {placing_node}: {graph}"));
    json!({
        "own_level": the_nodes_and_links_held_in(graph),
        "placed": the_nodes_and_links_held_in(&placing["fragment"]),
    })
}

/// Assert the documents of `project`'s stream `main`, read with what they place, equal `running`,
/// `main`'s running graph, at its own level and inside the fragment of its node `placing_node`.
fn assert_mains_documents_equal_its_running_graph(
    project: &ProjectWithGraphDocuments,
    running: &Value,
    placing_node: &str,
) {
    let read = streamlib_project_repository::graph_document_as_its_project_reads_it(
        project.path(),
        &project.the_graph_document_of("main"),
        &streamlib_project_repository::StreamDocumentsAnEditStaged::default(),
    )
    .expect("main's document reads with what it places");
    assert_eq!(
        the_nodes_and_links_with_those_placed_by(&read, placing_node),
        the_nodes_and_links_with_those_placed_by(running, placing_node),
        "main's documents equal its running graph"
    );
}

/// A stream fragment placing `main`, added inside the stream `sub` that `main` places, is
/// refused by the loaded `main`, in which the two would hold one another: the transaction is
/// refused whole in `main`'s words, `main`'s and `sub`'s documents and `main`'s running graph
/// are as they were, and the documents still equal the running graph.
///
/// Mental-revert: bringing `main` to the streams it places only once the documents hold the
/// edit commits the fragment to `sub`'s document, answers the transaction as applied, and logs
/// `main`'s refusal.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_a_placement_refuses_is_refused_whole_and_leaves_every_stream_as_it_was() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, stream_id, placing_node) = main_placing_sub(&engine).await;
    let main = project.stream_asset_of("main");
    let main_before = project.the_graph_document_of("main");
    let sub_before = project.the_graph_document_of("sub");
    let running_before = the_nodes_and_links_with_those_placed_by(
        &engine.the_live_graph_of(&stream_id),
        &placing_node,
    );

    let refused = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "add_node", "stream": main, "level": [placing_node],
                  "type": STREAM_FRAGMENT_NODE_TYPE, "name": "main-again", "as": "again" },
                { "operation": "set_node_input_value", "stream": main, "level": [placing_node],
                  "node": "again", "input": "stream", "value": { "asset": main } },
            ] }),
        )
        .await
        .expect_err("main refuses to hold itself through sub");

    assert!(
        refused.runtime_words.contains("cycle")
            && refused.runtime_words.contains("`main`")
            && refused.runtime_words.contains("`sub`"),
        "main's own refusal reaches the caller: {refused:?}"
    );
    assert_eq!(
        refused.refusal_code,
        ControlOperationRefusalCode::Refused,
        "{refused:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), main_before);
    assert_eq!(project.the_graph_document_of("sub"), sub_before);
    let running = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        the_nodes_and_links_with_those_placed_by(&running, &placing_node),
        running_before
    );
    assert_mains_documents_equal_its_running_graph(&project, &running, &placing_node);
}

/// A node type only `main` resolves, so a placement of it in another stream is refused.
const A_TYPE_ONLY_MAIN_RESOLVES: &str = "placement_refusal::OnlyMainResolvesIt";

/// Have the loaded stream `stream_id` resolve [`A_TYPE_ONLY_MAIN_RESOLVES`].
fn register_the_type_only_main_resolves_on(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
) {
    use streamlib::sdk::descriptors::{NodeClassImportPath, NodeClassShortName, NodeDescriptor};
    let node_type = NodeClassImportPath::new(A_TYPE_ONLY_MAIN_RESOLVES).expect("a node type");
    engine
        .engine
        .loaded_stream_of_the_id(stream_id)
        .expect("the stream is loaded")
        .node_types_this_stream_resolves()
        .register_a_described_type_that_runs_no_node_for_a_test(
            NodeDescriptor::new(
                NodeClassShortName::new("OnlyMainResolvesIt").expect("a short name"),
                node_type,
                "a node type only one stream resolves",
            ),
            None,
        )
        .expect("the stream resolves the type");
}

/// `main` and `zeta` both place `sub`; a transaction setting `gain` to 2.0 inside `sub` and
/// adding beside it a node of a type only `main` resolves is taken by `main`, whose batch comes
/// first, then refused by `zeta`: the transaction is refused in `zeta`'s words, and `main`,
/// which had taken it, runs `sub` as it was again, `gain` at 1.0, its documents equal to its
/// running graph.
///
/// Mental-revert: leaving a placement that took the edit as taking its batch back leaves it,
/// leaves `main` running `gain` at 2.0, which no document holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_placement_that_took_an_edit_another_refuses_runs_as_it_was_again() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, main_id, placing_node) = main_placing_sub(&engine).await;
    register_the_type_only_main_resolves_on(&engine, &main_id);
    project.write_the_graph_document(json!({
        "stream": "zeta",
        "nodes": [
            { "id": Uuid::new_v4(), "name": "sub", "type": STREAM_FRAGMENT_NODE_TYPE,
              "input_values": { "stream": { "asset": project.stream_asset_of("sub") } } },
        ],
        "links": [],
    }));
    let zeta_id = engine.a_stream_loaded_from_its_graph_document(&project, "zeta");
    assert!(
        main_id < zeta_id,
        "main's batch opens first: {main_id} {zeta_id}"
    );
    let sub_before = project.the_graph_document_of("sub");
    let main_running_before = the_nodes_and_links_with_those_placed_by(
        &engine.the_live_graph_of(&main_id),
        &placing_node,
    );

    let refused = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "set_node_input_value", "stream": project.stream_asset_of("main"),
                  "level": [placing_node],
                  "node": the_uuid_a_held_node_takes_under(&placing_node, GAIN_NODE_UUID),
                  "input": "gain", "value": 2.0 },
                { "operation": "add_node", "stream": project.stream_asset_of("main"),
                  "level": [placing_node], "type": A_TYPE_ONLY_MAIN_RESOLVES, "name": "only" },
            ] }),
        )
        .await
        .expect_err("zeta refuses a type it does not resolve");

    assert!(
        refused.runtime_words.contains("OnlyMainResolvesIt"),
        "zeta's own refusal reaches the caller: {refused:?}"
    );
    assert_eq!(
        refused.refusal_code,
        ControlOperationRefusalCode::NodeTypeNotFound,
        "zeta's refusal keeps its own kind: {refused:?}"
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before);
    let main_running = engine.the_live_graph_of(&main_id);
    assert_eq!(
        the_nodes_and_links_with_those_placed_by(&main_running, &placing_node),
        main_running_before
    );
    assert_mains_documents_equal_its_running_graph(&project, &main_running, &placing_node);
}

/// `main` places `sub`, whose document was since written, outside any transaction, to hold a node
/// of a type no stream resolves; an edit of the stream `other`, which places nothing, commits, as
/// no stream the edit changes is placed by `main`.
///
/// Mental-revert: bringing every loaded stream holding a stream fragment to what it places
/// inside the transaction refuses the edit of `other` in `main`'s words, the type not found.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_no_placement_reaches_commits_while_a_placement_is_out_of_step() {
    const OTHER_GAIN_NODE_UUID: &str = "7a7a7a7a-7a7a-4a7a-8a7a-7a7a7a7a7a7a";
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, _, _) = main_placing_sub(&engine).await;
    project.write_the_graph_document(json!({
        "stream": "other",
        "nodes": [
            { "id": OTHER_GAIN_NODE_UUID, "name": "gain",
              "type": input_value_test_node::InputValueTestNode::node_class_import_path().as_str(),
              "input_values": { "gain": 1.0 } },
        ],
        "links": [],
    }));
    engine.a_stream_loaded_from_its_graph_document(&project, "other");
    project.write_the_graph_document(json!({
        "stream": "sub",
        "nodes": [
            { "id": GAIN_NODE_UUID, "name": "gain",
              "type": "placement_refusal::NoStreamResolvesIt", "input_values": {} },
        ],
        "links": [],
    }));

    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "set_node_input_value", "stream": project.stream_asset_of("other"),
                  "node": OTHER_GAIN_NODE_UUID, "input": "gain", "value": 2.0 },
            ] }),
        )
        .await
        .expect("an edit of a stream main does not place commits");

    assert_eq!(
        project.the_graph_document_of("other")["nodes"][0]["input_values"]["gain"],
        json!(2.0)
    );
}
