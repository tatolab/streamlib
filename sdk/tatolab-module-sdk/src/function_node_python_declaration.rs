// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A Rust function node's declaration as the dict `node.rust_function(...)` reads.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::function_node::{
    FunctionNodeDeclaration, FunctionNodeOutputPort, FunctionNodeRecordFieldWithItsDefault,
};
use crate::module_host_table_abi::{MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR};

impl FunctionNodeDeclaration {
    /// This declaration as the dict a module's Python half hands `node.rust_function(...)`.
    pub fn to_python_declaration<'py>(&self, python: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let declaration = PyDict::new(python);
        declaration.set_item("short_name", self.short_name)?;
        declaration.set_item("description", self.description)?;
        declaration.set_item("category", self.category)?;
        let input_pins = PyList::empty(python);
        for input_pin in &self.input_pins {
            let python_input_pin = PyDict::new(python);
            python_input_pin.set_item("port", input_pin.port)?;
            python_input_pin.set_item("value_type", input_pin.value_type.wire_name())?;
            python_input_pin.set_item("pin_default", input_pin.pin_default)?;
            python_input_pin.set_item("takes_none", input_pin.takes_none)?;
            input_pins.append(python_input_pin)?;
        }
        declaration.set_item("input_pins", input_pins)?;
        declaration.set_item(
            "output_ports",
            python_output_ports(python, &self.output_ports)?,
        )?;
        declaration.set_item(
            "config_fields",
            python_record_fields(python, &self.config_fields)?,
        )?;
        declaration.set_item(
            "state_fields",
            python_record_fields(python, &self.state_fields)?,
        )?;
        declaration.set_item("stream_time_parameter", self.stream_time_parameter)?;
        declaration.set_item("state_parameter", self.state_parameter)?;
        declaration.set_item("config_parameter", self.config_parameter)?;
        declaration.set_item("evaluate_entry_address", self.evaluate_entry as usize)?;
        declaration.set_item("module_host_table_abi_major", MODULE_HOST_TABLE_ABI_MAJOR)?;
        declaration.set_item("module_host_table_abi_minor", MODULE_HOST_TABLE_ABI_MINOR)?;
        Ok(declaration)
    }
}

fn python_output_ports<'py>(
    python: Python<'py>,
    output_ports: &[FunctionNodeOutputPort],
) -> PyResult<Bound<'py, PyList>> {
    let python_output_ports = PyList::empty(python);
    for output_port in output_ports {
        let python_output_port = PyDict::new(python);
        python_output_port.set_item("name", output_port.name)?;
        python_output_port.set_item("value_type", output_port.value_type.wire_name())?;
        python_output_port.set_item("may_hand_out_nothing", output_port.may_hand_out_nothing)?;
        python_output_ports.append(python_output_port)?;
    }
    Ok(python_output_ports)
}

fn python_record_fields<'py>(
    python: Python<'py>,
    record_fields: &[FunctionNodeRecordFieldWithItsDefault],
) -> PyResult<Bound<'py, PyList>> {
    let python_record_fields = PyList::empty(python);
    for record_field in record_fields {
        let python_record_field = PyDict::new(python);
        python_record_field.set_item("name", record_field.field.name)?;
        python_record_field.set_item("value_type", record_field.field.value_type.wire_name())?;
        python_record_field.set_item("default", record_field.default_value)?;
        python_record_fields.append(python_record_field)?;
    }
    Ok(python_record_fields)
}
