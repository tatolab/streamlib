// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[function_node]` declares the node `@node.function` would from the same
//! signature, and its evaluate entry computes what the function does.

#![cfg(feature = "function-node")]

use tatolab_module_sdk::function_node::{
    FunctionNodeDeclaration, FunctionNodeInputPin, FunctionNodeNamedValue, FunctionNodeOutput,
    FunctionNodeOutputPort, FunctionNodeRecord, FunctionNodeRecordFieldWithItsDefault,
    FunctionNodeValueType, State, Stateful, Time, UnreadMessage, function_node,
    refuse_an_unread_message_output_following_no_one_input,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_OK, MODULE_HOST_TABLE_STATUS_REFUSED,
};

const SCALE_AND_OFFSET_DESCRIPTION: &str = "`x` scaled by `gain`, then moved by `offset`.";

/// The doc comment, which is never the node's description.
#[function_node]
#[meta(description = SCALE_AND_OFFSET_DESCRIPTION)]
fn scale_and_offset(x: f64, #[pin_default(2.0)] gain: f64, #[pin_default(0.5)] offset: f64) -> f64 {
    x * gain + offset
}

#[derive(Clone, Copy, FunctionNodeRecord)]
struct RunningTotal {
    total: f64,
    calls: i64,
    started: bool,
}

impl Default for RunningTotal {
    fn default() -> Self {
        Self {
            total: 10.0,
            calls: 0,
            started: false,
        }
    }
}

#[derive(Clone, Copy, Default, FunctionNodeRecord)]
struct AccumulateConfig {
    weight: f64,
}

#[derive(Clone, Copy, Default, FunctionNodeRecord)]
struct TotalAndWhenItChanged {
    total: f64,
    changed_at_ns: i64,
}

/// Add `x` times the config's weight into a running total.
#[function_node]
fn accumulate(
    x: f64,
    config: AccumulateConfig,
    now: Time,
    previous: State<RunningTotal>,
) -> Stateful<TotalAndWhenItChanged, RunningTotal> {
    let total = previous.total + x * config.weight;
    (
        TotalAndWhenItChanged {
            total,
            changed_at_ns: now,
        },
        RunningTotal {
            total,
            calls: previous.calls + 1,
            started: true,
        },
    )
}

#[function_node]
fn output_values(input_values: f64) -> f64 {
    input_values * 3.0
}

#[derive(Clone, Copy, Default)]
struct PanicsWritingItsSecondValue {
    first: f64,
}

impl FunctionNodeOutput for PanicsWritingItsSecondValue {
    const OUTPUT_PORTS: &'static [FunctionNodeOutputPort] = &[
        FunctionNodeOutputPort::handing_out::<f64>("first"),
        FunctionNodeOutputPort::handing_out::<f64>("second"),
    ];

    fn write_function_node_output_values(&self, output_values: &mut [f64]) {
        output_values[0] = self.first;
        panic!("a writer that fails part-way");
    }
}

#[function_node]
fn writes_half_its_outputs_then_panics(x: f64) -> PanicsWritingItsSecondValue {
    PanicsWritingItsSecondValue { first: x }
}

#[function_node]
fn panics_on_a_negative_input(x: f64) -> bool {
    assert!(x >= 0.0, "a negative input");
    true
}

/// `value` handed on while `open`, and nothing while closed.
#[function_node]
fn gate(value: UnreadMessage, #[pin_default(true)] open: bool) -> Option<UnreadMessage> {
    open.then_some(value)
}

/// `message` handed on while `count` is positive, called even when `message` holds nothing.
#[function_node]
fn hand_on_while_counting(message: Option<UnreadMessage>, count: i64) -> Option<UnreadMessage> {
    message.filter(|_| count > 0)
}

/// The reciprocal of `x`, or nothing at zero.
#[function_node]
fn reciprocal(x: f64) -> Option<f64> {
    (x != 0.0).then(|| 1.0 / x)
}

#[derive(Clone, Copy, FunctionNodeRecord)]
struct ThresholdCrossings {
    level: f64,
    rose_at_ns: Option<i64>,
    fell: Option<bool>,
}

/// The level, and when it last rose past the threshold or fell below it.
#[function_node]
fn threshold_crossings(level: f64, threshold: f64, now: Time) -> ThresholdCrossings {
    ThresholdCrossings {
        level,
        rose_at_ns: (level > threshold).then_some(now),
        fell: (level < threshold).then_some(true),
    }
}

