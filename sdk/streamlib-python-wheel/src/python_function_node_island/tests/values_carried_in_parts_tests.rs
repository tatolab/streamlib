// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::super::function_node_call_shapes::HowAFunctionNodeIsCalled;
use super::super::function_node_values_carried_in_parts::{SlotsOfEachValue, ValueCarriedInParts};
use super::*;

/// Takes a Vector2 `center` and a number `brightness`, and gives the Color of `center.x`,
/// `center.y` and `brightness`, opaque, then the number `center.x + center.y`.
unsafe extern "C" fn color_of_a_center_and_a_brightness(
    input_values: *const f64,
    _config_values: *const f64,
    _state_values: *mut f64,
    output_values: *mut f64,
    _now_ns: i64,
) -> i32 {
    unsafe {
        let inputs = std::slice::from_raw_parts(input_values, 3);
        let outputs = std::slice::from_raw_parts_mut(output_values, 5);
        outputs[0] = inputs[0];
        outputs[1] = inputs[1];
        outputs[2] = inputs[2];
        outputs[3] = 1.0;
        outputs[4] = inputs[0] + inputs[1];
    }
    MODULE_HOST_TABLE_STATUS_OK
}

fn carried_in_parts(entries: &[(usize, &str)]) -> Vec<(usize, String)> {
    entries
        .iter()
        .map(|(index, value_type)| (*index, (*value_type).to_owned()))
        .collect()
}

#[test]
fn each_value_carried_in_parts_takes_one_slot_per_part_in_port_order() {
    let slots = SlotsOfEachValue::of(
        "probe",
        "input",
        4,
        &carried_in_parts(&[(1, "color"), (3, "vector2")]),
    )
    .unwrap();

    assert_eq!(slots.count(), 4);
    assert_eq!(slots.slot_count(), 1 + 4 + 1 + 2);
    assert_eq!(
        (0..4)
            .map(|index| slots.first_slot_of(index))
            .collect::<Vec<_>>(),
        vec![0, 1, 5, 6]
    );
    assert_eq!(slots.carried_in_parts(1), Some(ValueCarriedInParts::Color));
    assert_eq!(slots.carried_in_parts(2), None);
}

#[test]
fn a_value_named_past_the_last_port_or_of_a_type_with_no_parts_is_refused() {
    let past_the_last =
        SlotsOfEachValue::of("probe", "output", 1, &carried_in_parts(&[(1, "color")])).unwrap_err();
    assert!(past_the_last.contains("it has 1"), "{past_the_last}");

    let no_parts =
        SlotsOfEachValue::of("probe", "input", 1, &carried_in_parts(&[(0, "float")])).unwrap_err();
    assert!(no_parts.contains("carries no parts"), "{no_parts}");
}

#[test]
fn an_input_reads_another_size_of_vector_or_color_and_a_number_with_the_free_conversion() {
    let read = |parts: ValueCarriedInParts, value: FunctionNodeValue| {
        let mut slots = vec![-1.0; parts.part_count()];
        parts
            .read_into(&mut slots, &value, "probe", 0)
            .map(|()| slots)
    };
    let vector2 = ValueCarriedInParts::Vector { component_count: 2 };
    let vector3 = ValueCarriedInParts::Vector { component_count: 3 };

    assert_eq!(
        read(vector2, FunctionNodeValue::Parts(vec![1.0, 2.0, 3.0, 4.0])).unwrap(),
        vec![1.0, 2.0]
    );
    assert_eq!(
        read(vector3, FunctionNodeValue::Parts(vec![1.0, 2.0])).unwrap(),
        vec![1.0, 2.0, 0.0]
    );
    assert_eq!(
        read(
            ValueCarriedInParts::Color,
            FunctionNodeValue::Parts(vec![0.1, 0.2, 0.3])
        )
        .unwrap(),
        vec![0.1, 0.2, 0.3, 1.0]
    );
    assert_eq!(
        read(ValueCarriedInParts::Color, FunctionNodeValue::Number(0.5)).unwrap(),
        vec![0.5, 0.5, 0.5, 1.0]
    );
    assert_eq!(
        read(vector2, FunctionNodeValue::Number(0.5)).unwrap(),
        vec![0.5, 0.5]
    );
}

#[test]
fn a_rectangle_input_reads_exactly_its_four_parts() {
    let rectangle = ValueCarriedInParts::Rectangle;
    let mut slots = vec![0.0; 4];

    rectangle
        .read_into(
            &mut slots,
            &FunctionNodeValue::Parts(vec![1.0, 2.0, 3.0, 4.0]),
            "probe",
            0,
        )
        .unwrap();
    assert_eq!(slots, vec![1.0, 2.0, 3.0, 4.0]);

    let refused = rectangle
        .read_into(
            &mut slots,
            &FunctionNodeValue::Parts(vec![1.0, 2.0]),
            "probe",
            0,
        )
        .unwrap_err()
        .to_string();
    assert!(refused.contains("takes 4 numbers on input 0"), "{refused}");
    let refused = rectangle
        .read_into(&mut slots, &FunctionNodeValue::Number(1.0), "probe", 0)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("was handed the number 1"), "{refused}");
}

#[test]
fn a_compiled_call_hands_its_entry_each_part_and_gives_a_value_of_parts() {
    let how_it_carries_its_values = HowAFunctionNodeCarriesValuesBeyondNumbers {
        inputs_carried_in_parts: carried_in_parts(&[(0, "vector2")]),
        outputs_carried_in_parts: carried_in_parts(&[(0, "color")]),
        ..HowAFunctionNodeCarriesValuesBeyondNumbers::default()
    };
    let mut how_it_is_called = HowAFunctionNodeIsCalled::handed_over_by_the_bootstrap(
        "probe",
        compiled_to(color_of_a_center_and_a_brightness),
        vec![],
        vec![],
        2,
        vec![],
        false,
        2,
        &how_it_carries_its_values,
    )
    .unwrap();
    assert_eq!(how_it_is_called.input_count(), 2);

    let center = FunctionNodeValue::Parts(vec![0.25, 0.75, 9.0, 9.0]);
    let brightness = FunctionNodeValue::Number(0.5);
    let mut output_values = vec![FunctionNodeValue::Nothing, FunctionNodeValue::Nothing];
    how_it_is_called
        .call_at("probe", &[&center, &brightness], &mut output_values, 0)
        .map_err(|failure| failure.described().raising_because)
        .unwrap();

    assert!(
        matches!(&output_values[0], FunctionNodeValue::Parts(parts) if parts == &[0.25, 0.75, 0.5, 1.0]),
        "{:?}",
        output_values[0]
    );
    assert!(
        matches!(output_values[1], FunctionNodeValue::Number(sum) if sum == 1.0),
        "{:?}",
        output_values[1]
    );
}
