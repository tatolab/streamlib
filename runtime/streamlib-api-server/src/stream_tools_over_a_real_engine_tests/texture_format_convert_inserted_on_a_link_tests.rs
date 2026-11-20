// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Two Textures of different formats connect through a `TextureFormatConvert` `connect` places in
//! the same edit, and one `undo` removes it; a pair of formats no conversion covers is refused
//! naming both.

use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::convert_node_inserted_on_a_link_tests::{
    CAMERA_NODE_UUID, MODEL_NODE_UUID, a_project_whose_main_stream_holds_a_camera_and_a_model,
    camera_into_model, connected_by_alice, link, the_inserted_node_in, the_links_in,
};
use super::*;

/// A `bgra8_unorm` Texture linked into an input that takes only `rgba8_unorm` gets a collapsed
/// `TextureFormatConvert` and both its links in one edit, holding no setting, named in the
/// answer; one `undo` removes the node and both links.
///
/// Mental-revert: without `TextureFormatConvert` registered, the link is refused with
/// `link_pin_types_do_not_connect` and nothing is added.
#[tokio::test(flavor = "multi_thread")]
async fn two_textures_of_different_formats_connect_through_a_texture_format_convert() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let saved_before = project.the_graph_document_of("main");

    let applied = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&camera_into_model(
                project.stream_asset_of("main"),
                "bgra_video_out",
                "rgba_video",
            )),
        )
        .await
        .unwrap_or_else(|refusal| panic!("the link connects through a node: {refusal:?}"));

    let saved = project.the_graph_document_of("main");
    let inserted = the_inserted_node_in(&saved);
    let inserted_uuid = inserted["id"].as_str().expect("a UUID").to_owned();
    assert_eq!(
        inserted["type"], "tatolab.stream:TextureFormatConvert",
        "{saved}"
    );
    assert_eq!(inserted["studio"]["collapsed"], true, "{saved}");
    assert!(
        inserted["input_values"]
            .as_object()
            .is_none_or(|input_values| input_values.is_empty()),
        "{saved}"
    );
    assert_eq!(
        the_links_in(&saved),
        [
            link(
                (CAMERA_NODE_UUID, "bgra_video_out"),
                (&inserted_uuid, "image")
            ),
            link(
                (&inserted_uuid, "rgba_image"),
                (MODEL_NODE_UUID, "rgba_video")
            ),
        ]
    );
    let added_nodes = applied["added_nodes"].as_array().expect("added_nodes");
    assert_eq!(added_nodes.len(), 1, "{applied}");
    assert_eq!(
        added_nodes[0]["node_type"], "tatolab.stream:TextureFormatConvert",
        "{applied}"
    );
    assert_eq!(
        added_nodes[0]["inserted_because"], "converts_pin_types",
        "{applied}"
    );

    engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the undo applies");
    let saved_after = project.the_graph_document_of("main");
    assert_eq!(
        sorted_node_names_in(&saved_after),
        sorted_node_names_in(&saved_before)
    );
    assert_eq!(the_links_in(&saved_after), [], "{saved_after}");
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        Vec::<String>::new()
    );
}

/// A Texture linked into one of a format no conversion writes from it is refused with
/// `link_pin_types_do_not_connect`, its words naming both formats, and nothing is added.
///
/// Mental-revert: a `TextureFormatConvert` declaring `rgba16_float` on its input is placed.
#[tokio::test(flavor = "multi_thread")]
async fn two_textures_whose_formats_no_conversion_covers_are_refused_naming_both() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_camera_and_a_model();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let saved_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&camera_into_model(
                project.stream_asset_of("main"),
                "half_float_video",
                "bgra_video",
            )),
        )
        .await
        .expect_err("no conversion writes bgra8_unorm from rgba16_float");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    for format in ["rgba16_float", "bgra8_unorm"] {
        assert!(
            refusal.runtime_words.contains(format),
            "{}",
            refusal.runtime_words
        );
    }
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}
