// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An input set through `transact` while its stream plays restarts nothing: the node holding a
//! stub exclusive device ran its setup once and no teardown, opened the device once, the stream
//! still runs, and its graph document equals its running graph.

use super::node_counting_its_builds::{
    NodeCountingItsBuilds, THE_COUNTING_NODES_LEVEL, register_the_node_counting_its_builds_once,
    the_builds_counted_in,
};
use super::*;

const COUNTING_NODE_UUID: &str = "6e6e6e6e-6e6e-4e6e-8e6e-6e6e6e6e6e6e";

/// A project whose stream `main` holds one [`NodeCountingItsBuilds`] named `counting`, its
/// `level` 0.25.
fn a_project_whose_main_stream_holds_a_node_counting_its_builds() -> ProjectWithGraphDocuments {
    register_the_node_counting_its_builds_once();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": COUNTING_NODE_UUID, "name": "counting",
              "type": NodeCountingItsBuilds::node_class_import_path().as_str(),
              "input_values": { THE_COUNTING_NODES_LEVEL: 0.25 } },
        ],
        "links": [],
    }))
}

/// Mental-revert: an input set that rebuilds its node runs a second setup and a teardown, and
/// opens the device twice; one held as "not live" leaves the document holding a value the
/// running graph does not.
#[tokio::test(flavor = "multi_thread")]
async fn an_input_set_through_transact_while_playing_restarts_nothing() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_node_counting_its_builds();
    let (stream_id, _served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;

    let answer = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "set_node_input_value", "stream": project.stream_asset_of("main"),
                  "node": COUNTING_NODE_UUID, "input": THE_COUNTING_NODES_LEVEL, "value": 0.75 },
            ] }),
        )
        .await
        .expect("the level is set");
    assert!(
        answer.get("nodes_rebuilt").is_none(),
        "no node is rebuilt: {answer}"
    );

    engine
        .assert_the_stream_restarted_nothing(&project, &stream_id)
        .await;
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// Assert the [`NodeCountingItsBuilds`] of `stream_id` set up once and never tore down,
    /// opening its device once, that the stream runs, and that `project`'s graph document of
    /// `main` equals its running graph.
    async fn assert_the_stream_restarted_nothing(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_id: &str,
    ) {
        let counted = the_builds_counted_in(stream_id, |counted_in_the_stream| {
            (
                counted_in_the_stream.setups(),
                counted_in_the_stream.teardowns(),
                counted_in_the_stream.device().opens(),
            )
        });
        assert_eq!(counted, (1, 0, 1), "setups, teardowns and device opens");
        let listing = the_listing_of(&self.operations_on_the_loaded_streams(), stream_id).await;
        assert_eq!(listing["state"], "running", "{listing}");
        assert_eq!(listing["condition"]["health"], "healthy", "{listing}");
        assert_the_graph_document_equals_the_running_graph(
            &project.the_graph_document_of("main"),
            &self.the_live_graph_of(stream_id),
        );
    }
}
