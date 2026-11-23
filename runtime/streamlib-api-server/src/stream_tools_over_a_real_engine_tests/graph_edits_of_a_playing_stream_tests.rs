// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A transaction's edits of a playing stream of native nodes, applied as one batch of graph
//! edits: a swap of what feeds a windowed port is taken, and a transaction whose one commit
//! fails after it took a node out puts that node back.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::nodes::ReactiveNode;

use super::*;

/// A native source of Channels on its output `audio`, with nothing to do.
#[streamlib::sdk::node(execution = manual, output("audio", family = Channels))]
pub struct StreamToolsTestAudioSource;

impl ManualNode for StreamToolsTestAudioSource::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// A native reader of Channels on its input `audio`, declaring no window, so the channel it is
/// first wired to is created shallower than a windowed ring.
#[streamlib::sdk::node(
    execution = reactive,
    input("audio", delivery_profile = "ordered", family = Channels)
)]
pub struct StreamToolsTestPlainAudioReader;

impl ReactiveNode for StreamToolsTestPlainAudioReader::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// A native reader of Channels on its input `audio`, declaring a window.
#[streamlib::sdk::node(
    execution = reactive,
    input(
        "audio",
        delivery_profile = "ordered",
        audio_window(
            sample_rate = 16_000,
            channels = 1,
            dtype = "f32",
            window_size = 512,
            hop = 512
        ),
        family = Channels,
    )
)]
pub struct StreamToolsTestWindowedAudioReader;

impl ReactiveNode for StreamToolsTestWindowedAudioReader::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn register_the_audio_nodes_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        NODE_REGISTRY.register::<StreamToolsTestAudioSource::Node>();
        NODE_REGISTRY.register::<StreamToolsTestPlainAudioReader::Node>();
        NODE_REGISTRY.register::<StreamToolsTestWindowedAudioReader::Node>();
    });
}

/// A project whose stream `main` holds `nodes`, each `(name, type)`, and `links`, each
/// `(source, target)` from one node's `audio` to another's.
fn a_project_of_audio_nodes(
    nodes: &[(&str, &String)],
    links: &[(&str, &str)],
) -> ProjectWithGraphDocuments {
    register_the_audio_nodes_once();
    register_the_stream_tools_test_source_once();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": nodes
            .iter()
            .map(|(name, node_type)| json!({
                "id": Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()),
                "name": name,
                "type": node_type,
            }))
            .collect::<Vec<_>>(),
        "links": links
            .iter()
            .map(|(source, target)| json!({
                "source": { "node": source, "port": "audio" },
                "target": { "node": target, "port": "audio" },
            }))
            .collect::<Vec<_>>(),
    }))
}

fn an_audio_source() -> String {
    StreamToolsTestAudioSource::node_class_import_path()
        .as_str()
        .to_owned()
}

fn a_windowed_reader() -> String {
    StreamToolsTestWindowedAudioReader::node_class_import_path()
        .as_str()
        .to_owned()
}

fn a_plain_reader() -> String {
    StreamToolsTestPlainAudioReader::node_class_import_path()
        .as_str()
        .to_owned()
}

fn an_idle_node() -> String {
    StreamToolsTestSource::node_class_import_path()
        .as_str()
        .to_owned()
}

/// The link from `source`'s `audio` to `target`'s, each node by its UUID in `document`, as a
/// transaction operation on `main`.
fn an_audio_link(
    operation: &str,
    project: &ProjectWithGraphDocuments,
    document: &Value,
    (source, target): (&str, &str),
) -> Value {
    json!({ "operation": operation, "stream": project.stream_asset_of("main"),
            "source": { "node": the_node_named(document, source)["id"], "port": "audio" },
            "target": { "node": the_node_named(document, target)["id"], "port": "audio" } })
}

/// A transaction on a playing stream swapping which source feeds a windowed port, the old link
/// out first, is taken.
///
/// Mental-revert: counting the link pending deletion refuses the new one as a second link into
/// a windowed port.
#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_swapping_the_source_of_a_windowed_port_on_a_playing_stream_is_taken() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_of_audio_nodes(
        &[
            ("one", &an_audio_source()),
            ("two", &an_audio_source()),
            ("windowed", &a_windowed_reader()),
        ],
        &[("one", "windowed")],
    );
    engine.playing(&project).await;
    let document = project.the_graph_document_of("main");

    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    an_audio_link("disconnect", &project, &document, ("one", "windowed")),
                    an_audio_link("connect", &project, &document, ("two", "windowed")),
                ],
            }),
        )
        .await
        .expect("the swap leaves one link into the port");

    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&project.stream_id_of("main"))),
        ["two/audio -> windowed/audio"]
    );
}

/// A transaction on a playing stream whose one commit fails after it took a node out puts the
/// node back, and leaves the project's documents as they were.
///
/// Mental-revert: reversing only the values a take-back leaves, as for a batch taken back
/// whole, leaves the node taken out gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_whose_commit_fails_after_a_removal_puts_the_node_back() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_of_audio_nodes(
        &[
            ("source", &an_audio_source()),
            ("plain", &a_plain_reader()),
            ("windowed", &a_windowed_reader()),
            ("idle", &an_idle_node()),
        ],
        &[("source", "plain")],
    );
    engine.playing(&project).await;
    let stream_id = project.stream_id_of("main");
    let document_before = project.the_graph_document_of("main");

    let refused = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "remove_node", "stream": project.stream_asset_of("main"),
                      "node": the_node_named(&document_before, "idle")["id"] },
                    an_audio_link("connect", &project, &document_before, ("source", "windowed")),
                ],
            }),
        )
        .await
        .expect_err("the windowed port cannot read through the channel's shallow ring");

    assert!(format!("{refused:?}").contains("bags deep"), "{refused:?}");
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["idle", "plain", "source", "windowed"]
    );
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        ["source/audio -> plain/audio"]
    );
    assert_eq!(project.the_graph_document_of("main"), document_before);
}
