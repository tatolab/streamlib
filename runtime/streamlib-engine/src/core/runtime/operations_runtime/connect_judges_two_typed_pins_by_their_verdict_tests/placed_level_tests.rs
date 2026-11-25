// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link at a placed stream's level, judged among that level's nodes as its document holds
//! them, takes the verdict connect takes on the same link in a stream's own graph.

use std::sync::Arc;

use serde_json::json;
use streamlib_project_repository::{
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument, StreamGraphDocumentLink,
    StreamGraphDocumentLinkEnd,
};
use uuid::Uuid;

use super::super::what_a_connect_makes_of_each_link_at_a_placed_level;
use super::{
    CELSIUS_SOURCE, FAHRENHEIT_SINK, FLOAT_SOURCE, FLOAT_STREAM_SOURCE, NUMBER_SINK, PASSTHROUGH,
    a_node_of, class_path, connect_on_this_thread, register_the_verdict_fixture_node_types,
};
use crate::core::Error;
use crate::core::compiler::Compiler;
use crate::core::descriptors::{FreePinConversion, PinLinkOutcome, PinLinkRefusal};
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::runtime::WhatALiveConnectMakesOfALink;

const FLOAT_SOURCE_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0001);
const NUMBER_SINK_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0002);
const CELSIUS_SOURCE_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0003);
const FAHRENHEIT_SINK_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0004);
const FRAGMENT_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0005);
const FLOAT_STREAM_SOURCE_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0006);

/// A placed stream's level holding a Float source, a source of a stream of Floats, a number sink, a Celsius source, a
/// Fahrenheit sink and a fragment, with `links`.
fn a_placed_level(links: serde_json::Value) -> StreamGraphDocument {
    let node = |id: Uuid, name: &str, class_name: &str| {
        let node_type = class_path(class_name);
        json!({ "id": id, "name": name, "type": node_type.as_str() })
    };
    StreamGraphDocument::from_value(json!({
        "nodes": [
            node(FLOAT_SOURCE_NODE, "float-source", FLOAT_SOURCE),
            node(NUMBER_SINK_NODE, "number-sink", NUMBER_SINK),
            node(FLOAT_STREAM_SOURCE_NODE, "float-stream-source", FLOAT_STREAM_SOURCE),
            node(CELSIUS_SOURCE_NODE, "celsius-source", CELSIUS_SOURCE),
            node(FAHRENHEIT_SINK_NODE, "fahrenheit-sink", FAHRENHEIT_SINK),
            { "id": FRAGMENT_NODE, "name": "inner", "type": STREAM_FRAGMENT_NODE_TYPE,
              "input_values": { "stream": { "asset": Uuid::nil() } } },
        ],
        "links": links,
    }))
    .expect("a placed level")
}

fn a_link(source: (Uuid, &str), target: (Uuid, &str)) -> StreamGraphDocumentLink {
    StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            node: source.0,
            port: source.1.to_owned(),
            member: None,
        },
        target: StreamGraphDocumentLinkEnd {
            node: target.0,
            port: target.1.to_owned(),
            member: None,
        },
    }
}

fn what_a_connect_makes_of(
    level: &StreamGraphDocument,
    link: StreamGraphDocumentLink,
) -> crate::core::Result<WhatALiveConnectMakesOfALink> {
    let mut made = what_a_connect_makes_of_each_link_at_a_placed_level(
        &Arc::new(NodeTypesOneStreamResolves::new()),
        level,
        std::slice::from_ref(&link),
    );
    assert_eq!(made.len(), 1, "one judgement for the one link");
    made.remove(0)
}

fn the_verdicts_refusal(refused: Error) -> PinLinkRefusal {
    match refused {
        Error::PinLinkRefused(refusal) => *refusal,
        other => panic!("a refusal in the verdict's words, not {other:?}"),
    }
}

