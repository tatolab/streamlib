// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The module host table this wheel lends a node's native code: the one
//! implementation of `tatolab-module-sdk`'s table, over the same data plane
//! the node's Python reaches its ports through.
//!
//! Every verb but the catalog entry's runs with no Python attached, from any
//! thread. The engine never calls into a module; a module calls in here, and
//! the audio capture and playback streams it opens call its callbacks from
//! their device threads. A virtual camera it opens is a PipeWire node in this
//! helper.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod host_memory_pixel_rows;
mod module_host_surface_verbs;
mod shared_native_service_leases;

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::{PyCapsule, PyDict};
use streamlib::sdk::context::CaptureDeviceAccessPrompting;
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::cast_exposed_name_to_url_safe;
use streamlib::sdk::iceoryx2::{
    InputMailboxesInner, MessageGroupBoundary, MessageReadFromInputPort, OutputWriterInner,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR, MODULE_HOST_TABLE_LOG_LEVEL_DEBUG,
    MODULE_HOST_TABLE_LOG_LEVEL_ERROR, MODULE_HOST_TABLE_LOG_LEVEL_INFO,
    MODULE_HOST_TABLE_LOG_LEVEL_WARN, MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME,
    MODULE_HOST_TABLE_STATUS_EMPTY, MODULE_HOST_TABLE_STATUS_ENDED, MODULE_HOST_TABLE_STATUS_OK,
    MODULE_HOST_TABLE_STATUS_REFUSED, MODULE_HOST_TABLE_STATUS_TIMED_OUT,
    MODULE_HOST_TABLE_TIMESTAMP_NOW, ModuleHostTable, ModuleHostTableBytes,
    ModuleHostTableColorInfo, ModuleHostTableDmaBufImport, ModuleHostTableErrorMessage,
    ModuleHostTableHostMemoryPixels, ModuleHostTableHostMemoryPixelsToFill,
    ModuleHostTableIOSurfaceImport, ModuleHostTableNodeOutputTextureRequest,
    ModuleHostTablePortHandle, ModuleHostTableReadMessage, ModuleHostTableSharedNativeServiceLease,
    ModuleHostTableSharedNativeServiceLeaseHandle, ModuleHostTableStatus,
    ModuleHostTableSurfaceHandle, ModuleHostTableUtf8,
};

use self::module_host_surface_verbs::SurfacesLentToNativeCode;
use self::shared_native_service_leases::{
    SharedNativeServiceLeasesHeldByNativeCode, SharedNativeServiceStateTheParentAnswered,
};
use crate::helper_process_native_escalate_channel::HelperProcessNativeEscalateChannel;
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;

use crate::python_bag_conversion::json_value_to_python_object;
use crate::python_helper_stream_clock::helper_stream_clock_now_ns;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_link_data_access::HelperProcessDataPlaneLentToNativeCode;

use audio_capture_streams::{
    AudioCaptureStreamsOpenedForOneNode, audio_capture_stream_close, audio_capture_stream_liveness,
    audio_capture_stream_open, audio_capture_stream_start, audio_capture_stream_stop,
};

use audio_playback_streams::{
    AudioPlaybackStreamsOpenedForOneNode, audio_playback_stream_close,
    audio_playback_stream_liveness, audio_playback_stream_open, audio_playback_stream_start,
    audio_playback_stream_stop,
};

use virtual_camera_streams::{
    VirtualCameraStreamsOpenedForOneNode, virtual_camera_stream_close,
    virtual_camera_stream_liveness, virtual_camera_stream_offer_extent, virtual_camera_stream_open,
    virtual_camera_stream_write_yuyv_frame,
};

mod audio_capture_streams;
mod audio_playback_streams;
mod virtual_camera_streams;

/// The `tracing` target a module's records are emitted under in this helper;
/// the module's own target rides as the `module_target` field.
const MODULE_LOG_RECORD_TRACING_TARGET: &str = "tatolab_module";

