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

use super::super::what_a_connect_makes_of_a_link_at_a_placed_level;
use super::{
    CELSIUS_SOURCE, FAHRENHEIT_SINK, FLOAT_SOURCE, NAMED_RECORDS_SOURCE, NUMBER_SINK,
    RECORDS_NEEDING_X, RECORDS_NEEDING_Z, RECORDS_PASSTHROUGH, UNTYPED_RGBA_PICTURE_SINK,
    a_node_of, a_node_type, class_path, connect_on_this_thread, register,
    register_the_verdict_fixture_node_types,
};
use crate::core::Error;
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    FreePinConversion, PinLinkOutcome, PinLinkRefusal, PortDescriptor, PortFamily,
    PortFamilyDeclaration,
};
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::runtime::WhatALiveConnectMakesOfALink;

const FLOAT_SOURCE_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0001);
const NUMBER_SINK_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0002);
const CELSIUS_SOURCE_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0003);
const FAHRENHEIT_SINK_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0004);
const FRAGMENT_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0005);

/// A placed stream's level holding a Float source, a number sink, a Celsius source, a
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
        },
        target: StreamGraphDocumentLinkEnd {
            node: target.0,
            port: target.1.to_owned(),
        },
    }
}

fn what_a_connect_makes_of(
    level: &StreamGraphDocument,
    link: StreamGraphDocumentLink,
) -> crate::core::Result<WhatALiveConnectMakesOfALink> {
    what_a_connect_makes_of_a_link_at_a_placed_level(
        &Arc::new(NodeTypesOneStreamResolves::new()),
        level,
        &link,
    )
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
                a_link((FLOAT_SOURCE_NODE, "value"), (NUMBER_SINK_NODE, "gain")),
            )
            .expect_err("a setting takes no wire"),
        ),
        PinLinkRefusal::InputTakesNoWire { .. }
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

/// A node type whose one output, `samples`, is an untyped Channels port.
const UNTYPED_CHANNELS_SOURCE: &str = "PlacedLevelUntypedChannelsSource";

const NAMED_RECORDS_SOURCE_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0011);
const RECORDS_PASSTHROUGH_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0012);
const RECORDS_NEEDING_X_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0013);
const RECORDS_NEEDING_Z_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0014);
const UNTYPED_CHANNELS_SOURCE_NODE: Uuid =
    Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0015);
const UNTYPED_PICTURE_SINK_NODE: Uuid = Uuid::from_u128(0x5151_0000_0000_4000_8000_0000_0000_0016);

fn register_the_untyped_channels_source_once() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register(a_node_type(
            class_path(UNTYPED_CHANNELS_SOURCE),
            Vec::new(),
            vec![PortDescriptor::new(
                "samples",
                "samples",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            )],
        ));
    });
}

/// A placed stream's level holding a records source naming `x` and `y` that feeds a passthrough
/// whose output follows its input, a sink requiring `x`, a sink requiring `z`, an untyped
/// Channels source and an untyped Image sink.
fn a_placed_level_of_untyped_and_following_ports() -> StreamGraphDocument {
    register_the_untyped_channels_source_once();
    let node = |id: Uuid, name: &str, class_name: &str| {
        let node_type = class_path(class_name);
        json!({ "id": id, "name": name, "type": node_type.as_str() })
    };
    StreamGraphDocument::from_value(json!({
        "nodes": [
            node(NAMED_RECORDS_SOURCE_NODE, "named-records", NAMED_RECORDS_SOURCE),
            node(RECORDS_PASSTHROUGH_NODE, "passthrough", RECORDS_PASSTHROUGH),
            node(RECORDS_NEEDING_X_NODE, "needing-x", RECORDS_NEEDING_X),
            node(RECORDS_NEEDING_Z_NODE, "needing-z", RECORDS_NEEDING_Z),
            node(UNTYPED_CHANNELS_SOURCE_NODE, "samples-source", UNTYPED_CHANNELS_SOURCE),
            node(UNTYPED_PICTURE_SINK_NODE, "picture-sink", UNTYPED_RGBA_PICTURE_SINK),
        ],
        "links": [
            { "source": { "node": NAMED_RECORDS_SOURCE_NODE, "port": "records" },
              "target": { "node": RECORDS_PASSTHROUGH_NODE, "port": "in" } },
        ],
    }))
    .expect("a placed level")
}

