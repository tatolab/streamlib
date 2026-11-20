// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A bundle or a List holding a Texture travels as Bundle frames, so a link from it into an
//! input that states no pin type, as a relay's send node's `content` does, is refused naming
//! each member that makes it one; a bundle of small members links there as before.

use super::*;
use crate::core::descriptors::BundleMember;

const FRAMED_PICTURE_SOURCE: &str = "VerdictCheckFramedPictureSource";
const FRAMED_PICTURE_SINK: &str = "VerdictCheckFramedPictureSink";
const PICTURES_SOURCE: &str = "VerdictCheckPicturesSource";
const SMALL_READING_SOURCE: &str = "VerdictCheckSmallReadingSource";
const UNTYPED_SEND_CONTENT_SINK: &str = "VerdictCheckUntypedSendContentSink";

fn a_member(name: &str, member_type: PinType) -> BundleMember {
    BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value: None,
    }
}

fn a_texture() -> PinType {
    PinType::Texture {
        pixel_formats: Vec::new(),
    }
}

/// A picture with the region of it that matters.
fn a_framed_picture() -> PinType {
    PinType::Bundle {
        members: vec![
            a_member("picture", a_texture()),
            a_member("region", PinType::Rectangle {}),
        ],
    }
}

fn register_the_bundle_frame_fixture_node_types() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    register_the_verdict_fixture_node_types();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register(a_node_type(
            class_path(FRAMED_PICTURE_SOURCE),
            Vec::new(),
            vec![a_typed_port("framed", a_framed_picture())],
        ));
        register(a_node_type(
            class_path(FRAMED_PICTURE_SINK),
            vec![a_typed_port("framed", a_framed_picture())],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(PICTURES_SOURCE),
            Vec::new(),
            vec![a_typed_port(
                "pictures",
                PinType::ListOf {
                    element_type: Box::new(a_texture()),
                },
            )],
        ));
        register(a_node_type(
            class_path(SMALL_READING_SOURCE),
            Vec::new(),
            vec![a_typed_port(
                "reading",
                PinType::Bundle {
                    members: vec![
                        a_member("level", float()),
                        a_member("label", PinType::String {}),
                    ],
                },
            )],
        ));
        register(a_node_type(
            class_path(UNTYPED_SEND_CONTENT_SINK),
            vec![PortDescriptor::new(
                "content",
                "content",
                true,
                PortFamilyDeclaration {
                    also_takes_families: vec![
                        PortFamily::Records,
                        PortFamily::Channels,
                        PortFamily::Array,
                    ],
                    ..PortFamilyDeclaration::of_family(PortFamily::Packets)
                },
            )],
            Vec::new(),
        ));
    });
}

/// Mental-revert: without the check, the link is judged by its families alone, and the
/// bundle's Records family is one the input also takes, so it connects.
#[test]
fn a_bundle_holding_a_texture_into_an_untyped_input_is_refused_naming_the_texture() {
    register_the_bundle_frame_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FRAMED_PICTURE_SOURCE));
    let sink = a_node_of(&compiler, class_path(UNTYPED_SEND_CONTENT_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "framed"),
            InputLinkPortRef::new(sink, "content"),
        )
        .expect_err("an input that states no pin type does not take a Bundle frame apart"),
    );

    assert!(
        matches!(
            refusal,
            PinLinkRefusal::BundleFrameIntoAnUntypedInput { .. }
        ),
        "{refusal:?}"
    );
    let words = refusal.to_string();
    assert!(words.contains("member `picture` is a Texture"), "{words}");
    assert!(words.contains(".content`"), "{words}");
    assert!(
        !words.contains("`region`"),
        "only a heavy member is named: {words}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}

#[test]
fn a_list_of_textures_into_an_untyped_input_is_refused_the_same_way() {
    register_the_bundle_frame_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(PICTURES_SOURCE));
    let sink = a_node_of(&compiler, class_path(UNTYPED_SEND_CONTENT_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "pictures"),
            InputLinkPortRef::new(sink, "content"),
        )
        .expect_err("a List of Textures travels as a Bundle frame"),
    );

    let words = refusal.to_string();
    assert!(words.contains("each element is a Texture"), "{words}");
    assert_eq!(the_links_in(&compiler), 0);
}

#[test]
fn a_bundle_of_small_members_into_an_untyped_input_connects_as_before() {
    register_the_bundle_frame_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(SMALL_READING_SOURCE));
    let sink = a_node_of(&compiler, class_path(UNTYPED_SEND_CONTENT_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "reading"),
        InputLinkPortRef::new(sink, "content"),
    )
    .expect("a bundle of small members travels as one Records message");
    assert_eq!(the_links_in(&compiler), 1);
}

#[test]
fn a_bundle_holding_a_texture_into_an_input_of_its_type_connects() {
    register_the_bundle_frame_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FRAMED_PICTURE_SOURCE));
    let sink = a_node_of(&compiler, class_path(FRAMED_PICTURE_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "framed"),
        InputLinkPortRef::new(sink, "framed"),
    )
    .expect("an input typed as the bundle takes its frame apart");
    assert_eq!(the_links_in(&compiler), 1);
}
