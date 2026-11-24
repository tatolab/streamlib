// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` between Texture pins is judged by every format the output states: a link made now
//! is refused when its input does not read one, and a saved graph's link is kept.

use super::*;

/// A Texture output stating a format the input does not take is refused naming the formats,
/// though another it states is one the input takes; an input stating none takes it.
///
/// Mental-revert: testing the formats for an overlap in the verdict connects the
/// rgba8_unorm-only sink.
#[test]
fn a_texture_output_is_refused_by_an_input_that_does_not_take_every_format_it_states() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(RGBA_OR_NV12_TEXTURE_SOURCE));
    let rgba_sink = a_node_of(&compiler, class_path(RGBA_TEXTURE_SINK));
    let any_sink = a_node_of(&compiler, class_path(ANY_TEXTURE_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source.clone(), "video"),
            InputLinkPortRef::new(rgba_sink, "video"),
        )
        .expect_err("the source may send nv12, which the sink does not take"),
    );
    let words = refusal.to_string();
    assert!(
        words.contains("(Texture (rgba8_unorm, nv12))")
            && words.contains("(Texture (rgba8_unorm))"),
        "{words}"
    );
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "video"),
        InputLinkPortRef::new(any_sink, "video"),
    )
    .expect("an input stating no format takes any");
    assert_eq!(the_links_in(&compiler), 1);
}

/// An output whose family follows an input sends the formats of what arrives there: an
/// rgba8_unorm or nv12 picture handed on reaches an input stating none, and not one taking
/// rgba8_unorm alone, either way round the two links are made.
///
/// Mental-revert: judging the formats by their overlap connects the rgba8_unorm-only sink.
#[test]
fn an_output_following_an_input_is_judged_by_the_formats_it_hands_on() {
    register_the_verdict_fixture_node_types();
    for (sink_type, connects) in [(ANY_TEXTURE_SINK, true), (RGBA_TEXTURE_SINK, false)] {
        for passthrough_feeds_first in [false, true] {
            let compiler = Arc::new(Compiler::new());
            let source = a_node_of(&compiler, class_path(RGBA_OR_NV12_TEXTURE_SOURCE));
            let passthrough = a_node_of(&compiler, class_path(TEXTURE_PASSTHROUGH));
            let sink = a_node_of(&compiler, class_path(sink_type));
            let into_the_passthrough = (
                OutputLinkPortRef::new(source, "video"),
                InputLinkPortRef::new(passthrough.clone(), "in"),
            );
            let out_of_the_passthrough = (
                OutputLinkPortRef::new(passthrough, "out"),
                InputLinkPortRef::new(sink, "video"),
            );
            let (first, second) = if passthrough_feeds_first {
                (out_of_the_passthrough, into_the_passthrough)
            } else {
                (into_the_passthrough, out_of_the_passthrough)
            };
            connect_on_this_thread(&compiler, first.0, first.1)
                .expect("the first link has nothing to judge the formats against");
            match (
                connect_on_this_thread(&compiler, second.0, second.1),
                connects,
            ) {
                (Ok(_), true) => {}
                (Err(Error::InvalidLink(words)), false) => {
                    assert!(words.contains("may write [\"nv12\"], which"), "{words}");
                }
                (outcome, _) => panic!(
                    "the handed-on picture into {sink_type}, passthrough feeding first: \
                     {passthrough_feeds_first}: {outcome:?}"
                ),
            }
        }
    }
}

/// A saved graph's Texture link whose output states a format the input does not take, besides
/// one it does, is kept, so that a saved document always opens; the same link made now is
/// refused.
///
/// Mental-revert: judging a saved link's formats as a new link's refuses it, and the saved graph
/// holding it no longer loads.
#[test]
fn a_saved_graphs_texture_link_its_formats_alone_refuse_is_kept_and_a_new_one_refused() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(RGBA_OR_NV12_TEXTURE_SOURCE));
    let saved_sink = a_node_of(&compiler, class_path(RGBA_TEXTURE_SINK));
    let new_sink = a_node_of(&compiler, class_path(RGBA_TEXTURE_SINK));

    connect_on_this_thread_as(
        &compiler,
        OutputLinkPortRef::new(source.clone(), "video"),
        InputLinkPortRef::new(saved_sink, "video"),
        ALinkConnected::OfASavedGraph,
    )
    .expect("a saved link whose formats alone refuse it is kept");
    assert_eq!(the_links_in(&compiler), 1);

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "video"),
        InputLinkPortRef::new(new_sink, "video"),
    )
    .expect_err("a link made now is judged by every format the output states");
    assert_eq!(the_links_in(&compiler), 1);
}

/// A saved graph's link into an untyped Image input, judged by the family check alone, is kept
/// when the input does not read every format the output states; made now it is refused.
///
/// Mental-revert: judging a saved link's formats on the family check as a new link's refuses it.
#[test]
fn a_saved_graphs_link_into_an_untyped_image_input_its_formats_alone_refuse_is_kept() {
    register_the_verdict_fixture_node_types();
    for (a_link_connected, kept) in [
        (ALinkConnected::OfASavedGraph, true),
        (ALinkConnected::MadeNow, false),
    ] {
        let compiler = Arc::new(Compiler::new());
        let source = a_node_of(&compiler, class_path(RGBA_OR_NV12_TEXTURE_SOURCE));
        let sink = a_node_of(&compiler, class_path(UNTYPED_RGBA_PICTURE_SINK));
        let connected = connect_on_this_thread_as(
            &compiler,
            OutputLinkPortRef::new(source, "video"),
            InputLinkPortRef::new(sink, "video"),
            a_link_connected,
        );
        match (connected, kept) {
            (Ok(_), true) => {}
            (Err(Error::InvalidLink(words)), false) => {
                assert!(words.contains("may write [\"nv12\"], which"), "{words}");
            }
            (outcome, _) => panic!("{a_link_connected:?}: {outcome:?}"),
        }
    }
}

