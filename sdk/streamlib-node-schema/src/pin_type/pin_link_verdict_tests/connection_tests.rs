// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How connections link: two ends of one connection type, whatever the input takes of referrers.

use super::*;

fn connection(import_path: &str, one_referrer_per_referenced_node: bool) -> PinType {
    PinType::Connection {
        connection_type: NamedPinTypeImportPath::new(import_path).unwrap(),
        one_referrer_per_referenced_node,
    }
}

/// Mental-revert: compared whole, the input's sole-referrer flag would refuse every connection
/// into a sole-referrer input.
#[test]
fn a_connection_connects_to_one_of_its_own_type_whatever_the_input_takes_of_referrers() {
    assert_eq!(
        the_outcome_between(
            &connection("relays:RelayConnection", false),
            &connection("relays:RelayConnection", true)
        ),
        PinLinkOutcome::Connects
    );
}

#[test]
fn a_connection_of_one_modules_type_is_refused_on_another_naming_both() {
    let PinLinkOutcome::Refused(refusal) = the_outcome_between(
        &connection("relays:RelayConnection", false),
        &connection("tracks:TrackConnection", false),
    ) else {
        panic!("two connection types do not connect");
    };
    let words = refusal.to_string();
    assert!(
        words.contains("relays:RelayConnection") && words.contains("tracks:TrackConnection"),
        "{words}"
    );
    assert!(matches!(
        the_outcome_between(
            &named("relays:RelayConnection"),
            &connection("relays:RelayConnection", false)
        ),
        PinLinkOutcome::Refused(_)
    ));
}
