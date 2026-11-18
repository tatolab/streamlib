// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link_candidates` over a real engine answers, for each candidate, exactly what
//! `check_link` answers for its link with the placed end, from an output or from an input, over
//! the placed ports, the node types' ports or a list of ends, and changes nothing.

use streamlib::sdk::runtime::graph_renders_and_commits_counted_for_a_test::commits_begun_of;
use streamlib_project_repository::GRAPH_INPUT_NODE_TYPE;

use super::input_value_set_tests::{MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer};
use super::link_checked_before_it_is_made_tests::{
    FRAMES_TYPE, FRAMES_UUID, MIX_UUID, REMAP_UUID, SECONDS_TYPE, SECONDS_UUID,
    a_project_whose_main_stream_holds_function_nodes, the_link,
};
use super::*;

/// `check_link_candidates`' params: the placed end `from`, `{ direction: (node, port) }`, among
/// `among`, in `stream`.
fn the_candidates(
    stream: Value,
    direction: &str,
    from: (&str, &str),
    among: Value,
    include_refused: bool,
) -> Value {
    json!({
        "stream": stream,
        "from": { direction: { "node": from.0, "port": from.1 } },
        "among": among,
        "include_refused": include_refused,
    })
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// `check_link_candidates`' answer for `params`; a refusal fails the test.
    async fn the_candidates_checked(&self, params: Value) -> Value {
        self.the_answer_or_the_refusal_of("check_link_candidates", params.clone())
            .await
            .unwrap_or_else(|refusal| panic!("check_link_candidates answers {params}: {refusal:?}"))
    }

    /// `check_link`'s answer for the link between `held` and the placed `candidate`, each
    /// `(node, port)`, `held` an output when `from_an_output`.
    async fn the_check_link_answer(
        &self,
        stream: &Value,
        held: (&str, &str),
        candidate: (&str, &str),
        from_an_output: bool,
    ) -> Value {
        let (source, target) = if from_an_output {
            (held, candidate)
        } else {
            (candidate, held)
        };
        let link = the_link(stream.clone(), source, target);
        self.the_answer_or_the_refusal_of("check_link", link.clone())
            .await
            .unwrap_or_else(|refusal| panic!("check_link answers {link}: {refusal:?}"))
    }
}

/// The ends of `answer`'s candidates, each a placed port's `(node, port)`.
fn the_placed_ends_of(answer: &Value) -> Vec<(String, String)> {
    answer["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|candidate| {
            assert_eq!(candidate["end"]["kind"], "placed_port", "{candidate}");
            (
                candidate["end"]["node"].as_str().expect("node").to_owned(),
                candidate["end"]["port"].as_str().expect("port").to_owned(),
            )
        })
        .collect()
}

