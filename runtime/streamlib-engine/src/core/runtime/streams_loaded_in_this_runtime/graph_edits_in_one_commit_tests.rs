// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Graph edits held for one commit on a stream of native nodes that build: a swap of what feeds
//! one port judged by the graph it leaves, a refused batch leaving a playing stream as it was, a
//! lifecycle change waiting for an open batch, and a commit failing after it changed what runs.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_project_repository::{
    StreamGraphDocument, StreamGraphDocumentLink, StreamGraphOperation,
};
use uuid::Uuid;

use crate::core::graph::graph_snapshot::GraphSnapshot;
use crate::core::graph::{GraphNodeWithComponents, NodeInstanceComponent};
use crate::core::nodes::NODE_REGISTRY;
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::LinksAReconcileMakes;
use crate::core::runtime::{
    GraphEditBatch, HowFarARefusedBatchWasTakenBack, LoadedStreamInThisRuntime, Runner,
    RuntimeOperations, RuntimeStatus,
};
use crate::core::test_support::{
    MockChannelsSourceNode, MockOutputOnlyNode, MockWindowedAudioConsumerNode,
    a_temporary_directory_at_owner_only_mode, ensure_test_mocks_registered,
};

/// A consumer of `audio` that declares no window, so the channel it is first wired to is
/// created at its delivery profile's depth, shallower than a windowed ring.
#[crate::node(
    execution = reactive,
    input("audio", delivery_profile = "ordered", family = Channels),
)]
pub(crate) struct PlainAudioConsumerOfAGraphEditTest;

impl crate::core::ReactiveNode for PlainAudioConsumerOfAGraphEditTest::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

fn the_uuid_of(name: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
}

const AUDIO: &str = "audio";

/// The stream `a` of a project on disk, loaded and not started, holding `nodes`, each
/// `(name, type)`, and `links`, each `((node, output), (node, input))`.
fn a_stream_holding(
    nodes: &[(&str, &str)],
    links: &[((&str, &str), (&str, &str))],
) -> (
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
    tempfile::TempDir,
) {
    ensure_test_mocks_registered();
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<PlainAudioConsumerOfAGraphEditTest::Node>());
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    let runner = Runner::new().expect("the engine builds");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "a",
        ))
        .expect("the stream loads");
    stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(a_document_of(nodes, links)).unwrap(),
            None,
        )
        .expect("the graph loads");
    (runner, stream, project_directory)
}

