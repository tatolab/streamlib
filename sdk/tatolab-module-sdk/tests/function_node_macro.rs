// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[function_node]` declares the node `@node.function` would from the same
//! signature, and its evaluate entry computes what the function does.

#![cfg(feature = "function-node")]

use tatolab_module_sdk::function_node::{
    FunctionNodeInputPin, FunctionNodeNamedValue, FunctionNodeRecord,
    FunctionNodeRecordFieldWithItsDefault, FunctionNodeValueType, State, Stateful, Time,
    function_node,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_OK, MODULE_HOST_TABLE_STATUS_REFUSED,
};

/// `x` scaled by `gain`, then moved by `offset`.
#[function_node]
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
fn panics_on_a_negative_input(x: f64) -> bool {
    assert!(x >= 0.0, "a negative input");
    true
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
            },
            FunctionNodeInputPin {
                port: "gain",
                value_type: FunctionNodeValueType::Float,
                pin_default: Some(2.0),
            },
            FunctionNodeInputPin {
                port: "offset",
                value_type: FunctionNodeValueType::Float,
                pin_default: Some(0.5),
            },
        ]
    );
    assert_eq!(
        declaration.output_ports,
        vec![FunctionNodeNamedValue {
            name: "result",
            value_type: FunctionNodeValueType::Float,
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
            FunctionNodeNamedValue {
                name: "total",
                value_type: FunctionNodeValueType::Float,
            },
            FunctionNodeNamedValue {
                name: "changed_at_ns",
                value_type: FunctionNodeValueType::Int,
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