/// A Float into an Integer at a placed level connects for free; a Float into a String is
/// refused naming both nodes as the level names them, and a link into a setting as taking no
/// wire; a link only a convert node bridges answers that convert node, as at the stream's own
/// level, for the caller to refuse where `connect` places none.
///
/// Mental-revert: a judgement reading the level's nodes from nowhere answers no verdict.
#[test]
fn a_link_at_a_placed_level_takes_the_verdict_connect_takes() {
    register_the_verdict_fixture_node_types();
    let level = a_placed_level(json!([]));

    let made = what_a_connect_makes_of(
        &level,
        a_link((FLOAT_SOURCE_NODE, "value"), (NUMBER_SINK_NODE, "count")),
    )
    .expect("a Float is read as an Integer for free");
    let WhatALiveConnectMakesOfALink::Makes(Some(verdict)) = made else {
        panic!("two typed ends have a verdict: {made:?}");
    };
    assert_eq!(
        verdict.outcome,
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenNumbers)
    );

    let not_connecting = the_verdicts_refusal(
        what_a_connect_makes_of(
            &level,
            a_link((FLOAT_SOURCE_NODE, "value"), (NUMBER_SINK_NODE, "label")),
        )
        .expect_err("a Float does not connect to a String"),
    );
    let words = not_connecting.to_string();
    assert!(words.contains("`float-source.value` (Float)"), "{words}");
    assert!(words.contains("`number-sink.label` (String)"), "{words}");

    assert!(matches!(
        the_verdicts_refusal(
            what_a_connect_makes_of(
                &level,
                a_link(
                    (FLOAT_STREAM_SOURCE_NODE, "value"),
                    (NUMBER_SINK_NODE, "gain")
                ),
            )
            .expect_err("a setting takes no stream"),
        ),
        PinLinkRefusal::StreamIntoASetting { .. }
    ));
    let through_a_convert_node = what_a_connect_makes_of(
        &level,
        a_link(
            (CELSIUS_SOURCE_NODE, "temperature"),
            (FAHRENHEIT_SINK_NODE, "temperature"),
        ),
    )
    .expect("a link a built-in conversion bridges answers that conversion");
    let WhatALiveConnectMakesOfALink::Makes(Some(verdict)) = through_a_convert_node else {
        panic!("two typed ends have a verdict: {through_a_convert_node:?}");
    };
    assert!(
        matches!(
            verdict.outcome,
            PinLinkOutcome::ConnectsThroughConvertNode(_)
        ),
        "{verdict:?}"
    );
}

/// A level holding links of its own is built with them, and a link at it is judged among its
/// nodes all the same; a link reaching a fragment is judged only as it is made.
#[test]
fn a_placed_levels_own_links_and_fragments_are_read_as_its_document_holds_them() {
    register_the_verdict_fixture_node_types();
    let level = a_placed_level(json!([
        { "source": { "node": FLOAT_SOURCE_NODE, "port": "value" },
          "target": { "node": NUMBER_SINK_NODE, "port": "count" } },
    ]));

    the_verdicts_refusal(
        what_a_connect_makes_of(
            &level,
            a_link((FLOAT_SOURCE_NODE, "value"), (NUMBER_SINK_NODE, "label")),
        )
        .expect_err("a Float does not connect to a String"),
    );
    assert_eq!(
        what_a_connect_makes_of(
            &level,
            a_link((FLOAT_SOURCE_NODE, "value"), (FRAGMENT_NODE, "anything")),
        )
        .expect("a link to a fragment is left to be judged as it is made"),
        WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade
    );
}

const PASSTHROUGH_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0012);

/// A placed stream's level holding a Float source feeding a passthrough whose output follows its
/// input, and a number sink.
fn a_placed_level_of_a_following_port() -> StreamGraphDocument {
    let node = |id: Uuid, name: &str, class_name: &str| {
        let node_type = class_path(class_name);
        json!({ "id": id, "name": name, "type": node_type.as_str() })
    };
    StreamGraphDocument::from_value(json!({
        "nodes": [
            node(FLOAT_SOURCE_NODE, "level-source", FLOAT_SOURCE),
            node(PASSTHROUGH_NODE, "passthrough", PASSTHROUGH),
            node(NUMBER_SINK_NODE, "numbers", NUMBER_SINK),
        ],
        "links": [
            { "source": { "node": FLOAT_SOURCE_NODE, "port": "value" },
              "target": { "node": PASSTHROUGH_NODE, "port": "in" } },
        ],
    }))
    .expect("a placed level")
}

