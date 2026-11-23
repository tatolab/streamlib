// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The rule a reader that runs reactively sets, that something other than a function node
//! island feeds it, judged on a stream by the graph its load, its reconcile, a removal or a
//! batch of edits leaves, never by the order the links are made in.

use std::sync::Arc;

use serde_json::{Value, json};
use streamlib_project_repository::{
    StreamGraphDocument, StreamGraphDocumentLink, StreamGraphDocumentNode, StreamGraphOperation,
};
use uuid::Uuid;

use crate::core::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind,
    PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use crate::core::error::Error;
use crate::core::execution::ProcessExecution;
use crate::core::graph::LinkUniqueId;
use crate::core::graph::graph_snapshot::GraphSnapshot;
use crate::core::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};
use crate::core::nodes::HowANodeOfADescribedTypeRuns;
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::{
    KeptNodesLeftToRebuild, LinksAReconcileMakes,
};
use crate::core::runtime::{GraphEditBatch, LoadedStreamInThisRuntime, Runner, RuntimeOperations};
use crate::core::runtime::a_project_on_disk_for_a_test::THE_NAME_OF_A_PROJECT_ON_DISK_FOR_A_TEST;
use crate::core::test_support::{
    a_temporary_directory_at_owner_only_mode, a_view_reading_records_in,
    ensure_test_mocks_registered,
};

// Each type is named as a native one is, so a load resolves it as this stream registers it,
// with no node interpreter to describe it.
const SWITCH: &str = "reactive_reader_rule::Switch";
const REACTIVE_READER: &str = "reactive_reader_rule::Meter";
const SOURCE: &str = "reactive_reader_rule::Source";
const MANUAL_READER: &str = "reactive_reader_rule::ManualMeter";

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
    descriptor.function_node_runs_as_python = matches!(node_kind, NodeKind::Function { .. });
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
        (MANUAL_READER, &["in1"][..], &[][..]),
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
    node_types.record_the_execution_a_described_class_declares(
        NodeClassImportPath::new(MANUAL_READER).unwrap(),
        ProcessExecution::Manual,
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
            KeptNodesLeftToRebuild::HandedTheirValuesLive,
        )
        .expect("the reconcile sets the island link aside");

    let set_aside = the_links_set_aside(&stream);
    assert_eq!(set_aside.len(), 1, "{set_aside:?}");
    assert_eq!(set_aside[0].0, "switch");
    assert!(set_aside[0].1.contains("would never run"), "{set_aside:?}");
    assert_eq!(the_live_links(&stream), 0);
    assert_eq!(the_live_nodes(&stream), 2);
}

fn an_async_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn the_uuid_of(name: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
}

/// The link of a graph document from `(node, output)` to `(node, input)`, each node by the UUID
/// its name derives.
fn a_document_link(
    (source, output): (&str, &str),
    (target, input): (&str, &str),
) -> StreamGraphDocumentLink {
    serde_json::from_value(json!({
        "source": {"node": the_uuid_of(source), "port": output},
        "target": {"node": the_uuid_of(target), "port": input},
    }))
    .unwrap()
}

/// The removal of the node named `name`, as the stream draws it.
fn the_removal_of(stream: &LoadedStreamInThisRuntime, name: &str) -> StreamGraphOperation {
    let document = StreamGraphDocument::from_value(stream.to_json().unwrap()).unwrap();
    StreamGraphOperation::RemoveNode {
        node: document.node(the_uuid_of(name)).unwrap(),
        links: Vec::new(),
    }
}

/// The node named `name` of `node_type`, as a graph document draws it.
fn a_document_node(name: &str, node_type: &str) -> StreamGraphDocumentNode {
    serde_json::from_value(json!({
        "id": the_uuid_of(name),
        "name": name,
        "type": node_type,
        "config": {},
    }))
    .unwrap()
}

fn a_graph_edit_batch_of(stream: &Arc<LoadedStreamInThisRuntime>) -> Box<dyn GraphEditBatch> {
    <LoadedStreamInThisRuntime as RuntimeOperations>::open_a_graph_edit_batch(Arc::clone(stream))
        .expect("the batch opens")
}

