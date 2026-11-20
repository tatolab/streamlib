// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How bundles connect: an output holding every member an input asks for, by name and type.

use super::*;

fn a_member(name: &str, member_type: PinType) -> BundleMember {
    BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value: None,
    }
}

fn a_bundle_of(members: &[(&str, PinType)]) -> PinType {
    PinType::Bundle {
        members: members
            .iter()
            .map(|(name, member_type)| a_member(name, member_type.clone()))
            .collect(),
    }
}

fn a_list_of(element_type: PinType) -> PinType {
    PinType::ListOf {
        element_type: Box::new(element_type),
    }
}

/// A placement of three members: `x`, `y` and `opacity`.
fn a_placement() -> PinType {
    a_bundle_of(&[("x", float()), ("y", float()), ("opacity", float())])
}

/// Bundle members are compared by name and type; a member meaning that differs only warns.
#[test]
fn bundles_connect_by_member_names_and_types_and_warn_of_member_meanings() {
    let bundle = |meaning: &str| PinType::Bundle {
        members: vec![BundleMember {
            name: "pitch".to_owned(),
            member_type: float(),
            meaning: Some(meaning.to_owned()),
            default_value: None,
        }],
    };
    assert_eq!(
        the_verdict_between(&bundle("frequency"), &bundle("note")),
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: vec![PinLinkWarning::MemberMeaningsDiffer {
                member_name: "pitch".to_owned(),
                output_meaning: "frequency".to_owned(),
                input_meaning: "note".to_owned(),
            }],
        }
    );
    let list_of = |element_type| PinType::ListOf {
        element_type: Box::new(element_type),
    };
    let pitch_warning = |member_name: &str| PinLinkWarning::MemberMeaningsDiffer {
        member_name: member_name.to_owned(),
        output_meaning: "frequency".to_owned(),
        input_meaning: "note".to_owned(),
    };
    assert_eq!(
        the_verdict_between(&list_of(bundle("frequency")), &list_of(bundle("note"))),
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: vec![pitch_warning("pitch")],
        }
    );
    let nested = |meaning: &str| PinType::Bundle {
        members: vec![BundleMember {
            name: "voice".to_owned(),
            member_type: bundle(meaning),
            meaning: None,
            default_value: None,
        }],
    };
    assert_eq!(
        the_verdict_between(&nested("frequency"), &nested("note")),
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: vec![pitch_warning("voice.pitch")],
        }
    );
    let renamed = PinType::Bundle {
        members: vec![BundleMember {
            name: "gain".to_owned(),
            member_type: float(),
            meaning: None,
            default_value: None,
        }],
    };
    assert!(matches!(
        the_outcome_between(&bundle("frequency"), &renamed),
        PinLinkOutcome::Refused(_)
    ));
    assert_eq!(bundle("frequency").to_string(), "Bundle (pitch: Float)");
}

/// Two bundles holding the same members in another order are one type: each member is matched
/// by its name, and a meaning that differs is warned of under that member's name.
///
/// Mental-revert: pairing members by their place refuses the link, `pitch` meeting `gain`.
#[test]
fn bundles_holding_the_same_members_in_another_order_connect_member_by_name() {
    let member = |name: &str, meaning: &str| BundleMember {
        name: name.to_owned(),
        member_type: float(),
        meaning: Some(meaning.to_owned()),
        default_value: None,
    };
    let output = PinType::Bundle {
        members: vec![member("pitch", "frequency"), member("gain", "decibels")],
    };
    let input = PinType::Bundle {
        members: vec![member("gain", "decibels"), member("pitch", "note")],
    };
    assert_eq!(
        the_verdict_between(&output, &input),
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: vec![PinLinkWarning::MemberMeaningsDiffer {
                member_name: "pitch".to_owned(),
                output_meaning: "frequency".to_owned(),
                input_meaning: "note".to_owned(),
            }],
        }
    );
    let with_another_member = PinType::Bundle {
        members: vec![member("gain", "decibels"), member("pan", "position")],
    };
    assert!(matches!(
        the_outcome_between(&output, &with_another_member),
        PinLinkOutcome::Refused(_)
    ));
}