/// An output following its input, at a placed level, carries on the names the link the level
/// holds into that input brings: `x` and `y` reach a sink requiring `x`, and a sink requiring
/// `z` is refused naming `z`, in the words connect refuses the same link with in a stream's own
/// graph.
///
/// Mental-revert: building the level without the links it holds leaves the passthrough
/// carrying on nothing known, and the link into the sink requiring `z` is made.
#[test]
fn a_link_at_a_placed_level_is_judged_by_what_the_links_it_holds_carry_on() {
    register_the_verdict_fixture_node_types();
    let level = a_placed_level_of_untyped_and_following_ports();

    what_a_connect_makes_of(
        &level,
        a_link(
            (RECORDS_PASSTHROUGH_NODE, "out"),
            (RECORDS_NEEDING_X_NODE, "records"),
        ),
    )
    .expect("`x` arrives through the passthrough");
    let refused_inside = what_a_connect_makes_of(
        &level,
        a_link(
            (RECORDS_PASSTHROUGH_NODE, "out"),
            (RECORDS_NEEDING_Z_NODE, "records"),
        ),
    )
    .expect_err("`z` never arrives through the passthrough");

    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(NAMED_RECORDS_SOURCE));
    let passthrough = a_node_of(&compiler, class_path(RECORDS_PASSTHROUGH));
    let sink = a_node_of(&compiler, class_path(RECORDS_NEEDING_Z));
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "records"),
        InputLinkPortRef::new(passthrough.clone(), "in"),
    )
    .expect("the source feeds the passthrough");
    let refused_at_the_top = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(passthrough, "out"),
        InputLinkPortRef::new(sink, "records"),
    )
    .expect_err("`z` never arrives through the passthrough");
    assert!(
        matches!(&refused_inside, Error::InvalidLink(words) if words.contains("\"z\"")),
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
            ["named-records", "passthrough", "needing-z"]
        ),
        each_node_by_its_role(
            refused_at_the_top.to_string(),
            [
                &NAMED_RECORDS_SOURCE.to_lowercase(),
                &RECORDS_PASSTHROUGH.to_lowercase(),
                &RECORDS_NEEDING_Z.to_lowercase(),
            ]
        ),
        "the refusal inside is the refusal at the top level, each node by its own name"
    );
}

/// Two untyped ends whose families differ, at a placed level, are refused as connect refuses
/// them in a stream's own graph: an untyped Channels output into an untyped Image input taking
/// Records too.
///
/// Mental-revert: judging only typed ends at a placed level makes the link.
#[test]
fn two_untyped_ends_whose_families_differ_are_refused_at_a_placed_level() {
    register_the_verdict_fixture_node_types();
    let level = a_placed_level_of_untyped_and_following_ports();

    let refused_inside = what_a_connect_makes_of(
        &level,
        a_link(
            (UNTYPED_CHANNELS_SOURCE_NODE, "samples"),
            (UNTYPED_PICTURE_SINK_NODE, "video"),
        ),
    )
    .expect_err("Channels are not an Image");

    register_the_untyped_channels_source_once();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(UNTYPED_CHANNELS_SOURCE));
    let sink = a_node_of(&compiler, class_path(UNTYPED_RGBA_PICTURE_SINK));
    let refused_at_the_top = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "samples"),
        InputLinkPortRef::new(sink, "video"),
    )
    .expect_err("Channels are not an Image");
    assert_eq!(
        std::mem::discriminant(&refused_inside),
        std::mem::discriminant(&refused_at_the_top),
        "{refused_inside:?} / {refused_at_the_top:?}"
    );
    assert_eq!(
        refused_inside
            .to_string()
            .replace("samples-source", "{source}")
            .replace("picture-sink", "{sink}"),
        refused_at_the_top
            .to_string()
            .replace(&UNTYPED_CHANNELS_SOURCE.to_lowercase(), "{source}")
            .replace(&UNTYPED_RGBA_PICTURE_SINK.to_lowercase(), "{sink}"),
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
    let WhatALiveConnectMakesOfALink::ReachesANodeNotBuilt { node_name, why } = made else {
        panic!("the link is made unjudged: {made:?}");
    };
    assert_eq!(node_name, "mystery");
    assert!(
        why.contains("`placed_level_test:NotDescribed` is not described"),
        "{why}"
    );
}