/// Call `declaration`'s evaluate entry with `input_values`, answering its status and its output slots.
fn evaluate_statelessly(
    declaration: &tatolab_module_sdk::function_node::FunctionNodeDeclaration,
    input_values: &[f64],
    output_slot_count: usize,
) -> (i32, Vec<f64>) {
    let mut output_values = vec![7.0; output_slot_count];
    // SAFETY: one value per input pin, one slot per output port and handed-out flag, none of config or state.
    let status = unsafe {
        (declaration.evaluate_entry)(
            input_values.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            output_values.as_mut_ptr(),
            0,
        )
    };
    (status, output_values)
}

#[test]
fn a_stateless_function_node_declares_its_pins_and_its_result() {
    let declaration = scale_and_offset_function_node_declaration();
    assert_eq!(declaration.short_name, "ScaleAndOffset");
    assert_eq!(
        declaration.description,
        "`x` scaled by `gain`, then moved by `offset`."
    );
    assert_eq!(
        declaration.input_pins,
        vec![
            FunctionNodeInputPin {
                port: "x",
                value_type: FunctionNodeValueType::Float,
                pin_default: None,
                takes_none: false,
            },
            FunctionNodeInputPin {
                port: "gain",
                value_type: FunctionNodeValueType::Float,
                pin_default: Some(2.0),
                takes_none: false,
            },
            FunctionNodeInputPin {
                port: "offset",
                value_type: FunctionNodeValueType::Float,
                pin_default: Some(0.5),
                takes_none: false,
            },
        ]
    );
    assert_eq!(
        declaration.output_ports,
        vec![FunctionNodeOutputPort {
            name: "result",
            value_type: FunctionNodeValueType::Float,
            may_hand_out_nothing: false,
        }]
    );
    assert!(declaration.config_fields.is_empty());
    assert!(declaration.state_fields.is_empty());
    assert_eq!(declaration.stream_time_parameter, None);
    assert_eq!(declaration.state_parameter, None);
    assert_eq!(declaration.config_parameter, None);
    assert_eq!(
        scale_and_offset(3.0, 2.0, 0.5),
        6.5,
        "the function is still called as itself"
    );
}

#[test]
fn a_stateless_function_node_evaluates_through_its_entry() {
    let declaration = scale_and_offset_function_node_declaration();
    let input_values = [3.0, 4.0, 1.0];
    let mut output_values = [0.0];
    // SAFETY: one value per input pin and per output port, none of config or state.
    let status = unsafe {
        (declaration.evaluate_entry)(
            input_values.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            output_values.as_mut_ptr(),
            0,
        )
    };
    assert_eq!(status, MODULE_HOST_TABLE_STATUS_OK);
    assert_eq!(output_values, [13.0]);
}

#[test]
fn a_stateful_function_node_declares_its_config_state_time_and_record_outputs() {
    let declaration = accumulate_function_node_declaration();
    assert_eq!(declaration.short_name, "Accumulate");
    assert_eq!(declaration.stream_time_parameter, Some("now"));
    assert_eq!(declaration.state_parameter, Some("previous"));
    assert_eq!(declaration.config_parameter, Some("config"));
    assert_eq!(
        declaration.output_ports,
        vec![
            FunctionNodeOutputPort {
                name: "total",
                value_type: FunctionNodeValueType::Float,
                may_hand_out_nothing: false,
            },
            FunctionNodeOutputPort {
                name: "changed_at_ns",
                value_type: FunctionNodeValueType::Int,
                may_hand_out_nothing: false,
            },
        ]
    );
    assert_eq!(
        declaration.config_fields,
        vec![FunctionNodeRecordFieldWithItsDefault {
            field: FunctionNodeNamedValue {
                name: "weight",
                value_type: FunctionNodeValueType::Float,
            },
            default_value: 0.0,
        }]
    );
    let state_fields_and_their_first_values: Vec<(&str, FunctionNodeValueType, f64)> = declaration
        .state_fields
        .iter()
        .map(|state_field| {
            (
                state_field.field.name,
                state_field.field.value_type,
                state_field.default_value,
            )
        })
        .collect();
    assert_eq!(
        state_fields_and_their_first_values,
        vec![
            ("total", FunctionNodeValueType::Float, 10.0),
            ("calls", FunctionNodeValueType::Int, 0.0),
            ("started", FunctionNodeValueType::Bool, 0.0),
        ]
    );
}

