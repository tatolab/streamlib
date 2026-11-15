// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The verbs a native module calls on the ports lent to it: opening one, writing an output, and
//! reading, describing and releasing an input's messages.

use std::ffi::c_void;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::cast_exposed_name_to_url_safe;
use streamlib::sdk::iceoryx2::{
    DataChannelPayloadDescriptor, MessageGroupBoundary, MessageReadFromInputPort,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_EMPTY, MODULE_HOST_TABLE_STATUS_OK, MODULE_HOST_TABLE_STATUS_REFUSED,
    MODULE_HOST_TABLE_STATUS_TIMED_OUT, MODULE_HOST_TABLE_TIMESTAMP_NOW, ModuleHostTableBytes,
    ModuleHostTableErrorMessage, ModuleHostTablePayloadDescriptor, ModuleHostTablePortHandle,
    ModuleHostTableReadMessage, ModuleHostTableStatus, ModuleHostTableSurfaceHandle,
    ModuleHostTableUtf8,
};

use crate::python_bag_conversion::json_value_to_python_object;
use crate::python_helper_stream_clock::helper_stream_clock_now_ns;

use super::{ModuleHostOfOneNode, node_host_of, refuse};

/// The lent port `port_name` names, as a handle counted from one.
fn handle_of_a_lent_port(
    direction: &str,
    lent_port_names: &[String],
    port_name: &str,
) -> Result<ModuleHostTablePortHandle, String> {
    let port_name = cast_exposed_name_to_url_safe(port_name)
        .map_err(|names_no_port| names_no_port.to_string())?;
    lent_port_names
        .iter()
        .position(|lent| *lent == *port_name)
        .map(|index| ModuleHostTablePortHandle {
            value: index as u64 + 1,
        })
        .ok_or_else(|| {
            format!(
                "{direction} port {port_name:?} was not lent to native code; \
                 `ctx.open_module_host_table` lent {lent_port_names:?}"
            )
        })
}

fn lent_port_name_of_a_handle<'node>(
    direction: &str,
    lent_port_names: &'node [String],
    port: ModuleHostTablePortHandle,
) -> Result<&'node str, String> {
    usize::try_from(port.value)
        .ok()
        .and_then(|value| value.checked_sub(1))
        .and_then(|index| lent_port_names.get(index))
        .map(String::as_str)
        .ok_or_else(|| {
            format!(
                "{direction} port handle {} names no port this table opened",
                port.value
            )
        })
}

