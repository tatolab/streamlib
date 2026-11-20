// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::NodeClassImportPath;

fn float() -> PinType {
    PinType::Float { exact_size: None }
}

fn integer() -> PinType {
    PinType::Integer { exact_size: None }
}

fn sized_integer(size: IntegerSize) -> PinType {
    PinType::Integer {
        exact_size: Some(size),
    }
}

fn a_sweep() -> PinType {
    serde_json::from_value(json!({ "type": "named", "named_pin_type": "nodes.sweep:Sweep" }))
        .expect("a named type")
}

/// A conversion `name` from `from` into `into`, casting a saved value with `cast` when given.
fn a_conversion(
    name: &str,
    from: PinType,
    into: PinType,
    cast: Option<PinConversionSavedValueCast>,
) -> PinConversion {
    PinConversion {
        node_type: NodeClassImportPath::of_built_in_node(name),
        input_port_name: "in".to_owned(),
        input_pin_type: from,
        output_port_name: "out".to_owned(),
        output_pin_type: into,
        output_picked_for_the_input: None,
        saved_value_cast: cast,
    }
}

const NUMBER_CAST: PinConversionSavedValueCast = PinConversionSavedValueCast {
    cast: the_number_cast,
};

#[test]
fn a_float_cast_into_an_integer_is_truncated_toward_zero() {
    for (float_value, whole) in [(0.1, 0), (2.9, 2), (-2.9, -2), (-0.5, 0)] {
        assert_eq!(
            the_number_cast(&json!(float_value), &integer()),
            Ok(json!(whole)),
            "{float_value}"
        );
    }
}

#[test]
fn a_float_cast_into_an_integer_is_held_to_the_integers_size() {
    assert_eq!(
        the_number_cast(&json!(300.7), &sized_integer(IntegerSize::UInt8)),
        Ok(json!(255))
    );
    assert_eq!(
        the_number_cast(&json!(-4.0), &sized_integer(IntegerSize::UInt8)),
        Ok(json!(0))
    );
    assert_eq!(
        the_number_cast(&json!(1e300), &integer()),
        Ok(json!(1_i64 << 53))
    );
}

#[test]
fn an_integer_cast_into_a_float_is_exact() {
    let cast = the_number_cast(&json!(2), &float()).expect("cast");
    assert_eq!(cast, json!(2.0));
    assert!(cast.as_f64().is_some() && cast.as_i64().is_none(), "{cast}");
    assert_eq!(
        the_number_cast(&json!(9_007_199_254_740_993_i64), &float()),
        Ok(json!(9_007_199_254_740_992.0))
    );
}

#[test]
fn a_boolean_is_cast_into_a_number_as_zero_or_one_and_back() {
    assert_eq!(the_number_cast(&json!(true), &integer()), Ok(json!(1)));
    assert_eq!(the_number_cast(&json!(false), &float()), Ok(json!(0.0)));
    assert_eq!(
        the_number_cast(&json!(0), &PinType::Boolean {}),
        Ok(json!(false))
    );
    assert_eq!(
        the_number_cast(&json!(0.25), &PinType::Boolean {}),
        Ok(json!(true))
    );
}

#[test]
fn a_value_no_number_is_not_cast() {
    assert!(the_number_cast(&json!("0.1"), &integer()).is_err());
    assert!(the_number_cast(&json!(1), &PinType::String {}).is_err());
}

/// Mental-revert: judging a conversion by the link verdict's free conversions fits
/// `IntegerToBoolean` and every other number conversion too, which is no single fit.
#[test]
fn the_one_conversion_whose_types_are_the_two_casts_the_saved_value() {
    let conversions = [
        a_conversion("FloatToInteger", float(), integer(), Some(NUMBER_CAST)),
        a_conversion("IntegerToFloat", integer(), float(), Some(NUMBER_CAST)),
        a_conversion(
            "IntegerToBoolean",
            integer(),
            PinType::Boolean {},
            Some(NUMBER_CAST),
        ),
    ];
    assert_eq!(
        the_saved_value_retyped(&json!(0.1), &float(), &integer(), &conversions),
        SavedValueRetyped::CastBy {
            conversion: NodeClassImportPath::of_built_in_node("FloatToInteger"),
            value: json!(0),
        }
    );
    assert_eq!(
        the_saved_value_retyped(
            &json!(-7.5),
            &float(),
            &sized_integer(IntegerSize::Int8),
            &conversions
        ),
        SavedValueRetyped::CastBy {
            conversion: NodeClassImportPath::of_built_in_node("FloatToInteger"),
            value: json!(-7),
        },
        "a size of the conversion's output type fits it"
    );
}