/// An output following its input, at a placed level, carries on the type the link the level
/// holds into that input brings: a Float reaches an Integer input, and a String input is refused,
/// in the words connect refuses the same link with in a stream's own graph.
///
/// Mental-revert: building the level without the links it holds leaves the passthrough
/// carrying on nothing typed, and the link into the String input is made.
#[test]
fn a_link_at_a_placed_level_is_judged_by_what_the_links_it_holds_carry_on() {
    register_the_verdict_fixture_node_types();
    let level = a_placed_level_of_a_following_port();

    what_a_connect_makes_of(
        &level,
        a_link((PASSTHROUGH_NODE, "out"), (NUMBER_SINK_NODE, "count")),
    )
    .expect("a Float arrives through the passthrough and is read as an Integer");
    let refused_inside = what_a_connect_makes_of(
        &level,
        a_link((PASSTHROUGH_NODE, "out"), (NUMBER_SINK_NODE, "label")),
    )
    .expect_err("a Float through the passthrough is no String");

    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let passthrough = a_node_of(&compiler, class_path(PASSTHROUGH));
    let sink = a_node_of(&compiler, class_path(NUMBER_SINK));
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "value"),
        InputLinkPortRef::new(passthrough.clone(), "in"),
    )
    .expect("the source feeds the passthrough");
    let refused_at_the_top = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(passthrough, "out"),
        InputLinkPortRef::new(sink, "label"),
    )
    .expect_err("a Float through the passthrough is no String");
    assert!(
        refused_inside.to_string().contains("String"),
        "{refused_inside:?}"
    );
    let each_node_by_its_role = |words: String, names: [&str; 3]| {
        names
            .into_iter()
            .zip(["{source}", "{via}", "{sink}"])
            .fold(words, |words, (name, role)| words.replace(name, role))
    };
    assert_eq!(
        each_node_by_its_role(
            refused_inside.to_string(),
            ["level-source", "passthrough", "numbers"]
        ),
        each_node_by_its_role(
            refused_at_the_top.to_string(),
            [
                &FLOAT_SOURCE.to_lowercase(),
                &PASSTHROUGH.to_lowercase(),
                &NUMBER_SINK.to_lowercase(),
            ]
        ),
        "the refusal inside is the refusal at the top level, each node by its own name"
    );
}

/// A link reaching a node whose type is not described is judged only as it is made, and says
/// so naming the node and its type.
#[test]
fn a_link_reaching_a_node_of_a_type_not_described_is_judged_only_as_it_is_made() {
    register_the_verdict_fixture_node_types();
    let undescribed = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0021);
    let mut level = a_placed_level(json!([])).to_value();
    level["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": undescribed, "name": "mystery", "type": "placed_level_test:NotDescribed",
    }));
    let level = StreamGraphDocument::from_value(level).expect("a placed level");

    let made = what_a_connect_makes_of(
        &level,
        a_link((FLOAT_SOURCE_NODE, "value"), (undescribed, "anything")),
    )
    .expect("a link to a node not built is left to be judged as it is made");
    let WhatALiveConnectMakesOfALink::ReachesANodeNotBuilt(not_built) = made else {
        panic!("the link is made unjudged: {made:?}");
    };
    assert_eq!(not_built.node_name, "mystery");
    assert!(
        not_built
            .why
            .contains("`placed_level_test:NotDescribed` is not described"),
        "{not_built}"
    );
}

/// A link reaching a node the level does not hold is refused, naming the node as not at this
/// level, not left to be judged as it is made.
#[test]
fn a_link_reaching_a_node_the_level_does_not_hold_is_refused() {
    register_the_verdict_fixture_node_types();
    let level = a_placed_level(json!([]));
    let elsewhere = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0022);

    let refused = what_a_connect_makes_of(
        &level,
        a_link((FLOAT_SOURCE_NODE, "value"), (elsewhere, "value")),
    )
    .expect_err("a link to a node the level does not hold is refused");

    assert!(
        refused
            .to_string()
            .contains(&format!("{elsewhere}, which is not at this level")),
        "{refused}"
    );
}