unsafe fn open_a_lent_port(
    direction: &str,
    lent_port_names: &[String],
    port_name: ModuleHostTableUtf8,
    opened_port: *mut ModuleHostTablePortHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK hands a view live for the call.
    let port_name = match unsafe { port_name.as_str() } {
        Ok(port_name) => port_name,
        Err(not_utf8) => {
            return unsafe { refuse(error_message, &format!("a port name is UTF-8: {not_utf8}")) };
        }
    };
    match handle_of_a_lent_port(direction, lent_port_names, port_name) {
        Ok(handle) => match unsafe { opened_port.as_mut() } {
            Some(opened_port) => {
                *opened_port = handle;
                MODULE_HOST_TABLE_STATUS_OK
            }
            None => unsafe { refuse(error_message, "no slot was handed for the opened port") },
        },
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn output_port_open(
    node_host: *mut c_void,
    port_name: ModuleHostTableUtf8,
    opened_port: *mut ModuleHostTablePortHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    unsafe {
        open_a_lent_port(
            "output",
            &node_host.lent_output_port_names,
            port_name,
            opened_port,
            error_message,
        )
    }
}

pub(super) unsafe extern "C" fn input_port_open(
    node_host: *mut c_void,
    port_name: ModuleHostTableUtf8,
    opened_port: *mut ModuleHostTablePortHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    unsafe {
        open_a_lent_port(
            "input",
            &node_host.lent_input_port_names,
            port_name,
            opened_port,
            error_message,
        )
    }
}

pub(super) unsafe extern "C" fn output_port_write(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    payload: ModuleHostTableBytes,
    timestamp_ns: i64,
    starts_group: u32,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    unsafe {
        write_on_a_lent_output_port(
            node_host,
            port,
            payload,
            timestamp_ns,
            starts_group,
            DataChannelPayloadDescriptor::bytes(),
            error_message,
        )
    }
}

pub(super) unsafe extern "C" fn output_port_write_described(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    payload: ModuleHostTableBytes,
    timestamp_ns: i64,
    starts_group: u32,
    payload_descriptor: *const ModuleHostTablePayloadDescriptor,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK hands a descriptor live for the call.
    let Some(payload_descriptor) = (unsafe { payload_descriptor.as_ref() }) else {
        return unsafe { refuse(error_message, "no payload descriptor was handed") };
    };
    unsafe {
        write_on_a_lent_output_port(
            node_host,
            port,
            payload,
            timestamp_ns,
            starts_group,
            DataChannelPayloadDescriptor {
                kind: payload_descriptor.kind,
                body: payload_descriptor.body,
            },
            error_message,
        )
    }
}

unsafe fn write_on_a_lent_output_port(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    payload: ModuleHostTableBytes,
    timestamp_ns: i64,
    starts_group: u32,
    payload_descriptor: DataChannelPayloadDescriptor,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let port_name =
        match lent_port_name_of_a_handle("output", &node_host.lent_output_port_names, port) {
            Ok(port_name) => port_name,
            Err(refusal) => return unsafe { refuse(error_message, &refusal) },
        };
    if !node_host.output_writer.has_port(port_name) {
        return MODULE_HOST_TABLE_STATUS_OK;
    }
    let timestamp_ns = if timestamp_ns == MODULE_HOST_TABLE_TIMESTAMP_NOW {
        helper_stream_clock_now_ns()
    } else {
        timestamp_ns
    };
    let group_boundary = if starts_group != 0 {
        MessageGroupBoundary::StartsGroup
    } else {
        MessageGroupBoundary::ContinuesGroup
    };
    // SAFETY: the SDK hands a payload live for the call.
    let payload_bytes = unsafe { payload.as_slice() };
    match node_host.output_writer.write_bytes_described_by(
        port_name,
        payload_bytes,
        timestamp_ns,
        group_boundary,
        payload_descriptor,
    ) {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(Error::PayloadExceedsChannelCeiling {
            channel,
            payload_bytes,
            largest_admitted_frame_bytes,
            tier,
            refused_bags_on_the_output_port: refused_bags,
            ..
        }) => {
            tracing::warn!(
                "output port {port_name:?} of node {:?} refused a {payload_bytes}-byte message \
                 its native code wrote, past the {largest_admitted_frame_bytes} bytes channel \
                 {channel:?} admits ({tier} tier); the message was dropped, and the port has \
                 refused {refused_bags} so far",
                node_host.node_id
            );
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(write_failure) => unsafe { refuse(error_message, &write_failure.to_string()) },
    }
}

pub(super) unsafe extern "C" fn output_port_declare_groups_given_up_before_the_next_write(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    given_up_group_count: u64,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let port_name =
        match lent_port_name_of_a_handle("output", &node_host.lent_output_port_names, port) {
            Ok(port_name) => port_name,
            Err(refusal) => return unsafe { refuse(error_message, &refusal) },
        };
    if !node_host.output_writer.has_port(port_name) {
        return MODULE_HOST_TABLE_STATUS_OK;
    }
    match node_host
        .output_writer
        .declare_groups_given_up_upstream_before_the_next_write(port_name, given_up_group_count)
    {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(declaration_failure) => unsafe {
            refuse(error_message, &declaration_failure.to_string())
        },
    }
}

pub(super) unsafe extern "C" fn output_port_set_stream_facts(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    family: ModuleHostTableUtf8,
    stream_facts_json: ModuleHostTableUtf8,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    // SAFETY: the SDK hands views live for the call.
    let (family, stream_facts_json) = match unsafe { (family.as_str(), stream_facts_json.as_str()) }
    {
        (Ok(family), Ok(stream_facts_json)) => (family, stream_facts_json),
        _ => {
            return unsafe { refuse(error_message, "stream facts' family and fields are UTF-8") };
        }
    };
    match set_stream_facts_through_the_parent(node_host, port, family, stream_facts_json) {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

fn set_stream_facts_through_the_parent(
    node_host: &ModuleHostOfOneNode,
    port: ModuleHostTablePortHandle,
    family: &str,
    stream_facts_json: &str,
) -> Result<(), String> {
    let port_name = lent_port_name_of_a_handle("output", &node_host.lent_output_port_names, port)?;
    let serde_json::Value::Object(mut stream_facts_wire) = serde_json::from_str(stream_facts_json)
        .map_err(|not_json| {
            format!(
                "output port {port_name:?}: stream facts' fields are one JSON object: {not_json}"
            )
        })?
    else {
        return Err(format!(
            "output port {port_name:?}: stream facts' fields are one JSON object, got \
             {stream_facts_json}"
        ));
    };
    stream_facts_wire.insert("family".to_string(), serde_json::Value::from(family));
    let stream_facts_wire = serde_json::Value::Object(stream_facts_wire);
    let escalate_request_to_parent =
        node_host
            .escalate_request_to_parent
            .as_ref()
            .ok_or_else(|| {
                "stream facts live on the parent's graph, and this node has no route to the parent"
                    .to_string()
            })?;
    Python::attach(|python| -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "set_output_port_stream_facts")?;
        op.set_item("port", port_name)?;
        op.set_item(
            "stream_facts",
            json_value_to_python_object(python, &stream_facts_wire)?,
        )?;
        escalate_request_to_parent.bind(python).call1((op,))?;
        Ok(())
    })
    .map_err(|refusal| refusal.to_string())
}

pub(super) unsafe extern "C" fn input_port_read(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    read_message: *mut ModuleHostTableReadMessage,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let port_name =
        match lent_port_name_of_a_handle("input", &node_host.lent_input_port_names, port) {
            Ok(port_name) => port_name,
            Err(refusal) => return unsafe { refuse(error_message, &refusal) },
        };
    let Some(read_message) = (unsafe { read_message.as_mut() }) else {
        return unsafe { refuse(error_message, "no slot was handed for the read message") };
    };
    if !node_host.input_mailboxes.has_port(port_name) {
        return MODULE_HOST_TABLE_STATUS_EMPTY;
    }
    match node_host.input_mailboxes.read_raw(port_name) {
        Ok(Some(message)) => {
            let message = Box::new(message);
            *read_message = ModuleHostTableReadMessage {
                payload: message.payload.as_ptr(),
                payload_length: message.payload.len(),
                timestamp_ns: message.timestamp_ns,
                starts_group: u32::from(matches!(
                    message.group_boundary,
                    MessageGroupBoundary::StartsGroup
                )),
                reserved: 0,
                lost_since_last_read: message.lost_since_last_read,
                host_owned_message: Box::into_raw(message).cast::<c_void>(),
            };
            MODULE_HOST_TABLE_STATUS_OK
        }
        Ok(None) => MODULE_HOST_TABLE_STATUS_EMPTY,
        Err(read_failure) => unsafe { refuse(error_message, &read_failure.to_string()) },
    }
}

/// The message a read handed out, as the host boxed it.
unsafe fn message_a_read_handed_out<'message>(
    read_message: *const ModuleHostTableReadMessage,
) -> Option<&'message MessageReadFromInputPort> {
    // SAFETY: a read boxed the message and the SDK holds it until release.
    let read_message = unsafe { read_message.as_ref() }?;
    unsafe {
        read_message
            .host_owned_message
            .cast::<MessageReadFromInputPort>()
            .as_ref()
    }
}

pub(super) unsafe extern "C" fn input_message_payload_descriptor(
    _node_host: *mut c_void,
    read_message: *const ModuleHostTableReadMessage,
    payload_descriptor: *mut ModuleHostTablePayloadDescriptor,
) -> ModuleHostTableStatus {
    let (Some(message), Some(payload_descriptor)) =
        (unsafe { message_a_read_handed_out(read_message) }, unsafe {
            payload_descriptor.as_mut()
        })
    else {
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    };
    *payload_descriptor = ModuleHostTablePayloadDescriptor {
        kind: message.payload_descriptor.kind,
        body: message.payload_descriptor.body,
    };
    MODULE_HOST_TABLE_STATUS_OK
}

pub(super) unsafe extern "C" fn input_message_lend_pooled_gpu_frame(
    node_host: *mut c_void,
    read_message: *const ModuleHostTableReadMessage,
    lent_surface: *mut ModuleHostTableSurfaceHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let Some(message) = (unsafe { message_a_read_handed_out(read_message) }) else {
        return unsafe { refuse(error_message, "no read message was handed") };
    };
    let Some(lent_surface) = (unsafe { lent_surface.as_mut() }) else {
        return unsafe { refuse(error_message, "no slot was handed for the lent surface") };
    };
    match node_host
        .surfaces_lent_to_native_code
        .lend_the_pooled_gpu_frame_a_message_carries(&message.payload_descriptor)
    {
        Ok(handle) => {
            *lent_surface = handle;
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn input_message_release(
    _node_host: *mut c_void,
    read_message: *mut ModuleHostTableReadMessage,
) {
    let Some(read_message) = (unsafe { read_message.as_mut() }) else {
        return;
    };
    let host_owned_message =
        std::mem::replace(&mut read_message.host_owned_message, std::ptr::null_mut());
    if !host_owned_message.is_null() {
        // SAFETY: the read boxed this message and handed it out once.
        drop(unsafe { Box::from_raw(host_owned_message.cast::<MessageReadFromInputPort>()) });
    }
    read_message.payload = std::ptr::null();
    read_message.payload_length = 0;
}

pub(super) unsafe extern "C" fn input_wait_for_any_port(
    node_host: *mut c_void,
    timeout_ns: u64,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    if node_host.input_mailboxes.any_port_has_data() {
        return MODULE_HOST_TABLE_STATUS_OK;
    }
    let timeout = Duration::from_nanos(timeout_ns);
    let timeout_ms =
        i32::try_from(timeout.as_millis().max(u128::from(timeout_ns > 0))).unwrap_or(i32::MAX);
    let listener_fd = node_host.input_mailboxes.listener_fd();
    let mut poll_descriptors: Vec<libc::pollfd> = listener_fd
        .map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .into_iter()
        .collect();
    // SAFETY: the slice is live for the call, and its length is its count.
    let polled = unsafe {
        libc::poll(
            poll_descriptors.as_mut_ptr(),
            poll_descriptors.len() as libc::nfds_t,
            timeout_ms,
        )
    };
    if polled < 0 {
        let poll_failure = std::io::Error::last_os_error();
        if poll_failure.kind() != std::io::ErrorKind::Interrupted {
            return unsafe {
                refuse(
                    error_message,
                    &format!("waiting on the input listener failed: {poll_failure}"),
                )
            };
        }
    }
    if listener_fd.is_some() {
        node_host.input_mailboxes.drain_listener();
    }
    if node_host.input_mailboxes.any_port_has_data() {
        MODULE_HOST_TABLE_STATUS_OK
    } else {
        MODULE_HOST_TABLE_STATUS_TIMED_OUT
    }
}
