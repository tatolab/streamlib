// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` over a real engine judges two typed pins by their verdict: a link the verdict
//! refuses is refused with `link_pin_types_do_not_connect`, and a link into a setting with
//! `input_takes_no_wire`, each in the runtime's words and before anything is applied.

use super::input_value_set_tests::{MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer};
use super::*;

use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

/// The `connect` arguments linking the mixer's output `output` into its input `input`.
fn a_link_from_the_mixer_into_itself(stream_asset: Uuid, output: &str, input: &str) -> Value {
    json!({
        "stream": stream_asset, "author": an_agent_named("alice"),
        "source": { "node": MIXER_NODE_UUID, "port": output },
        "target": { "node": MIXER_NODE_UUID, "port": input },
    })
}

/// A Float output linked into a Texture input is refused, naming both types, and the saved
/// graph is left as it was.
///
/// Mental-revert: judging the two typed pins by their families answers `refused` with the
/// family check's words.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_between_typed_pins_that_do_not_connect_is_refused_with_its_code() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let saved_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            a_link_from_the_mixer_into_itself(stream_asset, "level", "video"),
        )
        .await
        .expect_err("a Float does not connect to a Texture");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert!(
        refusal
            .runtime_words
            .contains("`mixer.level` (Float) to `mixer.video` (Texture)"),
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// The UUID of the top-level `GraphInput` `volume` beside the mixer.
const VOLUME_GRAPH_INPUT_NODE_UUID: &str = "70707070-7070-4070-8070-707070707070";

/// A link into the `value` of a top-level `GraphInput`, a setting, is refused as taking no
/// wire, naming the input.
///
/// Mental-revert: refusing the link in words alone answers `refused`.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_into_a_setting_is_refused_as_taking_no_wire() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let mut graph = project.the_graph_document_of("main");
    graph["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": VOLUME_GRAPH_INPUT_NODE_UUID, "name": "volume", "type": "tatolab.stream:GraphInput",
        "config": { "port": "volume", "family": "channels" },
    }));
    project.write_the_graph_document(graph);
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            json!({
                "stream": stream_asset, "author": an_agent_named("alice"),
                "source": { "node": MIXER_NODE_UUID, "port": "level" },
                "target": { "node": VOLUME_GRAPH_INPUT_NODE_UUID, "port": "value" },
            }),
        )
        .await
        .expect_err("a setting takes no wire");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputTakesNoWire,
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_field.as_deref(),
        Some("value"),
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_node.as_deref(),
        Some(VOLUME_GRAPH_INPUT_NODE_UUID),
        "the node as the caller named it: {refusal:?}"
    );
    assert!(refusal.runtime_words.contains("`volume`"), "{refusal:?}");
}

/// The type of the `GraphInput` placed inside a stream to hold a setting there.
const GRAPH_INPUT_NODE_TYPE: &str = "tatolab.stream:GraphInput";

