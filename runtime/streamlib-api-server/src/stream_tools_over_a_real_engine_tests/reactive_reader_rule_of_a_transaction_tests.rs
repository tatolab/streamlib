// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A transaction's edits of a running stream are one batch of graph edits, judged by the graph
//! they leave for the rule a reader that runs reactively sets, so the order they are listed in
//! never refuses a transaction whose end leaves no reader stranded; and a transaction whose end
//! strands one leaves the stream and the project's documents as they were.

use streamlib::sdk::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind,
    PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use streamlib::sdk::execution::ProcessExecution;

use super::*;

// Each type is named as a native one is, so an add resolves it as the stream registers it,
// with no node interpreter to describe it.
const SOURCE_TYPE: &str = "transaction_reader_rule::Source";
const SWITCH_TYPE: &str = "transaction_reader_rule::Switch";
const READER_TYPE: &str = "transaction_reader_rule::Meter";

fn a_records_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(
        name,
        "",
        true,
        PortFamilyDeclaration::of_family(PortFamily::Records),
    )
    .with_delivery_profile("newest")
}

fn a_node_type(
    node_type: &str,
    node_kind: NodeKind,
    inputs: &[&str],
    outputs: &[&str],
) -> NodeDescriptor {
    let node_type = NodeClassImportPath::new(node_type).expect("a node type");
    let short_name = node_type.as_str().rsplit(':').next().expect("a short name");
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).expect("a short name"),
        node_type.clone(),
        "a node type a transaction places",
    );
    descriptor.node_kind = node_kind;
    descriptor.inputs = inputs.iter().map(|input| a_records_port(input)).collect();
    descriptor.outputs = outputs
        .iter()
        .map(|output| a_records_port(output))
        .collect();
    descriptor
}

/// A loaded stream `main`, its graph empty, resolving a source, a function node `Switch` and a
/// reader running reactively, its id and the project holding it.
fn a_stream_resolving_a_reactive_reader(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
) -> (String, ProjectWithGraphDocuments) {
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main", "nodes": [], "links": [],
    }));
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = engine
        .engine
        .loaded_stream_of_the_id(&stream_id)
        .expect("the stream is loaded");
    let node_types = stream.node_types_this_stream_resolves();
    for (descriptor, execution) in [
        (
            a_node_type(SOURCE_TYPE, NodeKind::Process, &[], &["out"]),
            None,
        ),
        (
            a_node_type(
                SWITCH_TYPE,
                NodeKind::Function {
                    language: FunctionNodeLanguage::Python,
                },
                &["x"],
                &["result"],
            ),
            None,
        ),
        (
            a_node_type(READER_TYPE, NodeKind::Process, &["in1", "in2"], &[]),
            Some(ProcessExecution::Reactive),
        ),
    ] {
        node_types
            .register_a_described_type_that_runs_no_node_for_a_test(descriptor, execution)
            .expect("the stream resolves the type");
    }
    (stream_id, project)
}

/// Add a source, an island and a reader both feed to `main`, listing the island's link first.
async fn add_a_reader_fed_by_a_source_and_an_island(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    project: &ProjectWithGraphDocuments,
) {
    let stream = project.stream_asset_of("main");
    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": stream, "type": SWITCH_TYPE,
                      "name": "switch", "as": "switch" },
                    { "operation": "add_node", "stream": stream, "type": READER_TYPE,
                      "name": "reader", "as": "reader" },
                    { "operation": "connect", "stream": stream,
                      "source": { "node": "switch", "port": "result" },
                      "target": { "node": "reader", "port": "in1" } },
                    { "operation": "add_node", "stream": stream, "type": SOURCE_TYPE,
                      "name": "source", "as": "source" },
                    { "operation": "connect", "stream": stream,
                      "source": { "node": "source", "port": "out" },
                      "target": { "node": "reader", "port": "in2" } },
                ],
            }),
        )
        .await
        .expect("the transaction's end feeds the reader a source");
}

/// The UUID of the node named `name` in `graph`.
fn the_uuid_of_the_node_named(graph: &Value, name: &str) -> String {
    the_node_named(graph, name)["id"]
        .as_str()
        .expect("the node holds a UUID")
        .to_string()
}

/// The removals of the nodes named `names`, in order, as transaction operations on `main`.
fn the_removals_of(project: &ProjectWithGraphDocuments, names: &[&str]) -> Vec<Value> {
    let document = project.the_graph_document_of("main");
    names
        .iter()
        .map(|name| {
            json!({ "operation": "remove_node", "stream": project.stream_asset_of("main"),
                    "node": the_uuid_of_the_node_named(&document, name) })
        })
        .collect()
}

