// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The rule a reader that runs reactively sets, that something other than a function node
//! island feeds it, judged on a stream by the graph its load, its reconcile, a removal or a
//! batch of edits leaves, never by the order the links are made in.

use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::core::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind,
    PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use crate::core::error::Error;
use crate::core::execution::ProcessExecution;
use crate::core::graph::graph_snapshot::GraphSnapshot;
use crate::core::graph::LinkUniqueId;
use crate::core::nodes::HowANodeOfADescribedTypeRuns;
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::LinksAReconcileMakes;
use crate::core::runtime::{LoadedStreamInThisRuntime, Runner, RuntimeOperations};
use crate::core::test_support::{a_temporary_directory_at_owner_only_mode, ensure_test_mocks_registered};

// Each type is named as a native one is, so a load resolves it as this stream registers it,
// with no node interpreter to describe it.
const SWITCH: &str = "reactive_reader_rule::Switch";
const REACTIVE_READER: &str = "reactive_reader_rule::Meter";
const SOURCE: &str = "reactive_reader_rule::Source";

fn a_records_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(
        name,
        "",
        true,
        PortFamilyDeclaration::of_family(PortFamily::Records),
    )
    .with_delivery_profile("newest")
}

fn a_described_node_type(
    import_path: &str,
    node_kind: NodeKind,
    inputs: &[&str],
    outputs: &[&str],
) -> NodeDescriptor {
    let import_path = NodeClassImportPath::new(import_path).unwrap();
    let short_name = import_path.as_str().rsplit(':').next().unwrap().to_owned();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).unwrap(),
        import_path,
        "a fixture",
    );
    descriptor.node_kind = node_kind;
    descriptor.inputs = inputs.iter().map(|name| a_records_port(name)).collect();
    descriptor.outputs = outputs.iter().map(|name| a_records_port(name)).collect();
    descriptor
}

/// A stopped stream of a project on disk resolving a Python function node type, `Switch`, a
/// type running reactively in a helper process, `Meter`, and a source, `Source`.
fn a_stopped_stream_resolving_a_reactive_reader() -> (
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
    tempfile::TempDir,
) {
    ensure_test_mocks_registered();
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    let runner = Runner::new().expect("the engine builds");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "a",
        ))
        .expect("the stream loads");
    let node_types = stream.node_types_this_stream_resolves();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_node_type(
                SWITCH,
                NodeKind::Function {
                    language: FunctionNodeLanguage::Python,
                },
                &["x"],
                &["result"],
            ),
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: Box::new(|_node| {
                    Err(Error::NotSupported("a fixture runs no node".to_owned()))
                }),
            },
        )
        .unwrap();
    for (import_path, inputs, outputs) in [
        (REACTIVE_READER, &["in1", "in2"][..], &[][..]),
        (SOURCE, &[][..], &["out"][..]),
    ] {
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                a_described_node_type(import_path, NodeKind::Process, inputs, outputs),
                HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|_node| {
                    Err(Error::NotSupported("a fixture runs no node".to_owned()))
                })),
            )
            .unwrap();
    }
    node_types.record_the_execution_a_described_class_declares(
        NodeClassImportPath::new(REACTIVE_READER).unwrap(),
        ProcessExecution::Reactive,
    );
    (runner, stream, project_directory)
}

/// A graph document holding `nodes`, each `(name, type)` under a UUID its name derives, and
/// `links`, each `((node, output), (node, input))`, in the order given.
fn a_document_of(nodes: &[(&str, &str)], links: &[((&str, &str), (&str, &str))]) -> Value {
    json!({
        "nodes": nodes
            .iter()
            .map(|(name, node_type)| json!({
                "id": Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()),
                "name": name,
                "type": node_type,
                "config": {},
            }))
            .collect::<Vec<_>>(),
        "links": links
            .iter()
            .map(|((source, output), (target, input))| json!({
                "source": {"node": source, "port": output},
                "target": {"node": target, "port": input},
            }))
            .collect::<Vec<_>>(),
    })
}

fn load(stream: &LoadedStreamInThisRuntime, document: Value) {
    stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(document).unwrap(),
            None,
        )
        .expect("the graph loads");
}

/// Each link the stream holds set aside, as `(source node, why)`.
fn the_links_set_aside(stream: &LoadedStreamInThisRuntime) -> Vec<(String, String)> {
    stream
        .graph_only_nodes_as_drawn
        .lock()
        .links_set_aside
        .iter()
        .map(|set_aside| (set_aside.link.source.node.clone(), set_aside.described()))
        .collect()
}

fn the_live_links(stream: &LoadedStreamInThisRuntime) -> usize {
    stream
        .compiler
        .scope_reading(|graph| graph.traversal().e(()).iter().count())
}

fn the_live_nodes(stream: &LoadedStreamInThisRuntime) -> usize {
    stream
        .compiler
        .scope_reading(|graph| graph.traversal().v(()).iter().count())
}

/// The link the stream holds out of the node named `source_name`.
fn the_link_out_of(stream: &LoadedStreamInThisRuntime, source_name: &str) -> LinkUniqueId {
    let source = stream.the_node_named(source_name).unwrap().node_run_id;
    stream.compiler.scope_reading(|graph| {
        graph
            .traversal()
            .e(())
            .iter()
            .find(|link| *link.source().node_run_id() == source)
            .map(|link| link.id.clone())
            .unwrap()
    })
}

/// A reactive reader fed by `source` and by the island `switch`.
fn a_reactive_reader_fed_by_a_source_and_an_island() -> Value {
    a_document_of(
        &[
            ("source", SOURCE),
            ("switch", SWITCH),
            ("reader", REACTIVE_READER),
        ],
        &[
            (("source", "out"), ("reader", "in2")),
            (("switch", "result"), ("reader", "in1")),
        ],
    )
}

