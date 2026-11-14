// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The pin values a helper's unlinked input ports read.

use std::collections::HashMap;
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::python_helper_stream_clock::helper_stream_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

/// Read each input port of `pin_values` as its pin value while no link feeds
/// it, in place of the pin values read before; a port left out reads only its
/// links again.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, pin_values))]
pub(crate) fn read_each_unlinked_input_port_as_its_pin_value(
    link_data_access: &PythonProcessorLinkDataAccess,
    pin_values: HashMap<String, f64>,
) -> PyResult<()> {
    let input_mailboxes = link_data_access
        .input_mailboxes_of_this_helper()
        .ok_or_else(|| {
            PyValueError::new_err("a pin value feeds only a helper process's own data plane")
        })?;
    input_mailboxes.read_each_unlinked_input_port_as_its_pin_value(
        pin_values,
        Arc::new(helper_stream_clock_now_ns),
    );
    Ok(())
}
