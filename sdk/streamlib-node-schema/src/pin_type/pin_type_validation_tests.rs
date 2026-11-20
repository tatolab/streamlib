// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::{BundleMember, FloatSize, IntegerSize, PinType, VectorComponentCount};

fn a_list_of(element_type: PinType) -> PinType {
    PinType::ListOf {
        element_type: Box::new(element_type),
    }
}

fn a_member(name: &str, member_type: PinType) -> BundleMember {
    BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value: None,
    }
}

#[test]
fn a_list_of_each_small_type_is_a_pin_type() {
    for element_type in [
        PinType::Boolean {},
        PinType::Integer { exact_size: None },
        PinType::Float {
            exact_size: Some(FloatSize::Float32),
        },
        PinType::String {},
        PinType::Menu {
            option_names: vec!["sine".to_owned()],
        },
        PinType::Color {},
        PinType::Vector {
            component_count: VectorComponentCount::Three,
        },
        PinType::Rectangle {},
        PinType::Transform {},
        PinType::Asset { media: vec![] },
    ] {
        assert_eq!(
            a_list_of(element_type.clone()).validate(),
            Ok(()),
            "{element_type}"
        );
    }
}

#[test]
fn a_64_bit_integer_types_an_element_and_no_single_pin() {
    for exact_size in [IntegerSize::Int64, IntegerSize::UInt64] {
        let integer = PinType::Integer {
            exact_size: Some(exact_size),
        };
        assert_eq!(a_list_of(integer.clone()).validate(), Ok(()));
        let refusal = integer.validate().unwrap_err();
        assert!(refusal.contains(exact_size.variant_name()), "{refusal}");
    }
}

#[test]
fn a_list_of_a_list_is_refused() {
    let refusal = a_list_of(a_list_of(PinType::Float { exact_size: None }))
        .validate()
        .unwrap_err();
    assert!(refusal.contains("a List of a List"), "{refusal}");
}

#[test]
fn a_list_of_a_stream_or_event_type_is_refused_naming_it() {
    for element_type in [
        PinType::Trigger {},
        PinType::Texture {
            pixel_formats: vec![],
        },
        PinType::Audio {},
        PinType::EncodedVideo { codecs: vec![] },
        PinType::EncodedAudio { codecs: vec![] },
        PinType::Tensor {},
    ] {
        let refusal = a_list_of(element_type.clone()).validate().unwrap_err();
        assert!(
            refusal.contains(&element_type.to_string()) && refusal.contains("small type"),
            "{refusal}"
        );
    }
}

#[test]
fn a_list_of_a_bundle_of_small_types_is_a_pin_type_and_of_a_bundle_holding_a_list_is_not() {
    let placement = PinType::Bundle {
        members: vec![
            a_member("x", PinType::Integer { exact_size: None }),
            a_member("opacity", PinType::Float { exact_size: None }),
        ],
    };
    assert_eq!(a_list_of(placement).validate(), Ok(()));
    let holding_a_list = PinType::Bundle {
        members: vec![a_member(
            "points",
            a_list_of(PinType::Float { exact_size: None }),
        )],
    };
    assert_eq!(holding_a_list.validate(), Ok(()));
    assert!(a_list_of(holding_a_list).validate().is_err());
}

#[test]
fn a_bundle_inside_a_bundle_is_refused_and_a_bundle_of_tensors_is_not() {
    let inner = PinType::Bundle {
        members: vec![a_member("x", PinType::Float { exact_size: None })],
    };
    let refusal = PinType::Bundle {
        members: vec![a_member("inner", inner)],
    }
    .validate()
    .unwrap_err();
    assert!(refusal.contains("`inner`"), "{refusal}");
    let tensors = PinType::Bundle {
        members: vec![
            a_member("boxes", PinType::Tensor {}),
            a_member("scores", PinType::Tensor {}),
        ],
    };
    assert_eq!(tensors.validate(), Ok(()));
}

#[test]
fn a_menu_with_no_option_is_refused() {
    assert!(
        PinType::Menu {
            option_names: vec![]
        }
        .validate()
        .is_err()
    );
}

/// A member of a type a bundle cannot hold yet is refused at declaration, by the member's name.
#[test]
fn a_bundle_member_of_a_stream_or_event_type_is_refused_by_its_name() {
    for member_type in [
        PinType::Texture {
            pixel_formats: vec![],
        },
        PinType::Audio {},
        PinType::EncodedVideo { codecs: vec![] },
        PinType::EncodedAudio { codecs: vec![] },
        PinType::Trigger {},
    ] {
        let refusal = PinType::Bundle {
            members: vec![
                a_member("opacity", PinType::Float { exact_size: None }),
                a_member("frame", member_type.clone()),
            ],
        }
        .validate()
        .unwrap_err();
        assert!(
            refusal.contains("`frame`") && refusal.contains(&member_type.to_string()),
            "{refusal}"
        );
    }
}

/// A bundle naming one member twice is refused by that name, as its stored value is one object
/// keyed by member; so is a List element that is such a bundle.
///
/// Mental-revert: without the check, two `gain` members validate and one of them is never read.
#[test]
fn a_bundle_naming_a_member_twice_is_refused_by_that_name() {
    let named_twice = PinType::Bundle {
        members: vec![
            a_member("gain", PinType::Float { exact_size: None }),
            a_member("pan", PinType::Float { exact_size: None }),
            a_member("gain", PinType::Integer { exact_size: None }),
        ],
    };
    for pin_type in [named_twice.clone(), a_list_of(named_twice)] {
        let refusal = pin_type.validate().unwrap_err();
        assert!(
            refusal.contains("`gain`") && refusal.contains("more than once"),
            "{refusal}"
        );
    }
}