/// Every port of `direction` of every node the stream's live graph holds, `(node, port)`.
fn the_ports_of_the_live_graph(live_graph: &Value, direction: &str) -> Vec<(String, String)> {
    live_graph["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .flat_map(|node| {
            node["ports"][direction]
                .as_array()
                .expect("ports")
                .iter()
                .map(|port| {
                    (
                        node["id"].as_str().expect("id").to_owned(),
                        port["name"].as_str().expect("name").to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// From an output and from an input, among the placed ports and with the refused included,
/// each placed port of the other direction is a candidate, and each candidate's check is
/// exactly what `check_link` answers for its link; the graph document is left as it was.
///
/// Mental-revert: a candidate judged by a rule of its own, such as the pin types alone, would
/// answer `connects` where `check_link` answers the refusal of a setting or of a family.
#[tokio::test(flavor = "multi_thread")]
async fn each_placed_candidate_answers_what_check_link_answers_from_either_direction() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));
    let saved_before = project.the_graph_document_of("main");
    let live_graph = engine.the_live_graph_of(&stream_id);

    for (direction, held, candidate_direction) in [
        ("output", (MIXER_NODE_UUID, "level"), "inputs"),
        ("input", (MIXER_NODE_UUID, "gain"), "outputs"),
    ] {
        let answer = engine
            .the_candidates_checked(the_candidates(
                stream.clone(),
                direction,
                held,
                json!("placed_ports"),
                true,
            ))
            .await;

        assert_eq!(
            the_placed_ends_of(&answer),
            the_ports_of_the_live_graph(&live_graph, candidate_direction),
            "{direction}: {answer}"
        );
        for candidate in answer["candidates"].as_array().expect("candidates") {
            let checked = engine
                .the_check_link_answer(
                    &stream,
                    held,
                    (
                        candidate["end"]["node"].as_str().expect("node"),
                        candidate["end"]["port"].as_str().expect("port"),
                    ),
                    direction == "output",
                )
                .await;
            assert_eq!(candidate["check"], checked, "{direction}: {candidate}");
        }
        assert!(answer.get("not_checked").is_none(), "{answer}");
    }
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// Without `include_refused`, a candidate `connect` would refuse is left out and the rest are
/// kept: a Float output connects into the mixer's own Float input alone of the placed inputs.
///
/// Mental-revert: a filter keeping every candidate would list the monitor's input, which a
/// Float output does not feed.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_candidate_is_left_out_unless_the_refused_are_asked_for() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let answer = engine
        .the_candidates_checked(the_candidates(
            json!(project.stream_asset_of("main")),
            "output",
            (MIXER_NODE_UUID, "level"),
            json!("placed_ports"),
            false,
        ))
        .await;

    assert_eq!(
        answer,
        json!({ "candidates": [{
            "end": { "kind": "placed_port", "node": MIXER_NODE_UUID, "port": "gain" },
            "check": { "outcome": "connects" },
        }] })
    );
}

/// Among the node types' ports, a Float output learns that a node type's Integer input takes
/// it for free, warning that the meanings differ, exactly as `check_link` answers for a placed
/// node of that type; no node is left placed and the graph document is left as it was.
///
/// Mental-revert: judging a node type's port by its family alone would answer `connects` with
/// no warning; leaving the node held for the judgement would add a node to the live graph.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_types_port_answers_what_check_link_answers_for_a_placed_node_of_the_type() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));
    let saved_before = project.the_graph_document_of("main");
    let nodes_before = engine.the_live_graph_of(&stream_id)["nodes"]
        .as_array()
        .map(Vec::len);

    let answer = engine
        .the_candidates_checked(the_candidates(
            stream.clone(),
            "output",
            (SECONDS_UUID, "elapsed"),
            json!("node_type_ports"),
            false,
        ))
        .await;

    let frames = answer["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|candidate| candidate["end"]["node_type"] == FRAMES_TYPE)
        .unwrap_or_else(|| panic!("the Integer input of {FRAMES_TYPE} is a candidate: {answer}"));
    assert_eq!(
        frames["end"],
        json!({ "kind": "node_type_port", "node_type": FRAMES_TYPE, "port": "count" })
    );
    let placed_frames = engine
        .the_check_link_answer(
            &stream,
            (SECONDS_UUID, "elapsed"),
            (FRAMES_UUID, "count"),
            true,
        )
        .await;
    assert_eq!(frames["check"], placed_frames);
    assert!(
        answer["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .all(|candidate| candidate["check"]["outcome"] != "refused"),
        "{answer}"
    );
    assert_eq!(
        engine.the_live_graph_of(&stream_id)["nodes"]
            .as_array()
            .map(Vec::len),
        nodes_before,
        "no node is left placed"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// From an input, among an explicit list, a node type's output and a placed output are each
/// judged into it: a node type's Float output into a placed Integer input connects for free.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_types_output_is_judged_into_a_placed_input_among_a_list_of_ends() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));

    let answer = engine
        .the_candidates_checked(the_candidates(
            stream.clone(),
            "input",
            (FRAMES_UUID, "count"),
            json!({ "ends": [
                { "kind": "node_type_port", "node_type": SECONDS_TYPE, "port": "elapsed" },
                { "kind": "placed_port", "node": SECONDS_UUID, "port": "elapsed" },
            ] }),
            true,
        ))
        .await;

    let placed = engine
        .the_check_link_answer(
            &stream,
            (FRAMES_UUID, "count"),
            (SECONDS_UUID, "elapsed"),
            false,
        )
        .await;
    assert_eq!(answer["candidates"][0]["check"], placed, "{answer}");
    assert_eq!(answer["candidates"][1]["check"], placed, "{answer}");
}

/// The node types `stream_id`'s catalog lists, by import path.
fn the_node_catalog_of(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
) -> Vec<String> {
    engine
        .operations_on_the_loaded_streams()
        .node_catalog_of_the_stream_a_call_names(stream_id)
        .expect("the stream is loaded")
        .into_iter()
        .map(|node_type| node_type.node_class_import_path.as_str().to_owned())
        .collect()
}

/// A port its node type does not have is answered `refused` with `port_not_found`, naming the
/// node type.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_types_port_that_is_no_port_is_answered_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let answer = engine
        .the_candidates_checked(the_candidates(
            json!(project.stream_asset_of("main")),
            "output",
            (SECONDS_UUID, "elapsed"),
            json!({ "ends": [
                { "kind": "node_type_port", "node_type": FRAMES_TYPE, "port": "nothing" },
            ] }),
            true,
        ))
        .await;

    let refused = &answer["candidates"][0]["check"];
    assert_eq!(refused["outcome"], "refused", "{answer}");
    assert_eq!(refused["refusal"]["action"], "connect", "{answer}");
    assert_eq!(
        refused["refusal"]["refusal_code"], "port_not_found",
        "{answer}"
    );
    assert!(
        refused["refusal"]["runtime_words"]
            .as_str()
            .is_some_and(|words| words.contains(FRAMES_TYPE)),
        "{answer}"
    );
}

/// A node type the stream has not described yet is listed under `not_checked`, saying to place
/// a node of it first; it is not described, so the stream's node catalog is left as it was.
///
/// Mental-revert: describing the type, as an `add_node` of it would, registers it into the
/// stream, so the catalog afterwards lists one more type.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_type_not_described_yet_is_not_checked_and_not_described() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let catalog_before = the_node_catalog_of(&engine, &stream_id);
    let not_described = json!({
        "kind": "node_type_port", "node_type": "link_check_test:NotDescribedYet", "port": "x",
    });

    let answer = engine
        .the_candidates_checked(the_candidates(
            json!(project.stream_asset_of("main")),
            "output",
            (SECONDS_UUID, "elapsed"),
            json!({ "ends": [not_described] }),
            true,
        ))
        .await;

    assert!(
        answer
            .get("candidates")
            .is_none_or(|candidates| candidates == &json!([])),
        "{answer}"
    );
    assert_eq!(answer["not_checked"][0]["end"], not_described, "{answer}");
    assert!(
        answer["not_checked"][0]["why"]
            .as_str()
            .is_some_and(|why| why.contains("not described yet") && why.contains("`add_node`")),
        "{answer}"
    );
    assert_eq!(the_node_catalog_of(&engine, &stream_id), catalog_before);
}

/// A graph interface's or a fragment's node type, as a candidate, is listed under `not_checked`
/// with the reason `check_link` gives for a link reaching such a placed node.
///
/// Mental-revert: placing a node of the type for the judgement would judge a link the live
/// graph judges only as it is reconciled, answering where `add_node` and `connect` would not.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_interface_or_fragment_node_type_is_not_checked_as_check_link_does_not() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let ends: Vec<Value> = [
        GRAPH_INPUT_NODE_TYPE,
        GRAPH_OUTPUT_NODE_TYPE,
        STREAM_FRAGMENT_NODE_TYPE,
    ]
    .into_iter()
    .map(|node_type| json!({ "kind": "node_type_port", "node_type": node_type, "port": "value" }))
    .collect();

    let answer = engine
        .the_candidates_checked(the_candidates(
            json!(project.stream_asset_of("main")),
            "output",
            (SECONDS_UUID, "elapsed"),
            json!({ "ends": ends }),
            true,
        ))
        .await;

    let not_checked = answer["not_checked"].as_array().expect("not_checked");
    assert_eq!(
        not_checked
            .iter()
            .map(|it| it["end"].clone())
            .collect::<Vec<_>>(),
        ends,
        "{answer}"
    );
    for it in not_checked {
        assert_eq!(
            it["why"],
            "the link reaches a node that runs as no node of its own, such as a fragment",
            "{answer}"
        );
    }
}

