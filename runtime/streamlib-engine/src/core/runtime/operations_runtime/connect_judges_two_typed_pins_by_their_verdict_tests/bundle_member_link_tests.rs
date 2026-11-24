// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link from one member of a bundle output carries that member and is judged by its type; a
//! member the output does not hold is refused naming the members it has.

use super::*;
use crate::core::PortDirection;
use crate::core::descriptors::BundleMember;
use crate::core::graph::{GraphEdgeWithComponents, PendingDeletionComponent};

const READING_SOURCE: &str = "VerdictCheckReadingSource";
const FLOAT_SINK: &str = "VerdictCheckFloatSink";
const READING_SINK: &str = "VerdictCheckReadingSink";
const LABEL_SOURCE: &str = "VerdictCheckLabelSource";
const FRAMED_READING_SINK: &str = "VerdictCheckFramedReadingSink";
const TENSOR_SOURCE: &str = "VerdictCheckTensorSource";
const PICTURE_SOURCE: &str = "VerdictCheckPictureSource";
const PICTURE_SINK: &str = "VerdictCheckPictureSink";
const METERED_PICTURE_SOURCE: &str = "VerdictCheckMeteredPictureSource";
const METERED_PICTURE_SINK: &str = "VerdictCheckMeteredPictureSink";

fn a_member(name: &str, member_type: PinType) -> BundleMember {
    BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value: None,
    }
}

/// A reading: a Float `level` and a String `label`.
fn a_reading() -> PinType {
    PinType::Bundle {
        name: None,
        members: vec![
            a_member("level", float()),
            a_member("label", PinType::String {}),
        ],
    }
}

fn register_the_bundle_fixture_node_types() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    register_the_verdict_fixture_node_types();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register(a_node_type(
            class_path(READING_SOURCE),
            Vec::new(),
            vec![a_typed_port("reading", a_reading())],
        ));
        register(a_node_type(
            class_path(FLOAT_SINK),
            vec![a_typed_port("level", float())],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(READING_SINK),
            vec![a_typed_port("reading", a_reading())],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(LABEL_SOURCE),
            Vec::new(),
            vec![a_typed_port("label", PinType::String {})],
        ));
        register(a_node_type(
            class_path(FRAMED_READING_SINK),
            vec![a_typed_port(
                "reading",
                PinType::Bundle {
                    name: None,
                    members: vec![
                        a_member("level", float()),
                        a_member("frame", PinType::Tensor {}),
                    ],
                },
            )],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(TENSOR_SOURCE),
            Vec::new(),
            vec![a_typed_port("frame", PinType::Tensor {})],
        ));
    });
}

fn a_texture() -> PinType {
    PinType::Texture {
        pixel_formats: Vec::new(),
    }
}

/// A picture with the level metered off it: a Texture beside a Float.
fn a_metered_picture() -> PinType {
    PinType::Bundle {
        name: None,
        members: vec![a_member("picture", a_texture()), a_member("level", float())],
    }
}

fn register_the_picture_fixture_node_types() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    register_the_bundle_fixture_node_types();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register(a_node_type(
            class_path(PICTURE_SOURCE),
            Vec::new(),
            vec![a_typed_port("picture", a_texture())],
        ));
        register(a_node_type(
            class_path(PICTURE_SINK),
            vec![a_typed_port("picture", a_texture())],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(METERED_PICTURE_SOURCE),
            Vec::new(),
            vec![a_typed_port("metered", a_metered_picture())],
        ));
        register(a_node_type(
            class_path(METERED_PICTURE_SINK),
            vec![a_typed_port("metered", a_metered_picture())],
            Vec::new(),
        ));
    });
}

/// The member each link of `compiler`'s graph ends at.
fn the_members_the_links_end_at(compiler: &Compiler) -> Vec<Option<String>> {
    compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .e(())
            .iter()
            .map(|link| link.to_port().member().map(str::to_owned))
            .collect()
    })
}

/// The member each link of `compiler`'s graph starts at.
fn the_members_the_links_start_at(compiler: &Compiler) -> Vec<Option<String>> {
    compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .e(())
            .iter()
            .map(|link| link.source().member().map(str::to_owned))
            .collect()
    })
}

/// Mental-revert: judging the end by the bundle's own type refuses the link as a bundle into a
/// Float.
#[test]
fn a_link_from_a_float_member_of_a_bundle_into_a_float_input_connects() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(READING_SOURCE));
    let sink = a_node_of(&compiler, class_path(FLOAT_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "reading").at_member(Some("level".to_owned())),
        InputLinkPortRef::new(sink, "level"),
    )
    .expect("the member `level` is a Float");

    assert_eq!(
        the_members_the_links_start_at(&compiler),
        vec![Some("level".to_owned())]
    );
}