#[test]
fn a_stateful_function_node_reads_and_rewrites_its_state_through_its_entry() {
    let declaration = accumulate_function_node_declaration();
    let config_values = [0.5];
    let mut state_values: Vec<f64> = declaration
        .state_fields
        .iter()
        .map(|state_field| state_field.default_value)
        .collect();
    let mut output_values = [0.0; 2];
    for (input_value, now_ns) in [(4.0, 100), (6.0, 250)] {
        // SAFETY: one value per input pin, config field, state field and output port.
        let status = unsafe {
            (declaration.evaluate_entry)(
                [input_value].as_ptr(),
                config_values.as_ptr(),
                state_values.as_mut_ptr(),
                output_values.as_mut_ptr(),
                now_ns,
            )
        };
        assert_eq!(status, MODULE_HOST_TABLE_STATUS_OK);
    }
    assert_eq!(output_values, [15.0, 250.0]);
    assert_eq!(state_values, vec![15.0, 2.0, 1.0]);
    assert_eq!(
        RunningTotal::from_function_node_values(&state_values).calls,
        2,
        "an int field comes back as itself"
    );
}

#[test]
fn a_panicking_call_is_refused_and_writes_nothing() {
    let declaration = panics_on_a_negative_input_function_node_declaration();
    let mut output_values = [7.0];
    // SAFETY: one value per input pin and per output port.
    let status = unsafe {
        (declaration.evaluate_entry)(
            [-1.0].as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            output_values.as_mut_ptr(),
            0,
        )
    };
    assert_eq!(status, MODULE_HOST_TABLE_STATUS_REFUSED);
    assert_eq!(output_values, [7.0]);
}

#[test]
fn a_function_named_like_the_entrys_own_values_still_evaluates() {
    let declaration = output_values_function_node_declaration();
    let mut evaluated = [0.0];
    // SAFETY: one value per input pin and per output port.
    let status = unsafe {
        (declaration.evaluate_entry)(
            [2.0].as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            evaluated.as_mut_ptr(),
            0,
        )
    };
    assert_eq!(status, MODULE_HOST_TABLE_STATUS_OK);
    assert_eq!(evaluated, [6.0]);
}

#[test]
fn an_output_writer_that_panics_part_way_leaves_every_output_unwritten() {
    let declaration = writes_half_its_outputs_then_panics_function_node_declaration();
    let mut output_values = [7.0, 7.0];
    // SAFETY: one value per input pin and per output port.
    let status = unsafe {
        (declaration.evaluate_entry)(
            [1.0].as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            output_values.as_mut_ptr(),
            0,
        )
    };
    assert_eq!(status, MODULE_HOST_TABLE_STATUS_REFUSED);
    assert_eq!(output_values, [7.0, 7.0]);
}

#[cfg(feature = "pyo3")]
#[test]
fn a_declaration_reaches_python_as_the_dict_node_rust_function_reads() {
    use pyo3::prelude::*;
    use pyo3::types::PyDict;
    use tatolab_module_sdk::{MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR};

    Python::attach(|python| {
        let declaration = accumulate_function_node_declaration();
        let python_declaration = declaration
            .to_python_declaration(python)
            .expect("the declaration converts");
        let item = |key: &str| {
            python_declaration
                .get_item(key)
                .expect("a dict lookup")
                .unwrap_or_else(|| panic!("the declaration carries `{key}`"))
        };
        assert_eq!(
            item("short_name").extract::<String>().unwrap(),
            "Accumulate"
        );
        assert_eq!(
            item("evaluate_entry_address").extract::<usize>().unwrap(),
            declaration.evaluate_entry as usize
        );
        assert_eq!(
            item("stream_time_parameter")
                .extract::<Option<String>>()
                .unwrap(),
            Some("now".to_owned())
        );
        let first_state_field = item("state_fields")
            .get_item(0)
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        assert_eq!(
            first_state_field
                .get_item("default")
                .unwrap()
                .unwrap()
                .extract::<f64>()
                .unwrap(),
            10.0
        );
        assert!(
            item("input_pins")
                .get_item(0)
                .unwrap()
                .get_item("pin_default")
                .unwrap()
                .is_none(),
            "an input with no default must be wired"
        );
        assert_eq!(
            (
                item("module_host_table_abi_major")
                    .extract::<u32>()
                    .unwrap(),
                item("module_host_table_abi_minor")
                    .extract::<u32>()
                    .unwrap()
            ),
            (MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR)
        );
    });
}