/// A batch of graph edits taking out a source, its island and their reader is judged by the
/// graph it leaves, and takes all three out.
///
/// Mental-revert: judging each removal of the batch on its own refuses the source's.
#[test]
fn a_batch_taking_out_a_source_its_island_and_their_reader_is_taken() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(&stream, a_reactive_reader_fed_by_a_source_and_an_island());

    let batch = a_graph_edit_batch_of(&stream);
    for name in ["source", "switch", "reader"] {
        async_runtime
            .block_on(batch.apply_a_stream_graph_operation_live(the_removal_of(&stream, name)))
            .expect("the batch judges its end");
    }
    batch.close().expect("the batch leaves no reader stranded");

    assert_eq!(the_live_nodes(&stream), 0);
    assert_eq!(the_live_links(&stream), 0);
}

/// A batch of graph edits that leaves a reactive reader fed only by an island is refused as it
/// closes, and takes back every edit it made.
#[test]
fn a_batch_leaving_a_reactive_reader_fed_only_by_an_island_is_refused_as_it_closes() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(&stream, a_reactive_reader_fed_by_a_source_and_an_island());

    let batch = a_graph_edit_batch_of(&stream);
    async_runtime
        .block_on(batch.apply_a_stream_graph_operation_live(the_removal_of(&stream, "source")))
        .expect("the batch judges its end");
    let refused = batch.close().expect_err("the reader would never run");

    assert!(refused.to_string().contains("would never run"), "{refused}");
    assert_eq!(the_live_nodes(&stream), 3);
    assert_eq!(the_live_links(&stream), 2);
    assert!(stream.the_node_named("source").is_ok());
}

/// Two writers' batches on one stream never interleave: the second opens only once the first
/// closes, so the writer whose batch strands a reader is refused, and the other's is taken.
///
/// Mental-revert: without the stream's one writer at a time, the second writer's disconnect is
/// logged into the first writer's held commit, the first is refused and takes it back too.
#[test]
fn two_overlapping_writers_on_one_stream_are_judged_apart() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = Arc::new(an_async_runtime());
    load(&stream, a_reactive_reader_fed_by_a_source_and_an_island());

    let first = a_graph_edit_batch_of(&stream);
    async_runtime
        .block_on(first.apply_a_stream_graph_operation_live(the_removal_of(&stream, "source")))
        .expect("the batch judges its end");
    let the_first_closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let second_writer = {
        let stream = Arc::clone(&stream);
        let async_runtime = Arc::clone(&async_runtime);
        let the_first_closed = Arc::clone(&the_first_closed);
        std::thread::spawn(move || {
            let second = a_graph_edit_batch_of(&stream);
            let opened_once_the_first_closed =
                the_first_closed.load(std::sync::atomic::Ordering::SeqCst);
            async_runtime
                .block_on(second.apply_a_stream_graph_operation_live(
                    StreamGraphOperation::Disconnect {
                        link: a_document_link(("switch", "result"), ("reader", "in1")),
                    },
                ))
                .and_then(|_| second.close().map_err(|refused| refused.refusal))
                .map(|()| opened_once_the_first_closed)
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    the_first_closed.store(true, std::sync::atomic::Ordering::SeqCst);
    let first_refused = first
        .close()
        .expect_err("the first writer strands the reader");
    let opened_once_the_first_closed = second_writer
        .join()
        .unwrap()
        .expect("the second writer strands nothing");

    assert!(
        first_refused.to_string().contains("would never run"),
        "{first_refused}"
    );
    assert!(
        opened_once_the_first_closed,
        "the second writer waited for the first"
    );
    assert!(stream.the_node_named("source").is_ok());
    assert_eq!(the_live_links(&stream), 1);
}

/// A batch dropped open takes back what it made and holds nothing: the stream's next writer
/// opens at once.
#[test]
fn an_abandoned_batch_leaves_nothing_held() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(&stream, a_reactive_reader_fed_by_a_source_and_an_island());

    let batch = a_graph_edit_batch_of(&stream);
    async_runtime
        .block_on(batch.apply_a_stream_graph_operation_live(the_removal_of(&stream, "source")))
        .expect("the batch judges its end");
    drop(batch);

    assert!(stream.one_writer_of_the_graph_at_a_time.try_lock().is_ok());
    assert!(!stream.compiler.the_transaction_is_held_for_one_commit());
    assert_eq!(the_live_nodes(&stream), 3);
    assert_eq!(the_live_links(&stream), 2);
    stream
        .remove_node(&stream.the_node_named("switch").unwrap().node_run_id)
        .expect("the stream's next writer opens at once");
}

/// A batch mixing a disconnect of a link held set aside with connects drops the link's drawn
/// entry and leaves the reader rule to the close, which the batch's end satisfies.
///
/// Mental-revert: reconciling the graph at the disconnect sweeps the reader rule there, before
/// the batch connects the source the reader needs.
#[test]
fn a_batch_disconnecting_a_link_set_aside_among_its_edits_is_judged_at_its_end() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(
        &stream,
        a_document_of(
            &[
                ("source", SOURCE),
                ("switch", SWITCH),
                ("reader", REACTIVE_READER),
            ],
            &[(("switch", "result"), ("reader", "in1"))],
        ),
    );
    assert_eq!(the_links_set_aside(&stream).len(), 1);

    let batch = a_graph_edit_batch_of(&stream);
    for edit in [
        StreamGraphOperation::Disconnect {
            link: a_document_link(("switch", "result"), ("reader", "in1")),
        },
        StreamGraphOperation::Connect {
            link: a_document_link(("switch", "result"), ("reader", "in1")),
        },
        StreamGraphOperation::Connect {
            link: a_document_link(("source", "out"), ("reader", "in2")),
        },
    ] {
        async_runtime
            .block_on(batch.apply_a_stream_graph_operation_live(edit))
            .expect("the batch judges its end");
    }
    batch
        .close()
        .expect("the batch's end feeds the reader a source");

    assert_eq!(the_links_set_aside(&stream), Vec::new());
    assert_eq!(the_live_links(&stream), 2);
}

