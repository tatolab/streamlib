// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The reads and writes a function node run as a standalone node makes at its own ports: each
//! input's message unread, control value, parts or stored value, and each output's stored value
//! or message handed on.

use super::*;

/// The message `input_port` holds for this read, handed back unread: an
/// island's output at the port, or the newest message its link carried;
/// `None` when it holds none.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, input_port))]
pub(crate) fn read_the_message_at_an_input_port_unread(
    python: Python<'_>,
    link_data_access: &PythonNodeLinkDataAccess,
    input_port: &str,
) -> PyResult<Option<UnreadMessageAFunctionNodeHandsOn>> {
    Ok(link_data_access
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
        .map(|message| UnreadMessageAFunctionNodeHandsOn {
            message: Arc::new(
                MessageAFunctionNodeIslandHolds::hold_a_message_read_off_its_channel(
                    &message,
                    &link_data_access.claim_of_the_pooled_gpu_frames_its_islands_hold(),
                    input_port,
                ),
            ),
        }))
}

/// The number `input_port` holds for this read, read as an island reads one:
/// the first sample of a Channels block, f32 or f64, and any other message
/// refused by name; `None` when no message arrived since the last read.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, node_id, input_port))]
pub(crate) fn read_the_control_value_at_an_input_port(
    python: Python<'_>,
    link_data_access: &PythonNodeLinkDataAccess,
    node_id: &str,
    input_port: &str,
) -> PyResult<Option<f64>> {
    link_data_access
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
        .map(|message| the_control_value_a_message_carries(&message, node_id, input_port))
        .transpose()
        .map_err(|refusal| PyValueError::new_err(refusal.to_string()))
}

/// The parts of the value `input_port` holds for this read, a value of the pin
/// type the bootstrap names `value_type`, read as an island reads one off a
/// control message with the free conversion a link allows, and any other
/// message refused by name; `None` when no message arrived since the last read.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, node_id, input_port, value_type))]
pub(crate) fn read_the_parts_at_an_input_port<'py>(
    python: Python<'py>,
    link_data_access: &PythonNodeLinkDataAccess,
    node_id: &str,
    input_port: &str,
    value_type: &str,
) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let pin_type = the_pin_type_a_value_type_carries_in_parts(value_type).ok_or_else(|| {
        PyValueError::new_err(format!(
            "value type {value_type:?} is no Color, Vector, Rectangle or Transform"
        ))
    })?;
    link_data_access
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
        .map(|message| {
            the_parts_a_message_carries(&message, Some(&pin_type), node_id, input_port)
                .map_err(|refusal| PyValueError::new_err(refusal.to_string()))
                .and_then(|parts| PyTuple::new(python, parts))
        })
        .transpose()
}

/// The record `{"value": <stored JSON form>}` of the String, Menu, Asset or
/// List `input_port` holds for this read, a value of `pin_type`, given as JSON,
/// read off the message that type packs as and checked as a set value is, any
/// other message refused by name; `None` when no message arrived since the
/// last read.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, node_id, input_port, pin_type))]
pub(crate) fn read_the_stored_value_at_an_input_port<'py>(
    python: Python<'py>,
    link_data_access: &PythonNodeLinkDataAccess,
    node_id: &str,
    input_port: &str,
    pin_type: &str,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let pin_type = the_pin_type_given_as_json(pin_type)?;
    link_data_access
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
        .map(|message| {
            the_record_of_a_stored_value_a_message_carries(&message, &pin_type, node_id, input_port)
                .map_err(|refusal| PyValueError::new_err(refusal.to_string()))
                .and_then(|record| records_message_dict_of(python, input_port, &record))
        })
        .transpose()
}

/// Write `record`, `{"value": <stored JSON form>}` of a String, a Menu, an
/// Asset or a List of `pin_type`, given as JSON, on `output_port` as that type
/// packs: a Records message, or an Array frame for a List of numbers or values
/// of parts.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, output_port, pin_type, record))]
pub(crate) fn write_the_stored_value_on_an_output_port(
    python: Python<'_>,
    link_data_access: &PythonNodeLinkDataAccess,
    output_port: &str,
    pin_type: &str,
    record: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let pin_type = the_pin_type_given_as_json(pin_type)?;
    let PackedPinValue {
        payload_descriptor,
        payload,
    } = the_stored_value_a_record_carries_packed(
        record,
        &pin_type,
        &format!("output {output_port:?}"),
    )
    .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
    link_data_access.write_described_by(
        python,
        output_port,
        PyBytes::new(python, &payload).as_any(),
        None,
        true,
        |_| Ok(payload_descriptor),
    )
}

pub(super) fn the_pin_type_given_as_json(pin_type: &str) -> PyResult<PinType> {
    serde_json::from_str(pin_type).map_err(|refusal| {
        PyValueError::new_err(format!("the pin type {pin_type} does not read: {refusal}"))
    })
}

/// Write `message` on `output_port` exactly as it arrived, its descriptor,
/// stamp and group boundary unchanged.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, output_port, message))]
pub(crate) fn hand_an_unread_message_on(
    python: Python<'_>,
    link_data_access: &PythonNodeLinkDataAccess,
    output_port: &str,
    message: &UnreadMessageAFunctionNodeHandsOn,
) -> PyResult<()> {
    link_data_access.hand_a_message_on_unread(python, output_port, &message.message.message)
}
