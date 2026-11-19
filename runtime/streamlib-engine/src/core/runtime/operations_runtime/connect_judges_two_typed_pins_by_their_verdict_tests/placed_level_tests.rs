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
    CELSIUS_SOURCE, FAHRENHEIT_SINK, FLOAT_SOURCE, NUMBER_SINK, class_path,
    register_the_verdict_fixture_node_types,
};
use crate::core::Error;
use crate::core::descriptors::{FreePinConversion, PinLinkOutcome, PinLinkRefusal};
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
/// refused naming both nodes as the level names them, a link into a setting as taking no wire,
/// and a link only a convert node bridges as its pin types not connecting.
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
    assert!(matches!(
        the_verdicts_refusal(
            what_a_connect_makes_of(
                &level,
                a_link(
                    (CELSIUS_SOURCE_NODE, "temperature"),
                    (FAHRENHEIT_SINK_NODE, "temperature"),
                ),
            )
            .expect_err("a convert node is not placed inside a placed stream"),
        ),
        PinLinkRefusal::PinTypesDoNotConnect { .. }
    ));
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