/// A bundle naming one member twice is not the bundle naming each of its members once, in
/// either direction, though every name of one is a name of the other.
#[test]
fn a_bundle_naming_a_member_twice_does_not_connect_to_one_naming_it_once() {
    let bundle_of = |names: &[&str]| PinType::Bundle {
        members: names
            .iter()
            .map(|name| BundleMember {
                name: (*name).to_owned(),
                member_type: float(),
                meaning: None,
                default_value: None,
            })
            .collect(),
    };
    for (output, input) in [
        (
            bundle_of(&["pitch", "pitch"]),
            bundle_of(&["pitch", "gain"]),
        ),
        (
            bundle_of(&["pitch", "gain"]),
            bundle_of(&["pitch", "pitch"]),
        ),
    ] {
        assert!(
            matches!(
                the_outcome_between(&output, &input),
                PinLinkOutcome::Refused(_)
            ),
            "{output} into {input}"
        );
    }
}

/// A bundle output with three members connects as it is to an input that asks for two of them,
/// and only the members the input asks for are compared, their meanings warned of.
///
/// Mental-revert: comparing the two bundles whole refuses the link, as the output holds `y`.
#[test]
fn a_bundle_output_connects_to_an_input_asking_for_fewer_members() {
    let x_and_opacity = a_bundle_of(&[("opacity", float()), ("x", float())]);
    assert_eq!(
        the_verdict_between(&a_placement(), &x_and_opacity),
        PinLinkVerdict {
            outcome: PinLinkOutcome::Connects,
            warnings: Vec::new(),
        }
    );
    assert_eq!(
        the_outcome_between(&a_list_of(a_placement()), &a_list_of(x_and_opacity.clone())),
        PinLinkOutcome::Connects
    );
    let tensors = a_bundle_of(&[
        ("boxes", PinType::Tensor {}),
        ("scores", PinType::Tensor {}),
        ("labels", PinType::Tensor {}),
    ]);
    assert_eq!(
        the_outcome_between(&tensors, &a_bundle_of(&[("scores", PinType::Tensor {})])),
        PinLinkOutcome::Connects
    );
    let mut output_meaning_y = a_placement();
    let mut input_meaning_y = a_bundle_of(&[("y", float())]);
    for (bundle, meaning) in [
        (&mut output_meaning_y, "height"),
        (&mut input_meaning_y, "depth"),
    ] {
        let PinType::Bundle { members } = bundle else {
            unreachable!("a bundle");
        };
        for member in members.iter_mut().filter(|member| member.name == "y") {
            member.meaning = Some(meaning.to_owned());
        }
    }
    assert_eq!(
        the_verdict_between(&output_meaning_y, &input_meaning_y).warnings,
        vec![PinLinkWarning::MemberMeaningsDiffer {
            member_name: "y".to_owned(),
            output_meaning: "height".to_owned(),
            input_meaning: "depth".to_owned(),
        }]
    );
}

/// An input asking for a member the output does not hold, or holds under another type, is
/// refused, naming each such member; a member is compared by its type exactly, so an Integer
/// member meets no Float one though the two numbers convert as whole pins.
///
/// Mental-revert: refusing with only the two bundles' types leaves a person to find `gain` among
/// them.
#[test]
fn an_input_asking_for_a_member_the_output_lacks_or_types_otherwise_is_refused_by_name() {
    let input = a_bundle_of(&[("x", float()), ("gain", float()), ("opacity", float())]);
    let output = a_bundle_of(&[("x", float()), ("opacity", integer())]);
    assert_eq!(
        the_refusal_words_between(&output, &input),
        "cannot link `source.out` (Bundle (x: Float, opacity: Integer)) to `sink.in` (Bundle \
         (x: Float, gain: Float, opacity: Float)): a Bundle (x: Float, opacity: Integer) output \
         does not connect to a Bundle (x: Float, gain: Float, opacity: Float) input: the input \
         asks for member `gain`, which the output does not hold; and member `opacity` as \
         Float, which the output holds as Integer",
    );
    let words = the_refusal_words_between(&a_list_of(a_placement()), &a_list_of(input));
    assert!(
        words.ends_with("the input asks for member `gain`, which the output does not hold"),
        "{words}"
    );
}

/// An output naming the member an input asks for twice does not hold it once, and the refusal
/// says so by the member's name.
#[test]
fn an_output_naming_an_asked_member_twice_is_refused_by_that_name() {
    let output = a_bundle_of(&[("x", float()), ("x", float())]);
    let words = the_refusal_words_between(&output, &a_bundle_of(&[("x", float())]));
    assert!(
        words.ends_with("the input asks for member `x`, which the output names more than once"),
        "{words}"
    );
}