#[test]
fn a_gate_declares_an_unread_message_pin_and_an_output_that_may_hand_out_nothing() {
    let declaration = gate_function_node_declaration();
    assert_eq!(declaration.short_name, "Gate");
    assert_eq!(
        declaration.input_pins,
        vec![
            FunctionNodeInputPin {
                port: "value",
                value_type: FunctionNodeValueType::UnreadMessage,
                pin_default: None,
                takes_none: false,
            },
            FunctionNodeInputPin {
                port: "open",
                value_type: FunctionNodeValueType::Bool,
                pin_default: Some(1.0),
                takes_none: false,
            },
        ]
    );
    assert_eq!(
        declaration.output_ports,
        vec![FunctionNodeOutputPort {
            name: "result",
            value_type: FunctionNodeValueType::UnreadMessage,
            may_hand_out_nothing: true,
        }]
    );
}

#[test]
fn an_open_gate_hands_on_the_token_of_its_message_and_a_closed_one_hands_out_nothing() {
    let declaration = gate_function_node_declaration();
    let token_of_the_first_input = 1.0;
    assert_eq!(
        evaluate_statelessly(&declaration, &[token_of_the_first_input, 1.0], 2),
        (
            MODULE_HOST_TABLE_STATUS_OK,
            vec![token_of_the_first_input, 1.0]
        ),
        "the output slot carries the token, and the flag past it says it handed one out"
    );
    assert_eq!(
        evaluate_statelessly(&declaration, &[token_of_the_first_input, 0.0], 2),
        (MODULE_HOST_TABLE_STATUS_OK, vec![0.0, 0.0]),
    );
}

#[test]
fn an_optional_unread_message_pin_takes_none_when_it_holds_nothing() {
    let declaration = hand_on_while_counting_function_node_declaration();
    assert_eq!(
        declaration
            .input_pins
            .iter()
            .map(|input_pin| (input_pin.port, input_pin.value_type, input_pin.takes_none))
            .collect::<Vec<_>>(),
        vec![
            ("message", FunctionNodeValueType::UnreadMessage, true),
            ("count", FunctionNodeValueType::Int, false),
        ]
    );
    assert_eq!(
        evaluate_statelessly(&declaration, &[1.0, 3.0], 2),
        (MODULE_HOST_TABLE_STATUS_OK, vec![1.0, 1.0])
    );
    assert_eq!(
        evaluate_statelessly(&declaration, &[0.0, 3.0], 2),
        (MODULE_HOST_TABLE_STATUS_OK, vec![0.0, 0.0]),
        "a message pin holding nothing reaches the body as `None`"
    );
    assert_eq!(
        evaluate_statelessly(&declaration, &[1.0, 0.0], 2),
        (MODULE_HOST_TABLE_STATUS_OK, vec![0.0, 0.0])
    );
}

#[test]
fn a_number_that_may_be_none_writes_its_handed_out_flag_past_its_output() {
    let declaration = reciprocal_function_node_declaration();
    assert_eq!(
        declaration.output_ports,
        vec![FunctionNodeOutputPort {
            name: "result",
            value_type: FunctionNodeValueType::Float,
            may_hand_out_nothing: true,
        }]
    );
    assert_eq!(
        evaluate_statelessly(&declaration, &[4.0], 2),
        (MODULE_HOST_TABLE_STATUS_OK, vec![0.25, 1.0])
    );
    assert_eq!(
        evaluate_statelessly(&declaration, &[0.0], 2),
        (MODULE_HOST_TABLE_STATUS_OK, vec![0.0, 0.0])
    );
}

#[test]
fn a_record_of_outputs_gives_each_field_that_may_be_none_a_flag_in_output_order() {
    let declaration = threshold_crossings_function_node_declaration();
    assert_eq!(
        declaration.output_ports,
        vec![
            FunctionNodeOutputPort {
                name: "level",
                value_type: FunctionNodeValueType::Float,
                may_hand_out_nothing: false,
            },
            FunctionNodeOutputPort {
                name: "rose_at_ns",
                value_type: FunctionNodeValueType::Int,
                may_hand_out_nothing: true,
            },
            FunctionNodeOutputPort {
                name: "fell",
                value_type: FunctionNodeValueType::Bool,
                may_hand_out_nothing: true,
            },
        ]
    );
    let mut output_values = [7.0; 5];
    // SAFETY: one value per input pin, one slot per output port and handed-out flag, none of config or state.
    let status = unsafe {
        (declaration.evaluate_entry)(
            [3.0, 1.0].as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            output_values.as_mut_ptr(),
            500,
        )
    };
    assert_eq!(status, MODULE_HOST_TABLE_STATUS_OK);
    assert_eq!(
        output_values,
        [3.0, 500.0, 0.0, 1.0, 0.0],
        "`rose_at_ns` handed one out and `fell` did not, their flags after every output"
    );
}