/// One node's lent ports and what else its native code reaches, behind the
/// table it is reached through.
///
/// Reference-counted through the table's retain and release; the capsule
/// Python holds owns one count.
struct ModuleHostOfOneNode {
    module_host_table: ModuleHostTable,
    node_id: String,
    input_mailboxes: Arc<InputMailboxesInner>,
    output_writer: Arc<OutputWriterInner>,
    lent_input_port_names: Vec<String>,
    lent_output_port_names: Vec<String>,
    escalate_request_to_parent: Option<Py<PyAny>>,
    stop_requested_by_parent: Arc<AtomicBool>,
    host_capabilities_json: String,
    audio_capture_streams: AudioCaptureStreamsOpenedForOneNode,
    surfaces_lent_to_native_code: SurfacesLentToNativeCode,
    audio_playback_streams: AudioPlaybackStreamsOpenedForOneNode,
    shared_native_service_leases: SharedNativeServiceLeasesHeldByNativeCode,
    virtual_camera_streams: VirtualCameraStreamsOpenedForOneNode,
}

/// The capsule `ctx.open_module_host_table(...)` returns: a table over the
/// ports `data_plane` lends, for the node `node_id` names.
pub(crate) fn module_host_table_capsule_lending<'py>(
    python: Python<'py>,
    data_plane: HelperProcessDataPlaneLentToNativeCode,
    node_id: &str,
    runtime_id: &str,
    escalate_request_to_parent: Option<Py<PyAny>>,
    stop_requested_by_parent: Arc<AtomicBool>,
    surface_share_client: Option<Arc<HelperProcessGpuExchangeClient>>,
) -> PyResult<Bound<'py, PyCapsule>> {
    let module_host = module_host_of_one_node(
        data_plane,
        node_id,
        runtime_id,
        escalate_request_to_parent,
        stop_requested_by_parent,
        SurfacesLentToNativeCode::new(
            HelperProcessNativeEscalateChannel::of_this_helper_process(),
            surface_share_client,
        ),
        SharedNativeServiceLeasesHeldByNativeCode::new(
            HelperProcessNativeEscalateChannel::of_this_helper_process(),
        ),
    );
    let module_host_table =
        std::ptr::NonNull::from(&module_host.module_host_table).cast::<c_void>();
    // The capsule's count; its destructor releases it.
    let _ = Arc::into_raw(module_host);
    // SAFETY: the pointer is to the table inside an allocation the count just
    // leaked keeps alive until the destructor releases it.
    unsafe {
        PyCapsule::new_with_pointer_and_destructor(
            python,
            module_host_table,
            MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME,
            Some(release_the_count_a_module_host_table_capsule_holds),
        )
    }
}

fn module_host_of_one_node(
    data_plane: HelperProcessDataPlaneLentToNativeCode,
    node_id: &str,
    runtime_id: &str,
    escalate_request_to_parent: Option<Py<PyAny>>,
    stop_requested_by_parent: Arc<AtomicBool>,
    surfaces_lent_to_native_code: SurfacesLentToNativeCode,
    shared_native_service_leases: SharedNativeServiceLeasesHeldByNativeCode,
) -> Arc<ModuleHostOfOneNode> {
    let capture_device_access_prompting = CaptureDeviceAccessPrompting::of_this_helper_process();
    let host_capabilities_json = serde_json::json!({
        "module_host_table": {
            "major": MODULE_HOST_TABLE_ABI_MAJOR,
            "minor": MODULE_HOST_TABLE_ABI_MINOR,
        },
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "node_id": node_id,
        "runtime_id": runtime_id,
        "lent_input_ports": data_plane.lent_input_port_names,
        "lent_output_ports": data_plane.lent_output_port_names,
        "capture_device_access_prompting": capture_device_access_prompting.as_environment_value(),
    })
    .to_string();
    Arc::new_cyclic(
        |this_node_host: &std::sync::Weak<ModuleHostOfOneNode>| ModuleHostOfOneNode {
            module_host_table: ModuleHostTable {
                abi_major: MODULE_HOST_TABLE_ABI_MAJOR,
                abi_minor: MODULE_HOST_TABLE_ABI_MINOR,
                table_size_bytes: std::mem::size_of::<ModuleHostTable>() as u64,
                node_host: this_node_host.as_ptr().cast_mut().cast::<c_void>(),
                node_host_retain,
                node_host_release,
                output_port_open,
                output_port_write,
                output_port_set_catalog_entry,
                input_port_open,
                input_port_read,
                input_message_release,
                input_wait_for_any_port,
                stop_requested,
                monotonic_now_ns,
                log,
                host_capabilities_json: host_capabilities_json_of,
                audio_capture_stream_open,
                audio_capture_stream_start,
                audio_capture_stream_stop,
                audio_capture_stream_liveness,
                audio_capture_stream_close,
                surface_import_dma_buf,
                output_texture_acquire_from_node_output_pool,
                surface_convert_into,
                surface_id,
                surface_release,
                audio_playback_stream_open,
                audio_playback_stream_start,
                audio_playback_stream_stop,
                audio_playback_stream_liveness,
                audio_playback_stream_close,
                surface_import_iosurface,
                surface_write_from_host_memory,
                surface_lend_by_surface_id,
                shared_native_service_lease_acquire,
                shared_native_service_lease_state,
                shared_native_service_lease_release,
                node_fail_unrecoverably,
                virtual_camera_stream_open,
                virtual_camera_stream_offer_extent,
                virtual_camera_stream_write_yuyv_frame,
                virtual_camera_stream_liveness,
                virtual_camera_stream_close,
                surface_read_into_host_memory,
            },
            node_id: node_id.to_string(),
            input_mailboxes: data_plane.input_mailboxes,
            output_writer: data_plane.output_writer,
            lent_input_port_names: data_plane.lent_input_port_names,
            lent_output_port_names: data_plane.lent_output_port_names,
            escalate_request_to_parent,
            stop_requested_by_parent,
            host_capabilities_json,
            audio_capture_streams: AudioCaptureStreamsOpenedForOneNode::under(
                capture_device_access_prompting,
            ),
            surfaces_lent_to_native_code,
            audio_playback_streams: AudioPlaybackStreamsOpenedForOneNode::default(),
            shared_native_service_leases,
            virtual_camera_streams: VirtualCameraStreamsOpenedForOneNode::default(),
        },
    )
}