/// An add whose links list the island link into the new reader before its source link is
/// taken: the reader rule is judged once every link is made.
#[test]
fn an_add_listing_the_island_link_first_is_taken_on_a_stopped_stream() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(
        &stream,
        a_document_of(&[("source", SOURCE), ("switch", SWITCH)], &[]),
    );

    async_runtime
        .block_on(
            stream.apply_a_stream_graph_operation_live(StreamGraphOperation::AddNode {
                node: a_document_node("reader", REACTIVE_READER),
                links: vec![
                    a_document_link(("switch", "result"), ("reader", "in1")),
                    a_document_link(("source", "out"), ("reader", "in2")),
                ],
            }),
        )
        .expect("the add feeds its reader a source");

    assert_eq!(the_live_links(&stream), 2);
    assert_eq!(the_live_nodes(&stream), 3);
}

/// A reconcile to a document listing the island link into a reader before its source link sets
/// nothing aside.
#[test]
fn a_reconcile_listing_the_island_link_first_sets_nothing_aside_on_a_stopped_stream() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    load(
        &stream,
        a_document_of(
            &[
                ("source", SOURCE),
                ("switch", SWITCH),
                ("reader", REACTIVE_READER),
            ],
            &[],
        ),
    );

    stream
        .reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
            a_document_of(
                &[
                    ("source", SOURCE),
                    ("switch", SWITCH),
                    ("reader", REACTIVE_READER),
                ],
                &[
                    (("switch", "result"), ("reader", "in1")),
                    (("source", "out"), ("reader", "in2")),
                ],
            ),
            LinksAReconcileMakes::ThoseTheLiveGraphKeptThoughConnectRefusesThem,
            KeptNodesLeftToRebuild::HandedTheirValuesLive,
        )
        .expect("the reconcile feeds the reader a source");

    assert_eq!(the_links_set_aside(&stream), Vec::new());
    assert_eq!(the_live_links(&stream), 2);
}

/// What a stream announces on its topic, each connect by its source node's run id, until it
/// announces the graph changed.
struct ConnectsAnnounced(Arc<parking_lot::Mutex<(Vec<String>, bool)>>);

