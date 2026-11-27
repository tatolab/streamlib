// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The module host table this wheel lends a node's native code: the one
//! implementation of `tatolab-module-sdk`'s table, over the same data plane
//! the node's Python reaches its ports through.
//!
//! Every verb but the stream facts' runs with no Python attached, from any
//! thread. The engine never calls into a module; a module calls in here, and
//! the audio capture and playback streams it opens call its callbacks from
//! their device threads. A virtual camera it opens is a PipeWire node in this
//! helper.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod host_memory_pixel_rows;
mod module_host_lent_port_verbs;
mod module_host_surface_verbs;
mod shared_native_service_leases;

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;
use pyo3::types::PyCapsule;
use streamlib::sdk::context::CaptureDeviceAccessPrompting;
use streamlib::sdk::iceoryx2::{InputMailboxesInner, OutputWriterInner};
use streamlib_scope::CallTooSlowForAFrame;
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR,
    MODULE_HOST_TABLE_CALL_CREATES_A_KERNEL, MODULE_HOST_TABLE_CALL_OPENS_A_DEVICE,
    MODULE_HOST_TABLE_CALL_STARTS_A_SESSION, MODULE_HOST_TABLE_LOG_LEVEL_DEBUG,
    MODULE_HOST_TABLE_LOG_LEVEL_ERROR, MODULE_HOST_TABLE_LOG_LEVEL_INFO,
    MODULE_HOST_TABLE_LOG_LEVEL_WARN, MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME,
    MODULE_HOST_TABLE_STATUS_EMPTY, MODULE_HOST_TABLE_STATUS_ENDED, MODULE_HOST_TABLE_STATUS_OK,
    MODULE_HOST_TABLE_STATUS_REFUSED, ModuleHostTable, ModuleHostTableColorInfo,
    ModuleHostTableDmaBufImport, ModuleHostTableErrorMessage, ModuleHostTableHostMemoryPixels,
    ModuleHostTableHostMemoryPixelsToFill, ModuleHostTableIOSurfaceImport,
    ModuleHostTableNodeOutputTextureRequest, ModuleHostTablePayloadDescriptor,
    ModuleHostTableSharedNativeServiceLease, ModuleHostTableSharedNativeServiceLeaseHandle,
    ModuleHostTableStatus, ModuleHostTableSurfaceHandle, ModuleHostTableUtf8,
};

use self::module_host_lent_port_verbs::{
    input_message_lend_pooled_gpu_frame, input_message_payload_descriptor, input_message_release,
    input_port_open, input_port_read, input_wait_for_any_port,
    output_port_declare_groups_given_up_before_the_next_write, output_port_open,
    output_port_set_stream_facts, output_port_write, output_port_write_described,
};
use self::module_host_surface_verbs::SurfacesLentToNativeCode;
use self::shared_native_service_leases::{
    SharedNativeServiceLeasesHeldByNativeCode, SharedNativeServiceStateTheParentAnswered,
};
use crate::helper_process_native_escalate_channel::HelperProcessNativeEscalateChannel;
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;

use crate::python_helper_stream_clock::helper_stream_time_ns_of_a_machine_instant;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_node_link_data_access::HelperProcessDataPlaneLentToNativeCode;
use crate::python_node_scope::worded_while_a_frame_runs;

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
mod audio_stream_on_a_device;
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
                output_port_set_stream_facts,
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
                stream_time_ns_of_a_machine_instant,
                input_message_payload_descriptor,
                input_message_lend_pooled_gpu_frame,
                output_port_write_described,
                surface_gpu_pool_slot_frame,
                output_port_declare_groups_given_up_before_the_next_write,
                call_refused_while_a_frame_runs,
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

/// The refusal of `call` a verb answers while a frame of the node's runs on this thread, written
/// to `error_message`; `None` in the node's body, in a build, and anywhere else.
unsafe fn refused_while_a_frame_runs(
    error_message: *mut ModuleHostTableErrorMessage,
    does: CallTooSlowForAFrame,
    call: &str,
) -> Option<ModuleHostTableStatus> {
    worded_while_a_frame_runs(does, call)
        .err()
        .map(|refusal| unsafe { refuse(error_message, &refusal) })
}

unsafe extern "C" fn surface_gpu_pool_slot_frame(
    node_host: *mut c_void,
    surface: ModuleHostTableSurfaceHandle,
    gpu_pool_slot_frame: *mut ModuleHostTablePayloadDescriptor,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let Some(gpu_pool_slot_frame) = (unsafe { gpu_pool_slot_frame.as_mut() }) else {
        return unsafe { refuse(error_message, "no slot was handed for the descriptor") };
    };
    match node_host
        .surfaces_lent_to_native_code
        .gpu_pool_slot_frame_of_a_pool_texture(surface)
    {
        Ok(descriptor) => {
            *gpu_pool_slot_frame = ModuleHostTablePayloadDescriptor {
                kind: descriptor.kind,
                body: descriptor.body,
            };
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

unsafe extern "C" fn stop_requested(node_host: *mut c_void) -> u32 {
    let node_host = unsafe { node_host_of(node_host) };
    u32::from(node_host.stop_requested_by_parent.load(Ordering::Acquire))
}

unsafe extern "C" fn monotonic_now_ns(_node_host: *mut c_void) -> i64 {
    monotonic_clock_now_ns() as i64
}

unsafe extern "C" fn stream_time_ns_of_a_machine_instant(
    _node_host: *mut c_void,
    machine_instant_ns: i64,
) -> i64 {
    helper_stream_time_ns_of_a_machine_instant(machine_instant_ns)
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

unsafe extern "C" fn call_refused_while_a_frame_runs(
    _node_host: *mut c_void,
    call_does: u32,
    call: ModuleHostTableUtf8,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK hands a view live for the call.
    let Ok(call) = (unsafe { call.as_str() }) else {
        return unsafe { refuse(error_message, "the call's name is not UTF-8") };
    };
    let does = match call_does {
        MODULE_HOST_TABLE_CALL_CREATES_A_KERNEL => CallTooSlowForAFrame::CreatesAKernel,
        MODULE_HOST_TABLE_CALL_OPENS_A_DEVICE => CallTooSlowForAFrame::OpensADevice,
        MODULE_HOST_TABLE_CALL_STARTS_A_SESSION => CallTooSlowForAFrame::StartsASession,
        unknown => {
            return unsafe {
                refuse(
                    error_message,
                    &format!("{unknown} names nothing a call too slow for a frame does"),
                )
            };
        }
    };
    // SAFETY: the SDK hands an error message live for the call, or null.
    unsafe { refused_while_a_frame_runs(error_message, does, call) }
        .unwrap_or(MODULE_HOST_TABLE_STATUS_OK)
}

unsafe extern "C" fn shared_native_service_lease_acquire(
    node_host: *mut c_void,
    service_name: ModuleHostTableUtf8,
    granted_lease: *mut ModuleHostTableSharedNativeServiceLease,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK hands an error message live for the call, or null.
    if let Some(refused) = unsafe {
        refused_while_a_frame_runs(
            error_message,
            CallTooSlowForAFrame::StartsASession,
            "lease_shared_native_service",
        )
    } {
        return refused;
    }
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