/// Placed ports and node types' ports interleaved are each answered in the order sent, and a
/// node type's port answers what the placed node of that type answers.
///
/// Mental-revert: answering the placed candidates first and the node types' after would list
/// them out of the order sent.
#[tokio::test(flavor = "multi_thread")]
async fn placed_and_node_types_candidates_interleaved_are_answered_in_the_order_sent() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let ends = vec![
        json!({ "kind": "node_type_port", "node_type": FRAMES_TYPE, "port": "count" }),
        json!({ "kind": "placed_port", "node": FRAMES_UUID, "port": "count" }),
        json!({ "kind": "node_type_port", "node_type": FRAMES_TYPE, "port": "nothing" }),
        json!({ "kind": "placed_port", "node": MIX_UUID, "port": "b" }),
    ];

    let answer = engine
        .the_candidates_checked(the_candidates(
            json!(project.stream_asset_of("main")),
            "output",
            (SECONDS_UUID, "elapsed"),
            json!({ "ends": ends }),
            true,
        ))
        .await;

    let candidates = answer["candidates"].as_array().expect("candidates");
    assert_eq!(
        candidates
            .iter()
            .map(|it| it["end"].clone())
            .collect::<Vec<_>>(),
        ends,
        "{answer}"
    );
    assert_eq!(candidates[0]["check"], candidates[1]["check"], "{answer}");
    assert_eq!(candidates[2]["check"]["outcome"], "refused", "{answer}");
}

