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
}