impl EventListener for ConnectsAnnounced {
    fn on_event(&mut self, event: &Event) -> crate::core::Result<()> {
        let Event::OfALoadedStream { event, .. } = event else {
            return Ok(());
        };
        let mut announced = self.0.lock();
        match event {
            RuntimeEvent::RuntimeWillConnect { from, .. }
            | RuntimeEvent::RuntimeDidConnect { from, .. } => {
                announced.0.push(from.node_run_id().to_string());
            }
            RuntimeEvent::GraphDidChange => announced.1 = true,
            _ => {}
        }
        Ok(())
    }

    fn on_events_lost(&mut self, _lost_event_count: u64) -> crate::core::Result<()> {
        Ok(())
    }
}

/// Listen on `stream`'s topic for the connects it announces.
fn listen_for_the_connects_of(
    stream: &LoadedStreamInThisRuntime,
) -> (
    Arc<parking_lot::Mutex<(Vec<String>, bool)>>,
    Arc<parking_lot::Mutex<dyn EventListener>>,
) {
    let announced = Arc::new(parking_lot::Mutex::new((Vec::new(), false)));
    let listener: Arc<parking_lot::Mutex<dyn EventListener>> = Arc::new(parking_lot::Mutex::new(
        ConnectsAnnounced(Arc::clone(&announced)),
    ));
    PUBSUB
        .subscribe(
            &topics::loaded_stream(stream.loaded_stream_identity()),
            Arc::clone(&listener),
        )
        .unwrap();
    (announced, listener)
}

/// The connects announced once the stream announces its graph changed, each by its source
/// node's run id.
fn the_connects_announced_once_the_graph_changed(
    announced: &parking_lot::Mutex<(Vec<String>, bool)>,
) -> Vec<String> {
    let gave_up_at = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < gave_up_at && !announced.lock().1 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    let announced = announced.lock();
    assert!(announced.1, "the stream announces its graph changed");
    announced.0.clone()
}

/// A load announces a connect only for each link it keeps: none for an island link it sets
/// aside once every link is made.
///
/// Mental-revert: announcing each connect as it is made announces the island link the load's
/// sweep then sets aside.
#[test]
fn a_load_announces_no_connect_of_an_island_link_it_sets_aside() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let (announced, _listener) = listen_for_the_connects_of(&stream);
    load(
        &stream,
        a_document_of(
            &[
                ("source", SOURCE),
                ("switch", SWITCH),
                ("reader", REACTIVE_READER),
                ("other_reader", REACTIVE_READER),
            ],
            &[
                (("switch", "result"), ("reader", "in1")),
                (("source", "out"), ("other_reader", "in1")),
            ],
        ),
    );
    let switch = stream
        .the_node_named("switch")
        .unwrap()
        .node_run_id
        .to_string();
    let source = stream
        .the_node_named("source")
        .unwrap()
        .node_run_id
        .to_string();

    let connects = the_connects_announced_once_the_graph_changed(&announced);

    assert_eq!(the_links_set_aside(&stream).len(), 1);
    assert!(!connects.contains(&switch), "{connects:?}");
    assert_eq!(connects, vec![source.clone(), source], "{connects:?}");
}

/// A batch its close refuses announces none of its connects.
///
/// Mental-revert: announcing each connect as it is made announces the island link the refused
/// close takes back.
#[test]
fn a_refused_batch_announces_no_connect() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(
        &stream,
        a_document_of(&[("switch", SWITCH), ("reader", REACTIVE_READER)], &[]),
    );
    let (announced, _listener) = listen_for_the_connects_of(&stream);

    let batch = a_graph_edit_batch_of(&stream);
    async_runtime
        .block_on(
            batch.apply_a_stream_graph_operation_live(StreamGraphOperation::Connect {
                link: a_document_link(("switch", "result"), ("reader", "in1")),
            }),
        )
        .expect("the batch judges its end");
    batch.close().expect_err("the reader would never run");
    stream
        .rename_node(
            &stream.the_node_named("reader").unwrap().node_run_id,
            "renamed",
        )
        .unwrap();

    assert_eq!(
        the_connects_announced_once_the_graph_changed(&announced),
        Vec::<String>::new()
    );
    assert_eq!(the_live_links(&stream), 0);
}