unsafe extern "C" fn release_the_count_a_module_host_table_capsule_holds(
    capsule: *mut pyo3::ffi::PyObject,
) {
    // SAFETY: CPython calls this with the capsule being destroyed, whose name
    // is the one it was made with.
    let module_host_table = unsafe {
        pyo3::ffi::PyCapsule_GetPointer(capsule, MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME.as_ptr())
    };
    if module_host_table.is_null() {
        // SAFETY: an attached thread; the failed lookup set an exception that
        // a destructor has nowhere to raise.
        unsafe { pyo3::ffi::PyErr_Clear() };
        return;
    }
    // SAFETY: the table names the node host whose count this capsule holds.
    unsafe { node_host_release((*module_host_table.cast::<ModuleHostTable>()).node_host) };
}

/// The node host a verb was handed.
///
/// # Safety
///
/// `node_host` is the table's own, and some count keeps it alive for the call.
unsafe fn node_host_of<'call>(node_host: *mut c_void) -> &'call ModuleHostOfOneNode {
    // SAFETY: the caller's contract.
    unsafe { &*node_host.cast::<ModuleHostOfOneNode>() }
}

unsafe extern "C" fn node_host_retain(node_host: *mut c_void) {
    // SAFETY: a retain is called with a count already held.
    unsafe { Arc::increment_strong_count(node_host.cast::<ModuleHostOfOneNode>()) };
}

unsafe extern "C" fn node_host_release(node_host: *mut c_void) {
    // SAFETY: releases a count a retain, or the capsule, took.
    unsafe { Arc::decrement_strong_count(node_host.cast::<ModuleHostOfOneNode>()) };
}

/// Write `refusal` into the caller's buffer, when it handed one.
///
/// # Safety
///
/// `error_message` is null or points at a live buffer.
unsafe fn refuse(
    error_message: *mut ModuleHostTableErrorMessage,
    refusal: &str,
) -> ModuleHostTableStatus {
    if let Some(error_message) = unsafe { error_message.as_mut() } {
        error_message.write(refusal);
    }
    MODULE_HOST_TABLE_STATUS_REFUSED
}

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