/// A `from` naming a node the graph does not hold, or an input as an output, refuses the call
/// itself, coded, before any candidate is judged.
///
/// Mental-revert: judging each candidate against a `from` never resolved answers every
/// candidate refused, or a node type's port judged against no end at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_from_naming_no_node_or_a_port_of_the_other_direction_refuses_the_call() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));

    for (from, code) in [
        (
            ("6f6f6f6f-6f6f-4f6f-8f6f-6f6f6f6f6f6e", "elapsed"),
            "node_not_found",
        ),
        ((FRAMES_UUID, "count"), "port_not_found"),
    ] {
        let refused = engine
            .the_answer_or_the_refusal_of(
                "check_link_candidates",
                the_candidates(
                    stream.clone(),
                    "output",
                    from,
                    json!("node_type_ports"),
                    true,
                ),
            )
            .await
            .expect_err("the call is refused");

        assert_eq!(refused.action, "check_link_candidates", "{refused:?}");
        assert_eq!(
            serde_json::to_value(refused.refusal_code).unwrap(),
            json!(code),
            "{refused:?}"
        );
    }
}

/// On a playing stream, judging every node type's port changes nothing a commit reads: no
/// commit begins, the live graph holds as many nodes as before, and the stream still runs.
///
/// Mental-revert: a node held for a judgement logged as a pending operation, or left placed,
/// begins a commit and leaves one node more in the live graph.
#[tokio::test(flavor = "multi_thread")]
async fn node_types_candidates_on_a_playing_stream_begin_no_commit_and_leave_it_running() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let (stream_id, served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    let loaded_stream = engine
        .operations_on_the_loaded_streams()
        .loaded_stream_identity_of_the_stream_a_call_names(&stream_id)
        .expect("the stream is loaded");
    let nodes_in = |graph: Value| graph["nodes"].as_array().map(Vec::len);
    let nodes_before = nodes_in(engine.the_live_graph_of(&stream_id));
    let commits_begun_before = commits_begun_of(&loaded_stream);

    let answer = served
        .control_call(
            "check_link_candidates",
            the_candidates(
                json!(project.stream_asset_of("main")),
                "output",
                (MIXER_NODE_UUID, "level"),
                json!("node_type_ports"),
                true,
            ),
        )
        .await;

    assert!(
        answer["result"]["candidates"]
            .as_array()
            .is_some_and(|candidates| !candidates.is_empty()),
        "{answer}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        commits_begun_of(&loaded_stream),
        commits_begun_before,
        "a judgement began a commit"
    );
    assert_eq!(nodes_in(engine.the_live_graph_of(&stream_id)), nodes_before);
    let listed = served.control_call("list_streams", json!({})).await;
    let main = listed["result"]["streams"]
        .as_array()
        .and_then(|streams| streams.iter().find(|listed| listed["stream"] == stream_id))
        .unwrap_or_else(|| panic!("the stream is listed: {listed}"));
    assert_eq!(main["state"], "running", "{listed}");
}

