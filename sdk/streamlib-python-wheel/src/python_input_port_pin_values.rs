// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pin values a helper's unlinked input ports read, and the fires its unlinked Trigger
//! inputs queue.

use std::collections::HashMap;
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::python_helper_stream_clock::helper_stream_clock_now_ns;
use crate::python_node_link_data_access::PythonNodeLinkDataAccess;

/// Read each input port of `pin_values` as its pin value while no link feeds
/// it, in place of the pin values read before; a port left out reads only its
/// links again.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, pin_values))]
pub(crate) fn read_each_unlinked_input_port_as_its_pin_value(
    link_data_access: &PythonNodeLinkDataAccess,
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

/// Queue one fire of each Trigger input of `fired`, in order and stamped with this helper's
/// stream time, for its node to read as a Trigger message; an input named twice fires twice.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, fired))]
pub(crate) fn queue_a_fire_of_each_trigger_input(
    link_data_access: &PythonNodeLinkDataAccess,
    fired: Vec<String>,
) -> PyResult<()> {
    let input_mailboxes = link_data_access
        .input_mailboxes_of_this_helper()
        .ok_or_else(|| {
            PyValueError::new_err("a fire reaches only a helper process's own data plane")
        })?;
    for input in fired {
        let port = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&input)
            .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
        input_mailboxes
            .queue_a_trigger_fire(&port, helper_stream_clock_now_ns())
            .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
    }
    Ok(())
}