/// A load listing each island's link into its reader before the source links its island reads
/// sets aside each source link past the function node pool's cap, however its links are listed.
///
/// Mental-revert: judging the pool's cap only on a link out of an island keeps every source
/// link, and the start refuses the pool.
#[test]
fn a_load_listing_island_links_before_their_sources_sets_aside_the_links_past_the_pools_cap() {
    const READS_OF_EACH_ISLAND: usize = 30;
    let islands = streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION / READS_OF_EACH_ISLAND + 1;
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let mut nodes: Vec<(String, &str)> = Vec::new();
    let mut island_links = Vec::new();
    let mut source_links = Vec::new();
    for island in 0..islands {
        let switch = format!("switch_{island}");
        let reader = format!("reader_{island}");
        island_links.push(((switch.clone(), "result"), (reader.clone(), "in1")));
        for read in 0..READS_OF_EACH_ISLAND {
            let source = format!("source_{island}_{read}");
            source_links.push(((source.clone(), "out"), (switch.clone(), "x")));
            nodes.push((source, SOURCE));
        }
        nodes.push((switch, SWITCH));
        nodes.push((reader, MANUAL_READER));
    }
    let nodes: Vec<(&str, &str)> = nodes
        .iter()
        .map(|(name, node_type)| (name.as_str(), *node_type))
        .collect();
    let links: Vec<((&str, &str), (&str, &str))> = island_links
        .iter()
        .chain(&source_links)
        .map(|((source, output), (target, input))| {
            ((source.as_str(), *output), (target.as_str(), *input))
        })
        .collect();

    load(&stream, a_document_of(&nodes, &links));

    let set_aside = the_links_set_aside(&stream);
    let past_the_cap =
        islands * READS_OF_EACH_ISLAND - streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION;
    assert_eq!(set_aside.len(), past_the_cap, "{set_aside:?}");
    for (source, why) in &set_aside {
        assert!(source.starts_with("source_"), "{source}");
        assert!(
            why.contains("the stream's function node pool would hold"),
            "{why}"
        );
    }
}

/// A batch swapping which island feeds a reader's port, the old island's link out first, is
/// taken: the link leaving no longer counts toward the port an island feeds.
///
/// Mental-revert: counting the link pending deletion refuses the new island's link as a second
/// link into a port an island feeds.
#[test]
fn a_batch_swapping_the_island_feeding_a_port_is_taken() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(
        &stream,
        a_document_of(
            &[
                ("source", SOURCE),
                ("switch", SWITCH),
                ("other_switch", SWITCH),
                ("reader", REACTIVE_READER),
            ],
            &[
                (("source", "out"), ("reader", "in2")),
                (("switch", "result"), ("reader", "in1")),
            ],
        ),
    );

    let batch = a_graph_edit_batch_of(&stream);
    for operation in [
        StreamGraphOperation::Disconnect {
            link: a_document_link(("switch", "result"), ("reader", "in1")),
        },
        StreamGraphOperation::Connect {
            link: a_document_link(("other_switch", "result"), ("reader", "in1")),
        },
    ] {
        async_runtime
            .block_on(batch.apply_a_stream_graph_operation_live(operation))
            .expect("the swap leaves one link into the port");
    }
    batch.close().expect("the swap closes");

    assert_eq!(the_live_links(&stream), 2);
    let other_switch = stream.the_node_named("other_switch").unwrap().node_run_id;
    let reader = stream.the_node_named("reader").unwrap().node_run_id;
    let link_into_the_port = stream.compiler.scope_reading(|graph| {
        graph
            .link_into(&reader, "in1")
            .map(|link| link.source().node_run_id().clone())
    });
    assert_eq!(link_into_the_port, Some(other_switch));
}