unsafe extern "C" fn output_port_open(
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

unsafe extern "C" fn input_port_open(
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

unsafe extern "C" fn output_port_write(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    payload: ModuleHostTableBytes,
    timestamp_ns: i64,
    starts_group: u32,
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
    match node_host.output_writer.write_bytes_with_timestamp(
        port_name,
        payload_bytes,
        timestamp_ns,
        group_boundary,
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

unsafe extern "C" fn output_port_set_catalog_entry(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    section: ModuleHostTableUtf8,
    entry_json: ModuleHostTableUtf8,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    // SAFETY: the SDK hands views live for the call.
    let (section, entry_json) = match unsafe { (section.as_str(), entry_json.as_str()) } {
        (Ok(section), Ok(entry_json)) => (section, entry_json),
        _ => {
            return unsafe {
                refuse(
                    error_message,
                    "a catalog entry's section and fields are UTF-8",
                )
            };
        }
    };
    match set_catalog_entry_through_the_parent(node_host, port, section, entry_json) {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

fn set_catalog_entry_through_the_parent(
    node_host: &ModuleHostOfOneNode,
    port: ModuleHostTablePortHandle,
    section: &str,
    entry_json: &str,
) -> Result<(), String> {
    let port_name = lent_port_name_of_a_handle("output", &node_host.lent_output_port_names, port)?;
    let entry_fields: serde_json::Value = serde_json::from_str(entry_json).map_err(|not_json| {
        format!(
            "output port {port_name:?}: a catalog entry's fields are one JSON object: {not_json}"
        )
    })?;
    if !entry_fields.is_object() {
        return Err(format!(
            "output port {port_name:?}: a catalog entry's fields are one JSON object, got {entry_json}"
        ));
    }
    let escalate_request_to_parent = node_host.escalate_request_to_parent.as_ref().ok_or_else(
        || {
            "a catalog entry lives on the parent's graph, and this node has no route to the parent"
                .to_string()
        },
    )?;
    Python::attach(|python| -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "set_output_port_catalog_entry")?;
        op.set_item("port", port_name)?;
        op.set_item("section", section)?;
        op.set_item("entry", json_value_to_python_object(python, &entry_fields)?)?;
        escalate_request_to_parent.bind(python).call1((op,))?;
        Ok(())
    })
    .map_err(|refusal| refusal.to_string())
}

unsafe extern "C" fn input_port_read(
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

unsafe extern "C" fn input_message_release(
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

unsafe extern "C" fn input_wait_for_any_port(
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

unsafe extern "C" fn stop_requested(node_host: *mut c_void) -> u32 {
    let node_host = unsafe { node_host_of(node_host) };
    u32::from(node_host.stop_requested_by_parent.load(Ordering::Acquire))
}

unsafe extern "C" fn monotonic_now_ns(_node_host: *mut c_void) -> i64 {
    monotonic_clock_now_ns() as i64
}

unsafe extern "C" fn log(
    node_host: *mut c_void,
    level: u32,
    target: ModuleHostTableUtf8,
    message: ModuleHostTableUtf8,
) {
    let node_host = unsafe { node_host_of(node_host) };
    // SAFETY: the SDK hands views live for the call.
    let module_target = unsafe { target.as_str() }.unwrap_or("<target not UTF-8>");
    let message = unsafe { message.as_str() }.unwrap_or("<message not UTF-8>");
    let node_id = node_host.node_id.as_str();
    match level {
        MODULE_HOST_TABLE_LOG_LEVEL_ERROR => tracing::error!(
            target: MODULE_LOG_RECORD_TRACING_TARGET, module_target, node_id, "{message}"
        ),
        MODULE_HOST_TABLE_LOG_LEVEL_WARN => tracing::warn!(
            target: MODULE_LOG_RECORD_TRACING_TARGET, module_target, node_id, "{message}"
        ),
        MODULE_HOST_TABLE_LOG_LEVEL_INFO => tracing::info!(
            target: MODULE_LOG_RECORD_TRACING_TARGET, module_target, node_id, "{message}"
        ),
        MODULE_HOST_TABLE_LOG_LEVEL_DEBUG => tracing::debug!(
            target: MODULE_LOG_RECORD_TRACING_TARGET, module_target, node_id, "{message}"
        ),
        _ => tracing::trace!(
            target: MODULE_LOG_RECORD_TRACING_TARGET, module_target, node_id, "{message}"
        ),
    }
}

unsafe extern "C" fn host_capabilities_json_of(node_host: *mut c_void) -> ModuleHostTableUtf8 {
    let node_host = unsafe { node_host_of(node_host) };
    ModuleHostTableUtf8::of(&node_host.host_capabilities_json)
}

#[cfg(test)]
mod tests;

unsafe extern "C" fn surface_import_dma_buf(
    node_host: *mut c_void,
    dma_buf_import: *const ModuleHostTableDmaBufImport,
    imported_surface: *mut ModuleHostTableSurfaceHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK hands a retained node host and live pointers.
    let node_host = unsafe { node_host_of(node_host) };
    let Some(dma_buf_import) = (unsafe { dma_buf_import.as_ref() }) else {
        return unsafe { refuse(error_message, "no DMA-BUF import was handed") };
    };
    let Some(imported_surface) = (unsafe { imported_surface.as_mut() }) else {
        return unsafe { refuse(error_message, "no slot was handed for the imported surface") };
    };
    match node_host
        .surfaces_lent_to_native_code
        .import_dma_buf(dma_buf_import)
    {
        Ok(handle) => {
            *imported_surface = handle;
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn surface_import_iosurface(
    node_host: *mut c_void,
    iosurface_import: *const ModuleHostTableIOSurfaceImport,
    imported_surface: *mut ModuleHostTableSurfaceHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as in `surface_import_dma_buf`.
    let node_host = unsafe { node_host_of(node_host) };
    let Some(iosurface_import) = (unsafe { iosurface_import.as_ref() }) else {
        return unsafe { refuse(error_message, "no IOSurface import was handed") };
    };
    let Some(imported_surface) = (unsafe { imported_surface.as_mut() }) else {
        return unsafe { refuse(error_message, "no slot was handed for the imported surface") };
    };
    match node_host
        .surfaces_lent_to_native_code
        .import_iosurface(iosurface_import)
    {
        Ok(handle) => {
            *imported_surface = handle;
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn output_texture_acquire_from_node_output_pool(
    node_host: *mut c_void,
    request: *const ModuleHostTableNodeOutputTextureRequest,
    acquired_surface: *mut ModuleHostTableSurfaceHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as in `surface_import_dma_buf`.
    let node_host = unsafe { node_host_of(node_host) };
    let Some(request) = (unsafe { request.as_ref() }) else {
        return unsafe { refuse(error_message, "no texture request was handed") };
    };
    let Some(acquired_surface) = (unsafe { acquired_surface.as_mut() }) else {
        return unsafe { refuse(error_message, "no slot was handed for the acquired surface") };
    };
    match node_host
        .surfaces_lent_to_native_code
        .acquire_texture_from_node_output_pool(request)
    {
        Ok(handle) => {
            *acquired_surface = handle;
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn surface_lend_by_surface_id(
    node_host: *mut c_void,
    surface_id: ModuleHostTableUtf8,
    lent_surface: *mut ModuleHostTableSurfaceHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as in `surface_import_dma_buf`.
    let node_host = unsafe { node_host_of(node_host) };
    let Some(lent_surface) = (unsafe { lent_surface.as_mut() }) else {
        return unsafe { refuse(error_message, "no slot was handed for the lent surface") };
    };
    // SAFETY: the SDK hands a view live for the call.
    let surface_id = match unsafe { surface_id.as_str() } {
        Ok(surface_id) => surface_id,
        Err(not_utf8) => {
            return unsafe { refuse(error_message, &format!("a surface id is UTF-8: {not_utf8}")) };
        }
    };
    match node_host
        .surfaces_lent_to_native_code
        .lend_surface_a_message_names(surface_id)
    {
        Ok(handle) => {
            *lent_surface = handle;
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn surface_convert_into(
    node_host: *mut c_void,
    source: ModuleHostTableSurfaceHandle,
    destination: ModuleHostTableSurfaceHandle,
    ycbcr_side_color: *const ModuleHostTableColorInfo,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as in `surface_import_dma_buf`.
    let node_host = unsafe { node_host_of(node_host) };
    let Some(ycbcr_side_color) = (unsafe { ycbcr_side_color.as_ref() }) else {
        return unsafe { refuse(error_message, "no YCbCr colour description was handed") };
    };
    match node_host.surfaces_lent_to_native_code.convert_surface_into(
        source,
        destination,
        ycbcr_side_color,
    ) {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn surface_id(
    node_host: *mut c_void,
    surface: ModuleHostTableSurfaceHandle,
) -> ModuleHostTableUtf8 {
    // SAFETY: the SDK hands a retained node host.
    unsafe { node_host_of(node_host) }
        .surfaces_lent_to_native_code
        .surface_id_view(surface)
}

unsafe extern "C" fn surface_release(
    node_host: *mut c_void,
    surface: ModuleHostTableSurfaceHandle,
) {
    // SAFETY: the SDK hands a retained node host.
    unsafe { node_host_of(node_host) }
        .surfaces_lent_to_native_code
        .release(surface);
}

unsafe extern "C" fn surface_write_from_host_memory(
    node_host: *mut c_void,
    destination: ModuleHostTableSurfaceHandle,
    host_memory_pixels: *const ModuleHostTableHostMemoryPixels,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as in `surface_import_dma_buf`.
    let node_host = unsafe { node_host_of(node_host) };
    let Some(host_memory_pixels) = (unsafe { host_memory_pixels.as_ref() }) else {
        return unsafe { refuse(error_message, "no host-memory pixels were handed") };
    };
    match node_host
        .surfaces_lent_to_native_code
        .write_from_host_memory(destination, host_memory_pixels)
    {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn surface_read_into_host_memory(
    node_host: *mut c_void,
    source: ModuleHostTableSurfaceHandle,
    host_memory_pixels_to_fill: *const ModuleHostTableHostMemoryPixelsToFill,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as in `surface_import_dma_buf`.
    let node_host = unsafe { node_host_of(node_host) };
    let Some(host_memory_pixels_to_fill) = (unsafe { host_memory_pixels_to_fill.as_ref() }) else {
        return unsafe { refuse(error_message, "no host memory was handed to fill") };
    };
    match node_host
        .surfaces_lent_to_native_code
        .read_into_host_memory(source, host_memory_pixels_to_fill)
    {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn shared_native_service_lease_acquire(
    node_host: *mut c_void,
    service_name: ModuleHostTableUtf8,
    granted_lease: *mut ModuleHostTableSharedNativeServiceLease,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK hands a retained node host.
    let node_host = unsafe { node_host_of(node_host) };
    // SAFETY: the SDK hands a view live for the call.
    let service_name = match unsafe { service_name.as_str() } {
        Ok(service_name) => service_name,
        Err(_) => return unsafe { refuse(error_message, "the service name is not UTF-8") },
    };
    let Some(granted_lease) = (unsafe { granted_lease.as_mut() }) else {
        return unsafe { refuse(error_message, "no lease was handed to fill") };
    };
    match node_host.shared_native_service_leases.acquire(service_name) {
        Ok(lease) => {
            *granted_lease = lease;
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn shared_native_service_lease_state(
    node_host: *mut c_void,
    lease: ModuleHostTableSharedNativeServiceLeaseHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK hands a retained node host.
    let node_host = unsafe { node_host_of(node_host) };
    match node_host.shared_native_service_leases.state(lease) {
        Ok(SharedNativeServiceStateTheParentAnswered::Running) => MODULE_HOST_TABLE_STATUS_OK,
        Ok(SharedNativeServiceStateTheParentAnswered::StartingAgain) => {
            MODULE_HOST_TABLE_STATUS_EMPTY
        }
        Ok(SharedNativeServiceStateTheParentAnswered::Failed(why)) => {
            // SAFETY: `error_message` is null or the caller's live buffer.
            if let Some(error_message) = unsafe { error_message.as_mut() } {
                error_message.write(&why);
            }
            MODULE_HOST_TABLE_STATUS_ENDED
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn node_fail_unrecoverably(
    _node_host: *mut c_void,
    reason: ModuleHostTableUtf8,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK hands a view live for the call.
    let reason = match unsafe { reason.as_str() } {
        Ok(reason) => reason,
        Err(_) => return unsafe { refuse(error_message, "the failure's reason is not UTF-8") },
    };
    let Some(native_escalate_channel) =
        HelperProcessNativeEscalateChannel::of_this_helper_process()
    else {
        return unsafe {
            refuse(
                error_message,
                "this helper was started without a native escalate channel to its parent, so \
                 native code cannot fail its node",
            )
        };
    };
    match native_escalate_channel.request_from_parent(serde_json::json!({
        "op": "fail_the_node",
        "reason": reason,
    })) {
        Ok(_) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn shared_native_service_lease_release(
    node_host: *mut c_void,
    lease: ModuleHostTableSharedNativeServiceLeaseHandle,
) {
    // SAFETY: the SDK hands a retained node host.
    unsafe { node_host_of(node_host) }
        .shared_native_service_leases
        .release(lease);
}