#[test]
#[should_panic(expected = "take exactly one `UnreadMessage` input")]
fn an_unread_message_output_with_two_unread_message_inputs_to_follow_is_refused() {
    refuse_an_unread_message_output_following_no_one_input(
        &[
            FunctionNodeValueType::UnreadMessage,
            FunctionNodeValueType::UnreadMessage,
        ],
        <UnreadMessage as FunctionNodeOutput>::OUTPUT_PORTS,
    );
}

#[test]
#[should_panic(expected = "take exactly one `UnreadMessage` input")]
fn an_unread_message_output_with_no_unread_message_input_to_follow_is_refused() {
    refuse_an_unread_message_output_following_no_one_input(
        &[FunctionNodeValueType::Float],
        <Option<UnreadMessage> as FunctionNodeOutput>::OUTPUT_PORTS,
    );
}

#[cfg(feature = "pyo3")]
#[test]
fn a_gates_declaration_reaches_python_with_its_value_types_and_what_may_be_none() {
    use pyo3::prelude::*;

    Python::attach(|python| {
        let python_declaration = gate_function_node_declaration()
            .to_python_declaration(python)
            .expect("the declaration converts");
        let entry = |key: &str, index: usize, entry_key: &str| {
            python_declaration
                .get_item(key)
                .expect("a dict lookup")
                .unwrap_or_else(|| panic!("the declaration carries `{key}`"))
                .get_item(index)
                .unwrap()
                .get_item(entry_key)
                .unwrap()
        };
        assert_eq!(
            entry("input_pins", 0, "value_type")
                .extract::<String>()
                .unwrap(),
            "unread_message"
        );
        assert!(
            !entry("input_pins", 0, "takes_none")
                .extract::<bool>()
                .unwrap()
        );
        assert_eq!(
            entry("output_ports", 0, "value_type")
                .extract::<String>()
                .unwrap(),
            "unread_message"
        );
        assert!(
            entry("output_ports", 0, "may_hand_out_nothing")
                .extract::<bool>()
                .unwrap()
        );
    });
}

#[test]
fn a_doc_comment_is_never_a_function_nodes_description() {
    assert_eq!(gate_function_node_declaration().description, "");
}

/// A doc comment beside the declared description.
#[function_node]
#[meta(description = "One over `x`, *or nothing* at zero.")]
fn described_reciprocal(x: f64) -> Option<f64> {
    (x != 0.0).then(|| 1.0 / x)
}

#[test]
fn a_literal_description_is_declared_exactly_as_written() {
    assert_eq!(
        described_reciprocal_function_node_declaration().description,
        "One over `x`, *or nothing* at zero."
    );
}

#[test]
fn a_function_node_with_no_category_declares_none() {
    assert_eq!(
        described_reciprocal_function_node_declaration().category,
        None
    );
    assert_eq!(gate_function_node_declaration().category, None);
}

#[function_node]
#[meta(category = "Math/Rounding")]
#[meta(description = "`x` rounded down to a whole number.")]
fn floor_to_a_whole_number(x: f64) -> f64 {
    x.floor()
}

#[test]
fn a_category_is_declared_exactly_as_written_beside_a_description_in_its_own_meta() {
    let declaration = floor_to_a_whole_number_function_node_declaration();
    assert_eq!(declaration.category, Some("Math/Rounding"));
    assert_eq!(
        declaration.description,
        "`x` rounded down to a whole number."
    );
}

#[cfg(feature = "pyo3")]
#[test]
fn only_a_declared_category_reaches_python_and_an_undeclared_one_is_left_out() {
    use pyo3::prelude::*;

    Python::attach(|python| {
        let category_reaching_python = |declaration: FunctionNodeDeclaration| {
            declaration
                .to_python_declaration(python)
                .expect("the declaration converts")
                .get_item("category")
                .expect("a dict lookup")
                .map(|category| category.extract::<String>().unwrap())
        };
        assert_eq!(
            category_reaching_python(floor_to_a_whole_number_function_node_declaration()),
            Some("Math/Rounding".to_owned())
        );
        assert_eq!(
            category_reaching_python(described_reciprocal_function_node_declaration()),
            None
        );
    });
}