#[test]
fn a_link_from_a_member_is_judged_by_the_members_type_naming_the_member() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(READING_SOURCE));
    let sink = a_node_of(&compiler, class_path(FLOAT_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "reading").at_member(Some("label".to_owned())),
            InputLinkPortRef::new(sink, "level"),
        )
        .expect_err("a String member does not connect to a Float"),
    );

    let words = refusal.to_string();
    assert!(words.contains("label"), "{words}");
    assert!(words.contains("(String)"), "{words}");
    assert_eq!(the_links_in(&compiler), 0);
}

/// Mental-revert: without the member check, the link is refused by its types alone, with no
/// member named.
#[test]
fn a_member_the_bundle_does_not_hold_is_refused_naming_the_members_it_has() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(READING_SOURCE));
    let sink = a_node_of(&compiler, class_path(FLOAT_SINK));

    let refused = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "reading").at_member(Some("depth".to_owned())),
        InputLinkPortRef::new(sink, "level"),
    )
    .expect_err("the bundle holds no member `depth`");

    let Error::PortHasNoMemberNamed(not_held) = &refused else {
        panic!("refused as a member the output does not hold, not {refused:?}");
    };
    assert_eq!(not_held.member, "depth");
    assert_eq!(not_held.members_it_has, ["level", "label"]);
    let words = refused.to_string();
    assert!(
        words.contains("`level`") && words.contains("`label`"),
        "{words}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}

#[test]
fn a_member_of_an_output_carrying_no_bundle_is_refused() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let sink = a_node_of(&compiler, class_path(FLOAT_SINK));

    let refused = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "value").at_member(Some("level".to_owned())),
        InputLinkPortRef::new(sink, "level"),
    )
    .expect_err("a Float holds no member");

    assert!(
        matches!(
            &refused,
            Error::PortHasNoMemberNamed(not_held) if not_held.members_it_has.is_empty()
        ),
        "{refused:?}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}

/// A Float output linked into the Float member `level` of a bundle input connects, the link
/// ending at that member; a second link, from a String output into the String member `label`,
/// connects beside it.
///
/// Mental-revert: judging the end by the input's own type refuses a Float into a bundle.
#[test]
fn a_link_into_a_member_of_a_bundle_input_is_judged_by_the_members_type() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let level = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let label = a_node_of(&compiler, class_path(LABEL_SOURCE));
    let sink = a_node_of(&compiler, class_path(READING_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(level, "value"),
        InputLinkPortRef::new(sink.clone(), "reading").at_member(Some("level".to_owned())),
    )
    .expect("the member `level` is a Float");
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(label.clone(), "label"),
        InputLinkPortRef::new(sink.clone(), "reading").at_member(Some("label".to_owned())),
    )
    .expect("the member `label` is a String");

    assert_eq!(
        the_members_the_links_end_at(&compiler),
        vec![Some("level".to_owned()), Some("label".to_owned())]
    );

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(label, "label"),
            InputLinkPortRef::new(sink, "reading").at_member(Some("level".to_owned())),
        )
        .expect_err("a String does not connect to the Float member `level`"),
    );
    assert!(refusal.to_string().contains("level"), "{refusal}");
}

/// A link into a member the bundle input does not hold is refused naming its members.
#[test]
fn a_link_into_a_member_the_input_does_not_hold_is_refused_naming_its_members() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let sink = a_node_of(&compiler, class_path(READING_SINK));

    let refused = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "value"),
        InputLinkPortRef::new(sink, "reading").at_member(Some("depth".to_owned())),
    )
    .expect_err("the bundle holds no member `depth`");

    let Error::PortHasNoMemberNamed(not_held) = &refused else {
        panic!("refused as a member the input does not hold, not {refused:?}");
    };
    assert_eq!(not_held.direction, PortDirection::Input);
    assert_eq!(not_held.members_it_has, ["level", "label"]);
    assert!(
        refused.to_string().contains("ends at member `depth`"),
        "{refused}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}

/// A bundle input is fed whole or member by member: a whole link beside a member link is
/// refused, and so is a member link beside a whole one.
#[test]
fn a_whole_link_beside_a_member_link_into_one_bundle_input_is_refused() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let level = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let reading = a_node_of(&compiler, class_path(READING_SOURCE));
    let by_member = a_node_of(&compiler, class_path(READING_SINK));
    let whole = a_node_of(&compiler, class_path(READING_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(level.clone(), "value"),
        InputLinkPortRef::new(by_member.clone(), "reading").at_member(Some("level".to_owned())),
    )
    .unwrap();
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(reading.clone(), "reading"),
        InputLinkPortRef::new(whole.clone(), "reading"),
    )
    .unwrap();

    for (from, to) in [
        (
            OutputLinkPortRef::new(reading, "reading"),
            InputLinkPortRef::new(by_member, "reading"),
        ),
        (
            OutputLinkPortRef::new(level, "value"),
            InputLinkPortRef::new(whole, "reading").at_member(Some("level".to_owned())),
        ),
    ] {
        let refused = connect_on_this_thread(&compiler, from, to)
            .expect_err("a bundle input is fed whole or member by member");
        assert!(
            refused.to_string().contains("whole or member by member"),
            "{refused}"
        );
    }
    assert_eq!(the_links_in(&compiler), 2);
}