/// The mixer of a project's `main`, collapsed into the placed stream `sub`, beside a
/// `GraphInput` `volume` added inside `sub`: the placing node's UUID, and the mixer's and
/// `volume`'s UUIDs as `main` draws them inside it.
pub(super) struct TheMixerInsideAPlacedStream {
    pub(super) subgraph: String,
    pub(super) mixer: String,
    pub(super) volume: String,
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// The mixer of `project`'s loaded `main` collapsed by `alice` into the placed stream
    /// `sub`, with a `GraphInput` `volume` added beside it inside `sub`.
    pub(super) async fn the_mixer_collapsed_into_a_placed_stream(
        &self,
        project: &ProjectWithGraphDocuments,
    ) -> TheMixerInsideAPlacedStream {
        let stream_asset = project.stream_asset_of("main");
        let collapsed = self
            .the_answer_or_the_refusal_of(
                "collapse_selection_into_subgraph",
                json!({ "stream": stream_asset, "nodes": [MIXER_NODE_UUID], "name": "sub",
                        "author": an_agent_named("alice") }),
            )
            .await
            .expect("the mixer collapses into `sub`");
        let subgraph = collapsed["added_nodes"][0]["node"]
            .as_str()
            .expect("the placing node")
            .to_owned();
        self.the_answer_or_the_refusal_of(
            "add_node",
            json!({ "stream": stream_asset, "level": [subgraph], "type": GRAPH_INPUT_NODE_TYPE,
                    "name": "volume", "config": { "port": "volume", "family": "channels" },
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("a `GraphInput` is added inside `sub`");
        let sub = project.the_graph_document_of("sub");
        let drawn = |name: &str| {
            the_uuid_a_held_node_takes_under(
                &subgraph,
                the_node_named(&sub, name)["id"]
                    .as_str()
                    .expect("the node's UUID"),
            )
        };
        TheMixerInsideAPlacedStream {
            mixer: drawn("mixer"),
            volume: drawn("volume"),
            subgraph,
        }
    }
}

/// A link inside a placed stream is judged by its pin types as the same link at the stream's
/// own level is: a Float into a Texture is refused with `link_pin_types_do_not_connect`, and a
/// link into a setting with `input_takes_no_wire`, each leaving the placed stream's document as
/// it was; a link the verdict connects is saved in it.
///
/// Mental-revert: saving a link inside a placed stream unjudged saves the two refused links.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_inside_a_placed_stream_is_judged_by_its_pin_types() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let inside = engine
        .the_mixer_collapsed_into_a_placed_stream(&project)
        .await;
    let link_inside = |source: (&str, &str), target: (&str, &str)| {
        json!({
            "stream": stream_asset, "level": [inside.subgraph], "author": an_agent_named("alice"),
            "source": { "node": source.0, "port": source.1 },
            "target": { "node": target.0, "port": target.1 },
        })
    };
    let sub_before = project.the_graph_document_of("sub");

    let pin_types_refused = engine
        .the_answer_or_the_refusal_of(
            "connect",
            link_inside((&inside.mixer, "level"), (&inside.mixer, "video")),
        )
        .await
        .expect_err("a Float does not connect to a Texture inside a placed stream");
    let setting_refused = engine
        .the_answer_or_the_refusal_of(
            "connect",
            link_inside((&inside.mixer, "level"), (&inside.volume, "value")),
        )
        .await
        .expect_err("a setting inside a placed stream takes no wire");

    assert_eq!(
        pin_types_refused.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{pin_types_refused:?}"
    );
    assert!(
        pin_types_refused
            .runtime_words
            .contains("`mixer.level` (Float) to `mixer.video` (Texture)"),
        "{pin_types_refused:?}"
    );
    assert_eq!(
        setting_refused.refusal_code,
        ControlOperationRefusalCode::InputTakesNoWire,
        "{setting_refused:?}"
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before);

    engine
        .the_answer_or_the_refusal_of(
            "connect",
            link_inside((&inside.mixer, "level"), (&inside.mixer, "gain")),
        )
        .await
        .expect("a Float into a Float connects inside a placed stream");
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("sub")),
        ["mixer/level -> mixer/gain"]
    );
}

/// A link inside a placed stream reaching a `GraphOutput` that names no family, which runs as
/// no node of its own, is saved without being judged, and its entry under `connected_links`
/// says why.
///
/// Mental-revert: discarding what the judgement of a link at a placed level answers leaves the
/// entry with no `not_judged_because`.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_inside_a_placed_stream_saved_unjudged_says_why() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let inside = engine
        .the_mixer_collapsed_into_a_placed_stream(&project)
        .await;
    let added = engine
        .the_answer_or_the_refusal_of(
            "add_node",
            json!({ "stream": stream_asset, "level": [inside.subgraph],
                    "type": "tatolab.stream:GraphOutput", "name": "level-out",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("a `GraphOutput` naming no family is added inside `sub`");
    let output = added["added_nodes"][0]["node"]
        .as_str()
        .expect("the output's UUID")
        .to_owned();

    let connected = engine
        .the_answer_or_the_refusal_of(
            "connect",
            json!({
                "stream": stream_asset, "level": [inside.subgraph],
                "author": an_agent_named("alice"),
                "source": { "node": inside.mixer, "port": "level" },
                "target": { "node": output, "port": "value" },
            }),
        )
        .await
        .expect("a link into a `GraphOutput` naming no family is saved");

    let why = connected["connected_links"][0]["not_judged_because"]
        .as_str()
        .unwrap_or_else(|| panic!("the link says why it was not judged: {connected}"));
    assert!(why.contains("names no family"), "{why}");
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("sub")),
        ["mixer/level -> level-out/value"]
    );
}