#[test]
fn more_than_one_fitting_conversion_casts_nothing_and_names_each() {
    let conversions = [
        a_conversion("SweepPosition", a_sweep(), float(), Some(NUMBER_CAST)),
        a_conversion("SweepWidth", a_sweep(), float(), Some(NUMBER_CAST)),
    ];
    let SavedValueRetyped::TakesTheDefault(why) =
        the_saved_value_retyped(&json!({}), &a_sweep(), &float(), &conversions)
    else {
        panic!("no conversion is picked");
    };
    assert_eq!(
        why.conversions_that_fit(),
        [
            &NodeClassImportPath::of_built_in_node("SweepPosition"),
            &NodeClassImportPath::of_built_in_node("SweepWidth"),
        ]
    );
    let why = why.to_string();
    assert!(why.contains("SweepPosition`, `"), "{why}");
    assert!(why.contains("none is picked"), "{why}");
}

#[test]
fn a_conversion_that_supplies_no_cast_gives_way_to_the_default_saying_so() {
    let conversions = [a_conversion("SweepPosition", a_sweep(), float(), None)];
    let retyped = the_saved_value_retyped(&json!({}), &a_sweep(), &float(), &conversions);
    assert_eq!(
        retyped,
        SavedValueRetyped::TakesTheDefault(
            WhyASavedValueTakesTheDefault::TheConversionRunsOnlyInAStartedStream(
                NodeClassImportPath::of_built_in_node("SweepPosition")
            )
        )
    );
}

#[test]
fn with_no_conversion_a_value_the_new_type_takes_is_kept_and_any_other_gives_way() {
    let menu = PinType::Menu {
        option_names: vec!["warm".to_owned()],
    };
    assert_eq!(
        the_saved_value_retyped(&json!("warm"), &menu, &PinType::String {}, &[]),
        SavedValueRetyped::TakenAsItIs
    );
    assert_eq!(
        the_saved_value_retyped(&json!(0.1), &float(), &PinType::String {}, &[]),
        SavedValueRetyped::TakesTheDefault(WhyASavedValueTakesTheDefault::NoConversionFits)
    );
}

#[test]
fn a_cast_the_new_type_does_not_take_gives_way_to_the_default() {
    let conversions = [a_conversion(
        "FloatToString",
        float(),
        PinType::String {},
        Some(NUMBER_CAST),
    )];
    let SavedValueRetyped::TakesTheDefault(why) =
        the_saved_value_retyped(&json!(0.1), &float(), &PinType::String {}, &conversions)
    else {
        panic!("the number cast gives no String");
    };
    assert!(
        matches!(
            why,
            WhyASavedValueTakesTheDefault::TheConversionRefusedTheValue { .. }
        ),
        "{why:?}"
    );
}

#[test]
fn a_number_is_held_within_the_range() {
    assert_eq!(
        the_value_held_within_the_range(json!(5), &integer(), (0.5, 3.5)),
        json!(3)
    );
    assert_eq!(
        the_value_held_within_the_range(json!(-1), &integer(), (0.5, 3.5)),
        json!(1)
    );
    assert_eq!(
        the_value_held_within_the_range(json!(2.0), &float(), (0.0, 1.0)),
        json!(1.0)
    );
    assert_eq!(
        the_value_held_within_the_range(json!(0), &integer(), (0.0, 1.0)),
        json!(0)
    );
    assert_eq!(
        the_value_held_within_the_range(json!("a"), &PinType::String {}, (0.0, 1.0)),
        json!("a")
    );
}