/// A load whose document lists the link from an island into a reactive reader before the link
/// feeding that island from a source sets nothing aside: the reader rule is judged once every
/// link is made.
///
/// Mental-revert: judging the rule as each link is made sets `switch_one`'s link aside, since
/// its island has no source yet when it is made.
#[test]
fn a_load_listing_an_island_link_before_the_links_feeding_its_island_sets_nothing_aside() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    load(
        &stream,
        a_document_of(
            &[
                ("source", SOURCE),
                ("switch_two", SWITCH),
                ("switch_one", SWITCH),
                ("reader", REACTIVE_READER),
            ],
            &[
                (("switch_one", "result"), ("reader", "in1")),
                (("switch_two", "result"), ("switch_one", "x")),
                (("source", "out"), ("switch_two", "x")),
            ],
        ),
    );

    assert_eq!(the_links_set_aside(&stream), Vec::new());
    assert_eq!(the_live_links(&stream), 3);
}

/// A load whose reactive reader only an island feeds sets the island's link aside, naming why,
/// and holds every node.
#[test]
fn a_load_whose_reactive_reader_only_an_island_feeds_sets_the_island_link_aside() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    load(
        &stream,
        a_document_of(
            &[("switch", SWITCH), ("reader", REACTIVE_READER)],
            &[(("switch", "result"), ("reader", "in1"))],
        ),
    );

    let set_aside = the_links_set_aside(&stream);
    assert_eq!(set_aside.len(), 1, "{set_aside:?}");
    assert_eq!(set_aside[0].0, "switch");
    assert!(set_aside[0].1.contains("would never run"), "{set_aside:?}");
    assert_eq!(the_live_links(&stream), 0);
    assert_eq!(the_live_nodes(&stream), 2);
}

/// Disconnecting a reactive reader's only source, or removing that source, through the stream
/// is refused, naming what is removed, and leaves the graph as it was.
///
/// Mental-revert: dropping the stream's removal check takes the source out and leaves the
/// reader to an island that never wakes it.
#[test]
fn a_disconnect_or_a_removal_stranding_a_reactive_reader_is_refused_by_the_stream() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    load(&stream, a_reactive_reader_fed_by_a_source_and_an_island());
    let source_link = the_link_out_of(&stream, "source");
    let source = stream.the_node_named("source").unwrap().node_run_id;

    let disconnect_refused = stream
        .disconnect(&source_link)
        .expect_err("the reader would never run again")
        .to_string();
    let removal_refused = stream
        .remove_node(&source)
        .expect_err("the reader would never run again")
        .to_string();

    assert!(
        disconnect_refused.contains(&format!("removing link `{source_link}`")),
        "{disconnect_refused}"
    );
    assert!(
        removal_refused.contains(&format!("removing node `{source}`")),
        "{removal_refused}"
    );
    for refused in [&disconnect_refused, &removal_refused] {
        assert!(
            refused.contains("or disconnect the function node link into"),
            "{refused}"
        );
    }
    assert_eq!(the_live_links(&stream), 2);
    assert_eq!(the_live_nodes(&stream), 3);
}

/// A stopped stream's reconcile to a document that takes out a reactive reader's only source
/// sets the island link it keeps aside, naming why, so the stream still plays.
///
/// Mental-revert: leaving the kept island link connected fails the stream at its start.
#[test]
fn a_reconcile_taking_out_a_reactive_readers_only_source_sets_the_kept_island_link_aside() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    load(&stream, a_reactive_reader_fed_by_a_source_and_an_island());

    stream
        .reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
            a_document_of(
                &[("switch", SWITCH), ("reader", REACTIVE_READER)],
                &[(("switch", "result"), ("reader", "in1"))],
            ),
            LinksAReconcileMakes::EachAsASavedGraphsLink,
        )
        .expect("the reconcile sets the island link aside");

    let set_aside = the_links_set_aside(&stream);
    assert_eq!(set_aside.len(), 1, "{set_aside:?}");
    assert_eq!(set_aside[0].0, "switch");
    assert!(set_aside[0].1.contains("would never run"), "{set_aside:?}");
    assert_eq!(the_live_links(&stream), 0);
    assert_eq!(the_live_nodes(&stream), 2);
}

/// A batch of graph operations taking out a source, its island and their reader is judged by
/// the graph it leaves, and takes all three out.
///
/// Mental-revert: judging each removal of the batch on its own refuses the source's.
#[test]
fn a_batch_taking_out_a_source_its_island_and_their_reader_is_taken() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    load(&stream, a_reactive_reader_fed_by_a_source_and_an_island());
    let [source, switch, reader] =
        ["source", "switch", "reader"].map(|name| stream.the_node_named(name).unwrap().node_run_id);

    stream.open_a_batch_of_graph_operations();
    for node in [&source, &switch, &reader] {
        stream.remove_node(node).expect("the batch judges its end");
    }
    stream
        .close_a_batch_of_graph_operations()
        .expect("the batch leaves no reader stranded");

    assert_eq!(the_live_nodes(&stream), 0);
}

/// A batch of graph operations that leaves a reactive reader fed only by an island is refused
/// as it closes.
#[test]
fn a_batch_leaving_a_reactive_reader_fed_only_by_an_island_is_refused_as_it_closes() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    load(&stream, a_reactive_reader_fed_by_a_source_and_an_island());
    let source = stream.the_node_named("source").unwrap().node_run_id;

    stream.open_a_batch_of_graph_operations();
    stream
        .remove_node(&source)
        .expect("the batch judges its end");
    let refused = stream
        .close_a_batch_of_graph_operations()
        .expect_err("the reader would never run");

    assert!(refused.to_string().contains("would never run"), "{refused}");
}