/// A candidate whose link `check_link` would not check yet, one `connect` closes through a
/// `PreviousValue` node, is listed under `not_checked` with why, beside the candidates checked.
///
/// Mental-revert: failing the whole call, as `check_link` fails on such a link, would answer
/// nothing for the candidates it does check.
#[tokio::test(flavor = "multi_thread")]
async fn a_candidate_not_checked_yet_is_listed_with_why_beside_those_checked() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let answer = engine
        .the_candidates_checked(the_candidates(
            json!(project.stream_asset_of("main")),
            "output",
            (REMAP_UUID, "result"),
            json!({ "ends": [
                { "kind": "placed_port", "node": MIX_UUID, "port": "b" },
                { "kind": "placed_port", "node": FRAMES_UUID, "port": "count" },
            ] }),
            true,
        ))
        .await;

    assert_eq!(
        answer["not_checked"][0]["end"],
        json!({ "kind": "placed_port", "node": MIX_UUID, "port": "b" }),
        "{answer}"
    );
    assert!(
        answer["not_checked"][0]["why"]
            .as_str()
            .is_some_and(|why| why.contains("`PreviousValue` node")),
        "{answer}"
    );
    assert_eq!(
        answer["candidates"][0]["end"],
        json!({ "kind": "placed_port", "node": FRAMES_UUID, "port": "count" }),
        "{answer}"
    );
}

/// A non-empty `level` and a stream the runtime does not hold refuse the call itself: the first
/// unchecked, the second with `stream_not_found`.
#[tokio::test(flavor = "multi_thread")]
async fn a_level_inside_a_placed_stream_or_a_stream_not_held_refuses_the_call() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let mut inside_a_placed_stream = the_candidates(
        json!(project.stream_asset_of("main")),
        "output",
        (MIXER_NODE_UUID, "level"),
        json!("placed_ports"),
        false,
    );
    inside_a_placed_stream["level"] = json!([MIXER_NODE_UUID]);

    let unchecked = engine
        .the_answer_or_the_refusal_of("check_link_candidates", inside_a_placed_stream)
        .await
        .expect_err("a level inside a placed stream is not checked");
    let not_held = engine
        .the_answer_or_the_refusal_of(
            "check_link_candidates",
            the_candidates(
                json!("no-such-stream-0a0a0a"),
                "output",
                (MIXER_NODE_UUID, "level"),
                json!("placed_ports"),
                false,
            ),
        )
        .await
        .expect_err("a stream the runtime does not hold");

    assert_eq!(unchecked.action, "check_link_candidates", "{unchecked:?}");
    assert!(
        unchecked.runtime_words.contains("not checked yet"),
        "{unchecked:?}"
    );
    assert_eq!(not_held.action, "check_link_candidates", "{not_held:?}");
    assert_eq!(
        serde_json::to_value(not_held.refusal_code).unwrap(),
        json!("stream_not_found"),
        "{not_held:?}"
    );
}