/// A saved graph keeps a link whose Image formats alone are refused, and nothing else: a family
/// mismatch from an output stating formats, a missing required name and a codec outside the
/// required set are each refused as they are when the link is made now.
///
/// Mental-revert: keeping every link of a saved graph its families refuse keeps each of these.
#[test]
fn a_saved_graph_keeps_no_link_its_families_names_or_codec_refuse() {
    register_the_verdict_fixture_node_types();
    let keep = ALinkConnected::OfASavedGraph;
    let compiler = Arc::new(Compiler::new());

    let pictures = a_node_of(&compiler, class_path(RGBA_OR_NV12_TEXTURE_SOURCE));
    let numbers = a_node_of(&compiler, class_path(NUMBER_SINK));
    connect_on_this_thread_as(
        &compiler,
        OutputLinkPortRef::new(pictures, "video"),
        InputLinkPortRef::new(numbers, "count"),
        keep,
    )
    .expect_err("a picture stating formats never feeds an Integer, saved or not");

    let records = a_node_of(&compiler, class_path(NAMED_RECORDS_SOURCE));
    let needing_z = a_node_of(&compiler, class_path(RECORDS_NEEDING_Z));
    match connect_on_this_thread_as(
        &compiler,
        OutputLinkPortRef::new(records, "records"),
        InputLinkPortRef::new(needing_z, "records"),
        keep,
    ) {
        Err(Error::InvalidLink(words)) => assert!(words.contains("\"z\""), "{words}"),
        outcome => panic!("a missing required name refuses a saved link: {outcome:?}"),
    }

    let encoder = a_node_configured(
        &compiler,
        class_path(ENCODER_CODEC_FROM_CONFIG),
        serde_json::json!({ "codec": "h265" }),
    );
    let h264 = a_node_of(&compiler, class_path(H264_SINK));
    match connect_on_this_thread_as(
        &compiler,
        OutputLinkPortRef::new(encoder, "packets"),
        InputLinkPortRef::new(h264, "packets"),
        keep,
    ) {
        Err(Error::InvalidLink(words)) => assert!(words.contains("requires a codec in"), "{words}"),
        outcome => panic!("a codec outside the required set refuses a saved link: {outcome:?}"),
    }
    assert_eq!(the_links_in(&compiler), 0);
}

/// A config update on the source of a saved link kept despite its Image formats is taken: no
/// config sets the formats, so the wired link is not judged by them again.
///
/// Mental-revert: judging the wired links' formats at a config update refuses every update of
/// the source.
#[test]
fn a_config_update_on_the_source_of_a_link_kept_despite_its_formats_is_taken() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(RGBA_OR_NV12_TEXTURE_SOURCE));
    let sink = a_node_of(&compiler, class_path(RGBA_TEXTURE_SINK));
    connect_on_this_thread_as(
        &compiler,
        OutputLinkPortRef::new(source.clone(), "video"),
        InputLinkPortRef::new(sink, "video"),
        ALinkConnected::OfASavedGraph,
    )
    .expect("a saved link whose formats alone refuse it is kept");

    compiler.scope(|graph, _tx| {
        refuse_input_values_a_wired_link_out_of_the_node_would_refuse(
            graph,
            &source,
            serde_json::json!({ "label": "camera" })
                .as_object()
                .expect("an object of input values"),
        )
        .expect("a config update sets no format");
    });
}

/// A saved graph's link handed on past an output that follows its input is kept when the input
/// it reaches does not read every format, and refused when made now.
///
/// Mental-revert: judging what a saved link carries on as a new link's refuses it.
#[test]
fn a_saved_graphs_link_handed_on_to_an_input_not_reading_every_format_is_kept() {
    register_the_verdict_fixture_node_types();
    for (a_link_connected, kept) in [
        (ALinkConnected::OfASavedGraph, true),
        (ALinkConnected::MadeNow, false),
    ] {
        let compiler = Arc::new(Compiler::new());
        let source = a_node_of(&compiler, class_path(RGBA_OR_NV12_TEXTURE_SOURCE));
        let passthrough = a_node_of(&compiler, class_path(TEXTURE_PASSTHROUGH));
        let sink = a_node_of(&compiler, class_path(RGBA_TEXTURE_SINK));
        connect_on_this_thread_as(
            &compiler,
            OutputLinkPortRef::new(passthrough.clone(), "out"),
            InputLinkPortRef::new(sink, "video"),
            a_link_connected,
        )
        .expect("nothing arrives at the passthrough yet");
        let connected = connect_on_this_thread_as(
            &compiler,
            OutputLinkPortRef::new(source, "video"),
            InputLinkPortRef::new(passthrough, "in"),
            a_link_connected,
        );
        assert_eq!(
            connected.is_ok(),
            kept,
            "{a_link_connected:?}: {connected:?}"
        );
    }
}