/// A whole link a disconnect has marked leaves with the commit, so a member link into the same
/// input in that commit connects beside it.
#[test]
fn a_member_link_connects_beside_a_whole_link_marked_for_removal() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let level = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let reading = a_node_of(&compiler, class_path(READING_SOURCE));
    let sink = a_node_of(&compiler, class_path(READING_SINK));
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(reading, "reading"),
        InputLinkPortRef::new(sink.clone(), "reading"),
    )
    .unwrap();
    compiler.scope(|graph, _tx| {
        let whole = graph
            .traversal()
            .e(())
            .iter()
            .map(|link| link.id.clone())
            .next();
        let whole = whole.unwrap_or_else(|| panic!("the whole link is in the graph"));
        if let Some(link) = graph.traversal_mut().e(&whole).first_mut() {
            link.insert(PendingDeletionComponent);
        }
    });

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(level, "value"),
        InputLinkPortRef::new(sink, "reading").at_member(Some("level".to_owned())),
    )
    .unwrap_or_else(|why| panic!("the marked whole link does not stand: {why}"));
}

/// A member that travels only as messages holds no value for the input's port to build its
/// bundle with, so a link ending at it is refused; a member holding a value beside it connects.
#[test]
fn a_link_into_a_tensor_member_is_refused() {
    register_the_bundle_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let level = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let frame = a_node_of(&compiler, class_path(TENSOR_SOURCE));
    let sink = a_node_of(&compiler, class_path(FRAMED_READING_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(level, "value"),
        InputLinkPortRef::new(sink.clone(), "reading").at_member(Some("level".to_owned())),
    )
    .expect("the member `level` holds a value");
    let refused = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(frame, "frame"),
        InputLinkPortRef::new(sink, "reading").at_member(Some("frame".to_owned())),
    )
    .expect_err("a Tensor member travels only as messages");

    assert!(
        refused.to_string().contains("travels only as messages"),
        "{refused}"
    );
    assert_eq!(the_links_in(&compiler), 1);
}

/// A Texture member rides its own row of the Bundle frame the input's port builds, so a link
/// ending at it connects beside a link into a number member.
///
/// Mental-revert: judging a Texture member as one that travels only as messages refuses it.
#[test]
fn a_link_into_a_texture_member_connects_beside_a_number_member() {
    register_the_picture_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let picture = a_node_of(&compiler, class_path(PICTURE_SOURCE));
    let level = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let sink = a_node_of(&compiler, class_path(METERED_PICTURE_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(picture, "picture"),
        InputLinkPortRef::new(sink.clone(), "metered").at_member(Some("picture".to_owned())),
    )
    .expect("the member `picture` is a Texture");
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(level, "value"),
        InputLinkPortRef::new(sink, "metered").at_member(Some("level".to_owned())),
    )
    .expect("the member `level` holds a value");

    assert_eq!(
        the_members_the_links_end_at(&compiler),
        vec![Some("picture".to_owned()), Some("level".to_owned())]
    );
}

#[test]
fn a_link_from_a_texture_member_into_a_texture_input_connects() {
    register_the_picture_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(METERED_PICTURE_SOURCE));
    let sink = a_node_of(&compiler, class_path(PICTURE_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "metered").at_member(Some("picture".to_owned())),
        InputLinkPortRef::new(sink, "picture"),
    )
    .expect("the member `picture` is a Texture");

    assert_eq!(
        the_members_the_links_start_at(&compiler),
        vec![Some("picture".to_owned())]
    );
}