/// A batch swapping a source an island reads, at the function node pool's inbound cap, the old
/// source's link out first, is taken: the link leaving no longer counts toward the pool.
///
/// Mental-revert: counting the island's reads pending deletion refuses the new source's link
/// as taking the pool past its cap.
#[test]
fn a_batch_swapping_a_source_an_island_reads_at_the_pools_cap_is_taken() {
    const READS_OF_EACH_ISLAND: usize = 32;
    let islands = streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION / READS_OF_EACH_ISLAND;
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    let mut nodes: Vec<(String, &str)> = vec![("spare".to_owned(), SOURCE)];
    let mut links = Vec::new();
    for island in 0..islands {
        let switch = format!("switch_{island}");
        let reader = format!("reader_{island}");
        links.push(((switch.clone(), "result"), (reader.clone(), "in1")));
        for read in 0..READS_OF_EACH_ISLAND {
            let source = format!("source_{island}_{read}");
            links.push(((source.clone(), "out"), (switch.clone(), "x")));
            nodes.push((source, SOURCE));
        }
        nodes.push((switch, SWITCH));
        nodes.push((reader, MANUAL_READER));
    }
    let nodes: Vec<(&str, &str)> = nodes
        .iter()
        .map(|(name, node_type)| (name.as_str(), *node_type))
        .collect();
    let links: Vec<((&str, &str), (&str, &str))> = links
        .iter()
        .map(|((source, output), (target, input))| {
            ((source.as_str(), *output), (target.as_str(), *input))
        })
        .collect();
    load(&stream, a_document_of(&nodes, &links));
    assert_eq!(
        the_links_set_aside(&stream),
        Vec::new(),
        "the pool is at its cap"
    );

    let batch = a_graph_edit_batch_of(&stream);
    for operation in [
        StreamGraphOperation::Disconnect {
            link: a_document_link(("source_0_0", "out"), ("switch_0", "x")),
        },
        StreamGraphOperation::Connect {
            link: a_document_link(("spare", "out"), ("switch_0", "x")),
        },
    ] {
        async_runtime
            .block_on(batch.apply_a_stream_graph_operation_live(operation))
            .expect("the swap leaves the pool at its cap");
    }
    batch.close().expect("the swap closes");

    assert_eq!(
        the_live_links(&stream),
        islands * (READS_OF_EACH_ISLAND + 1)
    );
    let _ = the_link_out_of(&stream, "spare");
}

/// A saved graph holding a link from a function node into a view, in the app process, loads as
/// the stream's graph with that link shown in `graph`, failed with why, so the person sees it
/// and removes it; a disconnect of it leaves the graph without it.
///
/// Mental-revert: refusing the link on the load fails the whole graph; setting it aside without
/// drawing it leaves the person nothing to see or remove.
#[test]
fn a_saved_graphs_island_into_the_app_process_is_shown_failed_and_removed_by_a_disconnect() {
    let (_runner, stream, project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    a_view_reading_records_in(project.path(), "program");
    let view_reader = format!("{THE_NAME_OF_A_PROJECT_ON_DISK_FOR_A_TEST}:program");
    load(
        &stream,
        a_document_of(
            &[("switch", SWITCH), ("program", &view_reader)],
            &[(("switch", "result"), ("program", "in1"))],
        ),
    );
    let the_link_into_the_program = |stream: &LoadedStreamInThisRuntime| {
        let graph = stream.to_json().expect("the graph renders");
        graph["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|link| link["target"]["port"] == "in1")
            .cloned()
    };

    let shown = the_link_into_the_program(&stream).expect("the saved link is shown");
    assert_eq!(shown["state"], "error", "{shown}");
    assert!(
        shown["error_reason"]
            .as_str()
            .is_some_and(|why| why.contains("app process")),
        "{shown}"
    );
    assert_eq!(the_live_links(&stream), 0, "the link is never wired");

    let batch = a_graph_edit_batch_of(&stream);
    async_runtime
        .block_on(
            batch.apply_a_stream_graph_operation_live(StreamGraphOperation::Disconnect {
                link: a_document_link(("switch", "result"), ("program", "in1")),
            }),
        )
        .expect("the person removes the link");
    batch.close().expect("the disconnect closes");

    assert_eq!(the_link_into_the_program(&stream), None);
    assert_eq!(the_links_set_aside(&stream), Vec::new());
}

mod links_leaving_tests;