fn a_document_of(nodes: &[(&str, &str)], links: &[((&str, &str), (&str, &str))]) -> Value {
    json!({
        "nodes": nodes
            .iter()
            .map(|(name, node_type)| json!({
                "id": the_uuid_of(name),
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

fn a_channels_source() -> String {
    MockChannelsSourceNode::node_class_import_path()
        .as_str()
        .to_owned()
}

fn a_windowed_consumer() -> String {
    MockWindowedAudioConsumerNode::node_class_import_path()
        .as_str()
        .to_owned()
}

fn a_plain_consumer() -> String {
    PlainAudioConsumerOfAGraphEditTest::node_class_import_path()
        .as_str()
        .to_owned()
}

fn an_idle_node() -> String {
    MockOutputOnlyNode::node_class_import_path()
        .as_str()
        .to_owned()
}

/// Two channels sources, `one` and `two`, a windowed consumer `windowed` that `one` feeds, and
/// an idle node, `idle`.
fn a_stream_whose_windowed_port_one_source_feeds() -> (
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
    tempfile::TempDir,
) {
    let (source, windowed, idle) = (a_channels_source(), a_windowed_consumer(), an_idle_node());
    a_stream_holding(
        &[
            ("one", &source),
            ("two", &source),
            ("windowed", &windowed),
            ("idle", &idle),
        ],
        &[(("one", AUDIO), ("windowed", AUDIO))],
    )
}

fn play(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream plays");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("every node runs");
}

fn an_async_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn a_graph_edit_batch_of(stream: &Arc<LoadedStreamInThisRuntime>) -> Box<dyn GraphEditBatch> {
    <LoadedStreamInThisRuntime as RuntimeOperations>::open_a_graph_edit_batch(Arc::clone(stream))
        .expect("the batch opens")
}

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

/// The swap of what feeds `windowed`'s port: `one`'s link out, `two`'s in.
fn the_swap_of_the_source_feeding_the_windowed_port() -> [StreamGraphOperation; 2] {
    [
        StreamGraphOperation::Disconnect {
            link: a_document_link(("one", AUDIO), ("windowed", AUDIO)),
        },
        StreamGraphOperation::Connect {
            link: a_document_link(("two", AUDIO), ("windowed", AUDIO)),
        },
    ]
}

fn apply_each(
    async_runtime: &tokio::runtime::Runtime,
    batch: &dyn GraphEditBatch,
    operations: impl IntoIterator<Item = StreamGraphOperation>,
) -> crate::core::Result<()> {
    for operation in operations {
        async_runtime.block_on(batch.apply_a_stream_graph_operation_live(operation))?;
    }
    Ok(())
}

/// Each link the stream's graph holds that stands, as `source -> target`, by node name.
fn the_links_standing(stream: &LoadedStreamInThisRuntime) -> Vec<String> {
    let mut links: Vec<String> = stream.compiler.scope_reading(|graph| {
        let name_of = |node_run_id| {
            graph
                .traversal()
                .v(node_run_id)
                .first()
                .map(|node| node.display_name.clone())
                .unwrap_or_default()
        };
        graph
            .traversal()
            .e(())
            .iter()
            .filter(|link| graph.stands(link))
            .map(|link| {
                format!(
                    "{} -> {}",
                    name_of(link.source().node_run_id()),
                    name_of(link.to_port().node_run_id())
                )
            })
            .collect()
    });
    links.sort();
    links
}

/// Whether the node named `name` is in the graph, built.
fn is_built(stream: &LoadedStreamInThisRuntime, name: &str) -> bool {
    let Ok(node) = stream.the_node_named(name) else {
        return false;
    };
    stream.compiler.scope_reading(|graph| {
        graph
            .traversal()
            .v(&node.node_run_id)
            .first()
            .is_some_and(|node| node.has::<NodeInstanceComponent>())
    })
}

/// A batch swapping which source feeds a windowed port, the old link out first, is taken on a
/// stopped stream and on a playing one: the link leaving no longer counts toward the port.
///
/// Mental-revert: counting the link pending deletion refuses the new one as a second link into
/// a windowed port.
#[test]
#[serial]
fn a_batch_swapping_the_source_of_a_windowed_port_is_taken_stopped_and_playing() {
    for playing in [false, true] {
        let (_runner, stream, _project) = a_stream_whose_windowed_port_one_source_feeds();
        if playing {
            play(&stream);
        }
        let async_runtime = an_async_runtime();

        let batch = a_graph_edit_batch_of(&stream);
        apply_each(
            &async_runtime,
            batch.as_ref(),
            the_swap_of_the_source_feeding_the_windowed_port(),
        )
        .expect("the swap leaves one link into the port");
        batch.close().expect("the swap closes");

        assert_eq!(
            the_links_standing(&stream),
            ["two -> windowed"],
            "playing: {playing}"
        );
        if playing {
            stream.stop().expect("the stream stops");
        }
    }
}

/// A reconcile swapping which source feeds a windowed port sets nothing aside: the link it
/// takes out no longer counts toward the port.
///
/// Mental-revert: counting the link pending deletion sets the new link aside as a second link
/// into a windowed port.
#[test]
#[serial]
fn a_reconcile_swapping_the_source_of_a_windowed_port_sets_nothing_aside() {
    let (_runner, stream, _project) = a_stream_whose_windowed_port_one_source_feeds();
    let (source, windowed, idle) = (a_channels_source(), a_windowed_consumer(), an_idle_node());

    stream
        .reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
            a_document_of(
                &[
                    ("one", &source),
                    ("two", &source),
                    ("windowed", &windowed),
                    ("idle", &idle),
                ],
                &[(("two", AUDIO), ("windowed", AUDIO))],
            ),
            LinksAReconcileMakes::EachAsASavedGraphsLink,
        )
        .expect("the reconcile swaps the source");

    assert!(
        stream
            .graph_only_nodes_as_drawn
            .lock()
            .links_set_aside
            .is_empty()
    );
    assert_eq!(the_links_standing(&stream), ["two -> windowed"]);
}

/// A batch on a playing stream refused part-way and dropped leaves the stream as it was: the
/// node it took out is still built, and the links are those before it.
#[test]
#[serial]
fn a_refused_batch_on_a_playing_stream_leaves_it_as_it_was() {
    let (_runner, stream, _project) = a_stream_whose_windowed_port_one_source_feeds();
    play(&stream);
    let async_runtime = an_async_runtime();
    let idle = StreamGraphDocument::from_value(stream.to_json().unwrap())
        .unwrap()
        .node(the_uuid_of("idle"))
        .unwrap();

    let batch = a_graph_edit_batch_of(&stream);
    apply_each(
        &async_runtime,
        batch.as_ref(),
        [StreamGraphOperation::RemoveNode {
            node: idle,
            links: Vec::new(),
        }],
    )
    .expect("the removal waits for the close");
    let refused = apply_each(
        &async_runtime,
        batch.as_ref(),
        [StreamGraphOperation::Connect {
            link: a_document_link(("two", AUDIO), ("windowed", AUDIO)),
        }],
    )
    .expect_err("a windowed port takes one link");
    drop(batch);

    assert!(refused.to_string().contains("windowed port"), "{refused}");
    assert!(
        is_built(&stream, "idle"),
        "the node taken out is back, built"
    );
    assert_eq!(the_links_standing(&stream), ["one -> windowed"]);
    assert_eq!(stream.status(), RuntimeStatus::Started);
    stream.stop().expect("the stream stops");
}

/// A stop of a playing stream while a batch is open on another thread waits for the batch to
/// close, or to be dropped, then tears every node down.
///
/// Mental-revert: a stop that does not wait for the stream's one writer commits nothing under
/// the batch's hold, so the nodes it queues for removal keep running.
#[test]
#[serial]
fn a_stop_waits_for_an_open_batch_then_tears_every_node_down() {
    for closed in [true, false] {
        let (_runner, stream, _project) = a_stream_whose_windowed_port_one_source_feeds();
        play(&stream);
        let async_runtime = an_async_runtime();

        let batch = a_graph_edit_batch_of(&stream);
        apply_each(
            &async_runtime,
            batch.as_ref(),
            [StreamGraphOperation::Disconnect {
                link: a_document_link(("one", AUDIO), ("windowed", AUDIO)),
            }],
        )
        .expect("the disconnect waits for the close");
        let stopping = {
            let stream = Arc::clone(&stream);
            std::thread::spawn(move || stream.stop())
        };
        std::thread::sleep(Duration::from_millis(300));
        let stopped_before_the_batch_let_go = stopping.is_finished();
        if closed {
            batch.close().expect("the batch closes");
        } else {
            drop(batch);
        }
        stopping
            .join()
            .unwrap()
            .expect("the stop tears the stream down");

        assert!(
            !stopped_before_the_batch_let_go,
            "the stop waited for the batch (closed: {closed})"
        );
        assert_eq!(stream.status(), RuntimeStatus::Stopped);
        assert_eq!(
            stream
                .compiler
                .scope_reading(|graph| graph.traversal().v(()).iter().count()),
            0,
            "every node is torn down (closed: {closed})"
        );
    }
}

/// A stop on the thread holding a batch open is refused rather than waiting for itself.
#[test]
#[serial]
fn a_stop_on_the_thread_holding_a_batch_is_refused() {
    let (_runner, stream, _project) = a_stream_whose_windowed_port_one_source_feeds();
    play(&stream);

    let batch = a_graph_edit_batch_of(&stream);
    let refused = stream.stop().expect_err("the stop would wait for itself");
    batch.close().expect("the batch closes");

    assert!(
        refused
            .to_string()
            .contains("close the batch of graph edits first"),
        "{refused}"
    );
    stream
        .stop()
        .expect("the stream stops once the batch closed");
}

/// A batch whose one commit fails after it took a node out of the playing stream says it was
/// taken back only as far as nothing built or wired holds it, so its caller reverses it.
///
/// Mental-revert: reporting every refused close as taken back whole leaves the caller to
/// reverse nothing, and the node taken out stays gone.
#[test]
#[serial]
fn a_batch_whose_commit_fails_after_a_removal_says_it_was_not_taken_back_whole() {
    let (source, plain, windowed, idle) = (
        a_channels_source(),
        a_plain_consumer(),
        a_windowed_consumer(),
        an_idle_node(),
    );
    let (_runner, stream, _project) = a_stream_holding(
        &[
            ("source", &source),
            ("plain", &plain),
            ("windowed", &windowed),
            ("idle", &idle),
        ],
        &[(("source", AUDIO), ("plain", AUDIO))],
    );
    play(&stream);
    let async_runtime = an_async_runtime();
    let idle = StreamGraphDocument::from_value(stream.to_json().unwrap())
        .unwrap()
        .node(the_uuid_of("idle"))
        .unwrap();

    let batch = a_graph_edit_batch_of(&stream);
    apply_each(
        &async_runtime,
        batch.as_ref(),
        [
            StreamGraphOperation::RemoveNode {
                node: idle,
                links: Vec::new(),
            },
            StreamGraphOperation::Connect {
                link: a_document_link(("source", AUDIO), ("windowed", AUDIO)),
            },
        ],
    )
    .expect("each edit waits for the close");
    let refused = batch
        .close()
        .expect_err("the windowed port cannot read through the channel's shallow ring");

    assert!(
        refused.to_string().contains("bags deep"),
        "{}",
        refused.refusal
    );
    assert_eq!(
        refused.taken_back,
        HowFarARefusedBatchWasTakenBack::AsFarAsNothingBuiltOrWiredHoldsIt
    );
    assert!(stream.the_node_named("idle").is_err(), "the removal ran");
    assert_eq!(stream.status(), RuntimeStatus::Started);
    stream.stop().expect("the stream stops");
}

/// A forced shutdown of a playing stream ends it even while a batch stays open: the stop gives
/// up waiting for the batch, takes it back on its writer's behalf and tears every node down,
/// and the batch's later close or drop leaves the stopped stream as it is.
///
/// Mental-revert: a stop that gives up while the batch still holds the compiler's transaction
/// commits nothing under that hold, so every node keeps running, and the batch's later close
/// compiles its edits onto the stopped stream.
#[test]
#[serial]
fn a_forced_shutdown_ends_a_stream_whose_batch_stays_open() {
    for closed in [true, false] {
        let (_runner, stream, _project) = a_stream_whose_windowed_port_one_source_feeds();
        play(&stream);
        let async_runtime = an_async_runtime();

        let batch = a_graph_edit_batch_of(&stream);
        apply_each(
            &async_runtime,
            batch.as_ref(),
            [StreamGraphOperation::Disconnect {
                link: a_document_link(("one", AUDIO), ("windowed", AUDIO)),
            }],
        )
        .expect("the disconnect waits for the close");
        stream.force_this_streams_shutdown("a batch stays open");
        let ended = stream.wait_for_this_streams_end_within(Duration::from_secs(10));
        let the_stopped_stream = || {
            (
                stream.status(),
                stream.compiler.scope_reading(|graph| {
                    (
                        graph.traversal().v(()).iter().count(),
                        graph.traversal().e(()).iter().count(),
                    )
                }),
                stream.compiler.logged_pending_operations().len(),
                stream.compiler.the_transaction_is_held_for_one_commit(),
            )
        };
        let stopped = the_stopped_stream();
        let closed_or_dropped = if closed {
            batch.close().map_err(|refused| refused.to_string())
        } else {
            drop(batch);
            Ok(())
        };
        let after_the_batch_let_go = the_stopped_stream();

        assert!(
            ended,
            "the forced shutdown ended the stream (closed: {closed})"
        );
        assert_eq!(
            stopped,
            (RuntimeStatus::Stopped, (0, 0), 0, false),
            "the stop tore every node down and left nothing logged (closed: {closed})"
        );
        assert_eq!(
            after_the_batch_let_go, stopped,
            "the batch letting go left the stopped stream as it was (closed: {closed}, \
             close: {closed_or_dropped:?})"
        );
        if closed {
            assert!(
                closed_or_dropped
                    .as_ref()
                    .is_err_and(|refusal| refusal.contains("forced")),
                "the close says the forced shutdown took the batch back: {closed_or_dropped:?}"
            );
        }
    }
}

/// A second batch asked for on the thread holding the stream's open batch is refused at once,
/// not left waiting for the stream's one writer that thread holds itself.
///
/// Mental-revert: opening it waits for the first batch to close, which only its own thread
/// can do, and the test never returns.
#[test]
#[serial]
fn a_second_batch_on_the_thread_holding_one_is_refused() {
    let (_runner, stream, _project) = a_stream_whose_windowed_port_one_source_feeds();

    let batch = a_graph_edit_batch_of(&stream);
    let second = <LoadedStreamInThisRuntime as RuntimeOperations>::open_a_graph_edit_batch(
        Arc::clone(&stream),
    );
    batch.close().expect("the first batch closes");

    let Err(refused) = second else {
        panic!("a second batch opened on the thread holding the first");
    };
    assert!(
        refused
            .to_string()
            .contains("close the batch of graph edits first"),
        "{refused}"
    );
    a_graph_edit_batch_of(&stream)
        .close()
        .expect("a batch opens once the first closed");
}

/// A batch dropped unclosed takes back the rename it made: the node is named as before.
///
/// Mental-revert: a take-back that leaves names alone keeps the node renamed, though the
/// caller reverses only what a take-back leaves.
#[test]
#[serial]
fn a_dropped_batch_takes_back_its_rename() {
    let (_runner, stream, _project) = a_stream_whose_windowed_port_one_source_feeds();
    let async_runtime = an_async_runtime();

    let batch = a_graph_edit_batch_of(&stream);
    apply_each(
        &async_runtime,
        batch.as_ref(),
        [StreamGraphOperation::RenameNode {
            node: the_uuid_of("idle"),
            from: "idle".to_owned(),
            to: "renamed".to_owned(),
        }],
    )
    .expect("the rename is made in the batch");
    assert!(stream.the_node_named("renamed").is_ok());
    drop(batch);

    assert!(
        stream.the_node_named("idle").is_ok(),
        "the node is named as before"
    );
    assert!(stream.the_node_named("renamed").is_err());
}
