// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;
use streamlib::sdk::descriptors::{PinDeclaration, PinGroupDescriptor, PinType, PortDescriptor};

use super::*;

fn a_float_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(
        name,
        name,
        true,
        PinDeclaration::exactly(PinType::Float { exact_size: None }),
    )
}

fn the_layers_group() -> PinGroupDescriptor {
    PinGroupDescriptor::new("layers", "", vec![a_float_port("layer")], Vec::new())
}

fn a_link_into(source: Uuid, mixer: Uuid, port: &str) -> serde_json::Value {
    json!({
        "source": { "node": source, "port": "value" },
        "target": { "node": mixer, "port": port },
    })
}

fn a_link(source: Uuid, target: Uuid, port: &str) -> StreamGraphDocumentLink {
    StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            node: source,
            port: "value".to_owned(),
            member: None,
        },
        target: StreamGraphDocumentLinkEnd {
            node: target,
            port: port.to_owned(),
            member: None,
        },
    }
}

/// A mixer whose slots 0 and 2 are linked, slot 1 having just been unwired, holding `values`.
fn a_mixer_with_a_gap(values: serde_json::Value) -> (StreamGraphDocument, Uuid, [Uuid; 2]) {
    let (mixer, first, third) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let document = StreamGraphDocument::from_value(json!({
        "asset": Uuid::new_v4(),
        "nodes": [
            { "id": mixer, "name": "mixer", "type": "fixture:Mixer", "input_values": values },
            { "id": first, "name": "first", "type": "fixture:Source" },
            { "id": third, "name": "third", "type": "fixture:Source" },
        ],
        "links": [
            a_link_into(first, mixer, "layers.0.layer"),
            a_link_into(third, mixer, "layers.2.layer"),
        ],
    }))
    .unwrap();
    (document, mixer, [first, third])
}

/// Mental-revert: without the moves, slot 2 stays linked past the empty slot 1.
#[test]
fn a_later_slots_link_moves_down_into_the_gap() {
    let (document, mixer, [_, third]) = a_mixer_with_a_gap(json!({}));
    let node = document.node(mixer).unwrap();

    let moves = the_moves_closing_each_gap_of_the_node(&document, &node, &[the_layers_group()]);

    assert_eq!(
        moves,
        [
            ASlotMoved::Disconnect(a_link(third, mixer, "layers.2.layer")),
            ASlotMoved::Connect(a_link(third, mixer, "layers.1.layer")),
        ]
    );
}

#[test]
fn a_later_slots_value_moves_with_it_and_the_gaps_value_is_taken_off() {
    let (document, mixer, _) = a_mixer_with_a_gap(json!({
        "layers.1.layer": 0.25,
        "layers.2.layer": 0.5,
    }));
    let node = document.node(mixer).unwrap();

    let moves = the_moves_closing_each_gap_of_the_node(&document, &node, &[the_layers_group()]);

    assert_eq!(
        moves[2..],
        [
            ASlotMoved::SetInputValue {
                node: mixer,
                input: "layers.1.layer".to_owned(),
                value: Some(json!(0.5)),
            },
            ASlotMoved::SetInputValue {
                node: mixer,
                input: "layers.2.layer".to_owned(),
                value: None,
            },
        ]
    );
}

#[test]
fn slots_with_no_gap_move_nothing() {
    let (mixer, first) = (Uuid::new_v4(), Uuid::new_v4());
    let document = StreamGraphDocument::from_value(json!({
        "asset": Uuid::new_v4(),
        "nodes": [
            { "id": mixer, "name": "mixer", "type": "fixture:Mixer" },
            { "id": first, "name": "first", "type": "fixture:Source" },
        ],
        "links": [a_link_into(first, mixer, "layers.0.layer")],
    }))
    .unwrap();
    let node = document.node(mixer).unwrap();

    assert_eq!(
        the_moves_closing_each_gap_of_the_node(&document, &node, &[the_layers_group()]),
        []
    );
}

#[test]
fn a_group_whose_count_is_set_keeps_its_gaps() {
    let (document, mixer, _) = a_mixer_with_a_gap(json!({}));
    let node = document.node(mixer).unwrap();
    let counted = PinGroupDescriptor::new("layers", "", Vec::new(), vec![a_float_port("layer")]);

    assert_eq!(
        the_moves_closing_each_gap_of_the_node(&document, &node, &[counted]),
        []
    );
}
