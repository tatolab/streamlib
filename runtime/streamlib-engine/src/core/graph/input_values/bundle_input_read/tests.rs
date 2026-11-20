// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;

fn member(name: &str, default_value: Option<Value>) -> BundleMember {
    BundleMember {
        name: name.to_owned(),
        member_type: PinType::Float { exact_size: None },
        meaning: None,
        default_value,
    }
}

#[test]
fn each_member_resolves_alone_live_over_saved_over_its_default() {
    let members = [
        member("level", None),
        member("gain", None),
        member("pan", Some(json!(0.5))),
        member("unset", None),
    ];
    let live = json!({ "level": 1.0 });
    let saved = json!({ "level": 0.2, "gain": 2.0 });
    let held = ValuesABundleInputHolds {
        live: Some(&live),
        saved: Some(&saved),
        input_default: None,
    };

    let read = held.each_member_read(&members, &WhatTheLinksIntoAnInputFeed::default());

    let rows: Vec<(String, Option<Value>, Option<InputValueComesFromOnTheWire>)> = read
        .into_iter()
        .map(|row| (row.member, row.value, row.value_comes_from))
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "level".to_owned(),
                Some(json!(1.0)),
                Some(InputValueComesFromOnTheWire::Live)
            ),
            (
                "gain".to_owned(),
                Some(json!(2.0)),
                Some(InputValueComesFromOnTheWire::Saved)
            ),
            (
                "pan".to_owned(),
                Some(json!(0.5)),
                Some(InputValueComesFromOnTheWire::Default)
            ),
            ("unset".to_owned(), Some(Value::Null), None),
        ]
    );
    assert_eq!(
        held.the_bundle_read(&members),
        Some(json!({ "level": 1.0, "gain": 2.0, "pan": 0.5 }))
    );
}

#[test]
fn a_linked_bundle_input_reads_each_member_from_the_wire() {
    let saved = json!({ "level": 0.2 });
    let held = ValuesABundleInputHolds {
        live: None,
        saved: Some(&saved),
        input_default: None,
    };

    let read = held.each_member_read(
        &[member("level", None)],
        &WhatTheLinksIntoAnInputFeed::feeding(true, &[]),
    );

    assert_eq!(read[0].value, None);
    assert_eq!(
        read[0].value_comes_from,
        Some(InputValueComesFromOnTheWire::Wire)
    );
}

#[test]
fn a_member_a_link_ends_at_reads_from_the_wire_and_every_other_alone() {
    let saved = json!({ "level": 0.2, "gain": 2.0 });
    let held = ValuesABundleInputHolds {
        live: None,
        saved: Some(&saved),
        input_default: None,
    };

    let read = held.each_member_read(
        &[member("level", None), member("gain", None)],
        &WhatTheLinksIntoAnInputFeed::feeding(false, &["level"]),
    );

    assert_eq!(
        (read[0].value.clone(), read[0].value_comes_from),
        (None, Some(InputValueComesFromOnTheWire::Wire))
    );
    assert_eq!(
        (read[1].value.clone(), read[1].value_comes_from),
        (Some(json!(2.0)), Some(InputValueComesFromOnTheWire::Saved))
    );
}

#[test]
fn the_live_members_replace_those_members_alone() {
    assert_eq!(
        the_live_members_over_the_held_ones(
            Some(&json!({ "level": 0.2, "gain": 2.0 })),
            json!({ "level": 1.0 })
        ),
        json!({ "level": 1.0, "gain": 2.0 })
    );
}
