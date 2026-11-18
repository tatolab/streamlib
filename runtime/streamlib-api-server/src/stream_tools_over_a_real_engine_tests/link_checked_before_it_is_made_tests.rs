// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link` over a real engine answers what `connect` answers for the same link: the same
//! whole refusal for each link `connect` refuses, and the outcome of each link it makes, while
//! the graph, its document and the project's history are left as they were.

use super::input_value_set_tests::{MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer};
use super::*;

/// The UUID of the top-level `GraphInput` `volume` beside the mixer, whose `value` is a setting.
const VOLUME_GRAPH_INPUT_NODE_UUID: &str = "71717171-7171-4171-8171-717171717171";

/// The UUID no node of the mixer's stream holds.
const NO_NODE_UUID: &str = "0e0e0e0e-0e0e-4e0e-8e0e-0e0e0e0e0e0e";

/// A project whose stream `main` holds the mixer, its `monitor`, and a top-level `GraphInput`
/// `volume`.
fn a_project_whose_main_stream_holds_a_mixer_and_a_graph_input() -> ProjectWithGraphDocuments {
    let project = a_project_whose_main_stream_holds_a_mixer();
    let mut graph = project.the_graph_document_of("main");
    graph["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": VOLUME_GRAPH_INPUT_NODE_UUID, "name": "volume", "type": "tatolab.stream:GraphInput",
        "config": { "port": "volume", "family": "channels" },
    }));
    project.write_the_graph_document(graph);
    project
}

/// The link from `source` to `target`, each `(node, port)`, in `stream`, as `check_link` takes it.
fn the_link(stream: Value, source: (&str, &str), target: (&str, &str)) -> Value {
    json!({
        "stream": stream,
        "source": { "node": source.0, "port": source.1 },
        "target": { "node": target.0, "port": target.1 },
    })
}

/// `link` as `connect` takes it, by the agent `alice`.
fn connected_by_alice(link: &Value) -> Value {
    let mut connect = link.clone();
    connect["author"] = an_agent_named("alice");
    connect
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// `check_link`'s answer for `link`; a refusal of `check_link` itself fails the test.
    async fn the_check_of(&self, link: &Value) -> Value {
        self.the_answer_or_the_refusal_of("check_link", link.clone())
            .await
            .unwrap_or_else(|refusal| panic!("check_link answers {link}: {refusal:?}"))
    }
}

/// Each link `connect` refuses, `check_link` answers `refused` with the very refusal `connect`
/// answers for it, and the graph document is left as it was by the check.
///
/// Mental-revert: a check that judged pin types but read a node or a stream its own way would
/// answer another code, place or words than `connect` for one of these links.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_answers_the_refusal_connect_answers_for_each_link_it_refuses() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer_and_a_graph_input();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));
    let links_connect_refuses = [
        (
            "pin types that do not connect",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (MIXER_NODE_UUID, "video"),
            ),
        ),
        (
            "a setting",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (VOLUME_GRAPH_INPUT_NODE_UUID, "value"),
            ),
        ),
        (
            "a typed output into an untyped input whose family refuses it",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (SOURCE_NODE_UUID, "video"),
            ),
        ),
        (
            "a port the node does not have",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (MIXER_NODE_UUID, "nothing"),
            ),
        ),
        (
            "a node the stream does not hold",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (NO_NODE_UUID, "gain"),
            ),
        ),
        (
            "a node named by no UUID",
            the_link(
                stream.clone(),
                ("mixer", "level"),
                (MIXER_NODE_UUID, "gain"),
            ),
        ),
        (
            "a stream the runtime does not hold",
            the_link(
                json!("no-such-stream-0a0a0a"),
                (MIXER_NODE_UUID, "level"),
                (MIXER_NODE_UUID, "gain"),
            ),
        ),
    ];
    let saved_before = project.the_graph_document_of("main");

    for (what, link) in links_connect_refuses {
        let checked = engine.the_check_of(&link).await;
        assert_eq!(
            project.the_graph_document_of("main"),
            saved_before,
            "{what}"
        );
        let refused = engine
            .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
            .await
            .expect_err(what);

        assert_eq!(checked["outcome"], "refused", "{what}: {checked}");
        assert_eq!(
            checked["refusal"],
            serde_json::to_value(&*refused).unwrap(),
            "{what}: check_link answers the refusal connect answers"
        );
        assert!(checked.get("convert_node").is_none(), "{what}: {checked}");
    }
}

/// A link `connect` makes, `check_link` answers `connects`, with no refusal, and leaves the live
/// graph, its document and the project's history as they were; once `connect` has made it, a
/// second check answers the refusal a second `connect` answers.
///
/// Mental-revert: a check applying the link and reversing it would leave a commit for `undo`.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_answers_a_link_connect_makes_and_changes_nothing() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let link = the_link(
        json!(project.stream_asset_of("main")),
        (MIXER_NODE_UUID, "level"),
        (MIXER_NODE_UUID, "gain"),
    );
    let saved_before = project.the_graph_document_of("main");

    let checked = engine.the_check_of(&link).await;

    assert_eq!(checked, json!({ "outcome": "connects" }));
    assert_eq!(project.the_graph_document_of("main"), saved_before);
    assert_eq!(
        engine.the_live_graph_of(&stream_id)["links"],
        json!([]),
        "the check makes no link"
    );
    let undo = json!({ "project_directory": project.path(), "author": an_agent_named("alice") });
    let nothing_to_undo = engine
        .the_answer_or_the_refusal_of("undo", undo)
        .await
        .expect_err("the check records nothing in history");
    assert!(
        nothing_to_undo
            .runtime_words
            .contains("no transaction left to undo"),
        "{nothing_to_undo:?}"
    );

    engine
        .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
        .await
        .expect("connect makes the link the check answered");
    let checked_again = engine.the_check_of(&link).await;
    let refused_again = engine
        .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
        .await
        .expect_err("the graph already holds the link");

    assert_eq!(checked_again["outcome"], "refused", "{checked_again}");
    assert_eq!(
        checked_again["refusal"],
        serde_json::to_value(&*refused_again).unwrap()
    );
}

/// A link inside a placed stream is refused unchecked, since the check judges a link at the
/// stream's own level alone.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_refuses_a_link_inside_a_placed_stream() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let mut link = the_link(
        json!(project.stream_asset_of("main")),
        (MIXER_NODE_UUID, "level"),
        (MIXER_NODE_UUID, "gain"),
    );
    link["level"] = json!([NO_NODE_UUID]);

    let refusal = engine
        .the_answer_or_the_refusal_of("check_link", link)
        .await
        .expect_err("a level inside a placed stream is not checked");

    assert_eq!(refusal.action, "check_link", "{refusal:?}");
    assert!(
        refusal.runtime_words.contains("not checked yet"),
        "{refusal:?}"
    );
}
