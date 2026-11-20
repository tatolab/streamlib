// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link from one member of a bundle output carries that member and is judged by its type; a
//! member the output does not hold is refused naming the members it has.

use super::*;
use crate::core::descriptors::BundleMember;

const READING_SOURCE: &str = "VerdictCheckReadingSource";
const FLOAT_SINK: &str = "VerdictCheckFloatSink";

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
    });
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

    let Error::OutputPortHasNoMemberNamed(not_held) = &refused else {
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
            Error::OutputPortHasNoMemberNamed(not_held) if not_held.members_it_has.is_empty()
        ),
        "{refused:?}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}
