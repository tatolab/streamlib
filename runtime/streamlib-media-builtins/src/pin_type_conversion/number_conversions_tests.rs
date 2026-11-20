// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};
use streamlib::sdk::descriptors::{
    NodeClassImportPath, PinType, SavedValueRetyped, the_saved_value_retyped,
};
use streamlib::sdk::nodes::NODE_REGISTRY;
use streamlib::sdk::pin_value_packing::{pack_pin_value, unpack_pin_value};

use super::number_conversions::the_message_cast;

fn float() -> PinType {
    PinType::Float { exact_size: None }
}

fn integer() -> PinType {
    PinType::Integer { exact_size: None }
}

/// What becomes of `value` of `from` retyped into `into` among the conversions the process-wide
/// registry holds once the built-ins are registered.
fn retyped_by_the_registry(value: Value, from: &PinType, into: &PinType) -> SavedValueRetyped {
    crate::register_media_builtin_node_types();
    the_saved_value_retyped(&value, from, into, &NODE_REGISTRY.pin_conversions())
}

fn cast_by(conversion: &str, value: Value) -> SavedValueRetyped {
    SavedValueRetyped::CastBy {
        conversion: NodeClassImportPath::of_built_in_node(conversion),
        value,
    }
}

/// Each saved number is cast by the one registered number conversion between its two types.
///
/// Mental-revert: registering the number conversions with no saved-value cast gives way to each
/// new type's default, since a conversion with no cast runs only inside a started stream.
#[test]
fn a_saved_number_is_cast_by_the_registered_number_conversion_between_its_two_types() {
    let boolean = PinType::Boolean {};
    for (value, from, into, expected) in [
        (
            json!(0.1),
            float(),
            integer(),
            cast_by("FloatToInteger", json!(0)),
        ),
        (
            json!(2),
            integer(),
            float(),
            cast_by("IntegerToFloat", json!(2.0)),
        ),
        (
            json!(true),
            boolean.clone(),
            integer(),
            cast_by("BooleanToInteger", json!(1)),
        ),
        (
            json!(0),
            integer(),
            boolean.clone(),
            cast_by("IntegerToBoolean", json!(false)),
        ),
        (
            json!(false),
            boolean.clone(),
            float(),
            cast_by("BooleanToFloat", json!(0.0)),
        ),
        (
            json!(-0.4),
            float(),
            boolean,
            cast_by("FloatToBoolean", json!(true)),
        ),
    ] {
        assert_eq!(
            retyped_by_the_registry(value.clone(), &from, &into),
            expected,
            "{value} from {from} into {into}"
        );
    }
}

/// A message through `FloatToInteger` is cast the way a saved value is: truncated toward zero.
#[test]
fn a_message_through_a_number_conversion_is_cast_as_a_saved_value_is() {
    let message = pack_pin_value(&float(), &json!(-2.9), 7).expect("a Float packs");
    let cast = the_message_cast(
        &float(),
        &integer(),
        &message.payload_descriptor,
        &message.payload,
    )
    .expect("a Float casts into an Integer");
    assert_eq!(unpack_pin_value(&integer(), cast.as_ref()), Ok(json!(-2)));
}