/// A transaction taking out a source, its island and their reader, the source first, is taken:
/// the reader rule is judged by the graph the transaction leaves.
///
/// Mental-revert: applying each removal on its own, outside one batch, refuses the source's,
/// which strands the reader until the transaction's later removals take it out.
#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_taking_out_a_source_its_island_and_their_reader_is_taken() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, project) = a_stream_resolving_a_reactive_reader(&engine);
    add_a_reader_fed_by_a_source_and_an_island(&engine, &project).await;

    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": the_removals_of(&project, &["source", "switch", "reader"]),
            }),
        )
        .await
        .expect("the transaction strands no reader");

    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        Vec::<String>::new()
    );
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        Vec::<String>::new()
    );
}

/// A transaction whose end strands a reader is refused, and leaves the running stream and the
/// project's documents as they were.
#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_stranding_a_reader_is_refused_and_leaves_everything_as_it_was() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, project) = a_stream_resolving_a_reactive_reader(&engine);
    add_a_reader_fed_by_a_source_and_an_island(&engine, &project).await;
    let live_before = engine.the_live_graph_of(&stream_id);
    let document_before = project.the_graph_document_of("main");

    let refused = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": the_removals_of(&project, &["source"]),
            }),
        )
        .await
        .expect_err("the reader would never run again");

    assert!(
        format!("{refused:?}").contains("would never run"),
        "{refused:?}"
    );
    let live_after = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        sorted_node_names_in(&live_after),
        sorted_node_names_in(&live_before)
    );
    assert_eq!(
        links_by_node_name_in(&live_after),
        links_by_node_name_in(&live_before)
    );
    assert_eq!(project.the_graph_document_of("main"), document_before);
}

/// The undo of a transaction taking out a source, its island and their reader puts all three
/// back, and its redo takes all three out again, the source first.
///
/// Mental-revert: applying a reversal's removals each on its own refuses the redo at the
/// source's.
#[tokio::test(flavor = "multi_thread")]
async fn the_undo_and_the_redo_of_a_transaction_taking_out_a_reader_and_what_feeds_it_are_taken() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, project) = a_stream_resolving_a_reactive_reader(&engine);
    add_a_reader_fed_by_a_source_and_an_island(&engine, &project).await;
    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": the_removals_of(&project, &["source", "switch", "reader"]),
            }),
        )
        .await
        .expect("the transaction strands no reader");

    engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the undo puts the reader and what feeds it back");
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(sorted_node_names_in(&live), ["reader", "source", "switch"]);
    assert_eq!(links_by_node_name_in(&live).len(), 2, "{live}");

    engine
        .the_answer_or_the_refusal_of(
            "redo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the redo takes them out again");
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        Vec::<String>::new()
    );
}

/// A transaction swapping which island feeds a reader's port, the old island's link out first,
/// is taken: the link leaving no longer counts toward the port an island feeds.
///
/// Mental-revert: counting the link pending deletion refuses the new island's link as a second
/// link into a port an island feeds.
#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_swapping_the_island_feeding_a_port_is_taken() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, project) = a_stream_resolving_a_reactive_reader(&engine);
    add_a_reader_fed_by_a_source_and_an_island(&engine, &project).await;
    let stream = project.stream_asset_of("main");
    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": stream, "type": SWITCH_TYPE,
                      "name": "other_switch" },
                ],
            }),
        )
        .await
        .expect("the other island is added");
    let document = project.the_graph_document_of("main");
    let uuid_of = |name: &str| the_uuid_of_the_node_named(&document, name);

    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "disconnect", "stream": stream,
                      "source": { "node": uuid_of("switch"), "port": "result" },
                      "target": { "node": uuid_of("reader"), "port": "in1" } },
                    { "operation": "connect", "stream": stream,
                      "source": { "node": uuid_of("other_switch"), "port": "result" },
                      "target": { "node": uuid_of("reader"), "port": "in1" } },
                ],
            }),
        )
        .await
        .expect("the swap leaves one link into the port");

    let links = links_by_node_name_in(&engine.the_live_graph_of(&stream_id));
    assert!(
        links.iter().any(|link| link.starts_with("other_switch/")),
        "{links:?}"
    );
    assert!(
        !links.iter().any(|link| link.starts_with("switch/")),
        "{links:?}"
    );
}
