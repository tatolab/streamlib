// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a module built against this SDK does with tables of other versions,
//! over a host stood up here in Rust.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32,
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16, MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES,
    MODULE_HOST_TABLE_STATUS_EMPTY, MODULE_HOST_TABLE_STATUS_ENDED, MODULE_HOST_TABLE_STATUS_OK,
    MODULE_HOST_TABLE_STATUS_REFUSED, MODULE_HOST_TABLE_STATUS_TIMED_OUT,
    MODULE_HOST_TABLE_TIMESTAMP_NOW,
    MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_EVERY_BUFFER_IS_HELD_BY_THE_CONSUMER,
    ModuleHostTableAudioBlockRequestedByDevice, ModuleHostTableAudioBlockRequestedByDeviceCallback,
    ModuleHostTableAudioCaptureStreamRequest, ModuleHostTableAudioPlaybackStreamRequest,
    ModuleHostTableAudioStreamFormat, ModuleHostTableAudioStreamHandle, ModuleHostTableBytes,
    ModuleHostTableCapturedAudioBlock, ModuleHostTableCapturedAudioBlockCallback,
    ModuleHostTableColorInfo, ModuleHostTableDmaBufImport, ModuleHostTableErrorMessage,
    ModuleHostTableHostMemoryPixels, ModuleHostTableHostMemoryPixelsToFill,
    ModuleHostTableIOSurfaceImport, ModuleHostTableNodeOutputTextureRequest,
    ModuleHostTableOpenedAudioPlaybackStream, ModuleHostTablePayloadDescriptor,
    ModuleHostTablePortHandle, ModuleHostTableReadMessage, ModuleHostTableSharedNativeServiceLease,
    ModuleHostTableSharedNativeServiceLeaseHandle, ModuleHostTableStatus,
    ModuleHostTableSurfaceHandle, ModuleHostTableUtf8, ModuleHostTableVirtualCameraStreamHandle,
};
use tatolab_module_sdk::{
    MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR, ModuleHost,
    ModuleHostAudioCaptureStreamRequest, ModuleHostAudioPlaybackStreamRequest,
    ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat, ModuleHostColorInfo, ModuleHostError,
    ModuleHostForeignDmaBuf, ModuleHostForeignIOSurface, ModuleHostHostMemoryPixels,
    ModuleHostHostMemoryPixelsToFill, ModuleHostLogLevel, ModuleHostNodeOutputTextureRequest,
    ModuleHostSharedNativeServiceState, ModuleHostTable, ModuleHostVirtualCameraFramePresentation,
    refuse_a_module_host_table_this_module_cannot_read,
};

mod audio_and_virtual_camera_stream_tests;
mod port_surface_and_node_host_verb_tests;
mod stood_up_audio_and_virtual_camera_streams;
mod table_loading_tests;

use stood_up_audio_and_virtual_camera_streams::*;

/// One message a stood-up host was handed by a write.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WriteTheHostRecorded {
    port: u64,
    payload: Vec<u8>,
    timestamp_ns: i64,
    starts_group: bool,
}

/// The node host behind a stood-up table.
#[derive(Default)]
struct StoodUpNodeHost {
    references: AtomicUsize,
    writes: Mutex<Vec<WriteTheHostRecorded>>,
    described_writes: Mutex<Vec<(WriteTheHostRecorded, ModuleHostTablePayloadDescriptor)>>,
    log_records: Mutex<Vec<(u32, String, String)>>,
    released_reads: AtomicUsize,
    read_already: AtomicU32,
    stop_requested: AtomicU32,
    audio_capture: Mutex<StoodUpAudioCaptureStream>,
    surface_calls: Mutex<Vec<String>>,
    audio_playback: Mutex<StoodUpAudioPlaybackStream>,
    lease_calls: Mutex<Vec<String>>,
    node_failure_reasons: Mutex<Vec<String>>,
    virtual_camera_calls: Mutex<Vec<String>>,
    given_up_group_declarations: Mutex<Vec<(u64, u64)>>,
}

/// What the stood-up host knows of the one playback stream it opens.
#[derive(Default)]
struct StoodUpAudioPlaybackStream {
    requested_device_id: Option<String>,
    requested_format: Option<(u32, u32, u32)>,
    installed_callback: Option<(ModuleHostTableAudioBlockRequestedByDeviceCallback, usize)>,
    ended_with: Option<String>,
    closed: bool,
}

/// What the stood-up host knows of the one capture stream it opens.
#[derive(Default)]
struct StoodUpAudioCaptureStream {
    requested_device_id: Option<String>,
    requested_format: Option<(u32, u32, u32)>,
    installed_callback: Option<(ModuleHostTableCapturedAudioBlockCallback, usize)>,
    ended_with: Option<String>,
    closed: bool,
}

/// The handle the stood-up host opens its capture stream under.
const STOOD_UP_AUDIO_STREAM_HANDLE: u64 = 3;

fn node_host_of(node_host: *mut c_void) -> &'static StoodUpNodeHost {
    // SAFETY: every table here names a leaked `StoodUpNodeHost`.
    unsafe { &*node_host.cast::<StoodUpNodeHost>() }
}

unsafe extern "C" fn retain(node_host: *mut c_void) {
    node_host_of(node_host)
        .references
        .fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn release(node_host: *mut c_void) {
    node_host_of(node_host)
        .references
        .fetch_sub(1, Ordering::SeqCst);
}

unsafe extern "C" fn open_port_named_lent(
    _node_host: *mut c_void,
    port_name: ModuleHostTableUtf8,
    opened_port: *mut ModuleHostTablePortHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live view and live out-pointers.
    let port_name = unsafe { port_name.as_str() }.unwrap_or("");
    if port_name != "lent" {
        unsafe { (*error_message).write(&format!("port {port_name:?} was not lent")) };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    unsafe { (*opened_port).value = 7 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn write(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    payload: ModuleHostTableBytes,
    timestamp_ns: i64,
    starts_group: u32,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    node_host_of(node_host)
        .writes
        .lock()
        .unwrap()
        .push(WriteTheHostRecorded {
            port: port.value,
            // SAFETY: the SDK passes the payload it was handed.
            payload: unsafe { payload.as_slice() }.to_vec(),
            timestamp_ns,
            starts_group: starts_group != 0,
        });
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn refuse_every_stream_facts(
    _node_host: *mut c_void,
    _port: ModuleHostTablePortHandle,
    _family: ModuleHostTableUtf8,
    _stream_facts_json: ModuleHostTableUtf8,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    unsafe { (*error_message).write("the facts are another family's") };
    MODULE_HOST_TABLE_STATUS_REFUSED
}

static ONE_PAYLOAD_TO_READ: [u8; 3] = [1, 2, 3];

unsafe extern "C" fn read_one_message_then_empty(
    node_host: *mut c_void,
    _port: ModuleHostTablePortHandle,
    read_message: *mut ModuleHostTableReadMessage,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    if node_host_of(node_host)
        .read_already
        .swap(1, Ordering::SeqCst)
        == 1
    {
        return MODULE_HOST_TABLE_STATUS_EMPTY;
    }
    unsafe {
        (*read_message).payload = ONE_PAYLOAD_TO_READ.as_ptr();
        (*read_message).payload_length = ONE_PAYLOAD_TO_READ.len();
        (*read_message).timestamp_ns = 42;
        (*read_message).starts_group = 1;
        (*read_message).lost_since_last_read = 3;
    }
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn count_a_released_read(
    node_host: *mut c_void,
    _read_message: *mut ModuleHostTableReadMessage,
) {
    node_host_of(node_host)
        .released_reads
        .fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn wait_times_out(
    _node_host: *mut c_void,
    _timeout_ns: u64,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_TIMED_OUT
}

unsafe extern "C" fn stop_requested(node_host: *mut c_void) -> u32 {
    node_host_of(node_host)
        .stop_requested
        .load(Ordering::SeqCst)
}

unsafe extern "C" fn monotonic_now_ns(_node_host: *mut c_void) -> i64 {
    1_000
}

unsafe extern "C" fn stream_time_at_half_the_machine_instant(
    _node_host: *mut c_void,
    machine_instant_ns: i64,
) -> i64 {
    machine_instant_ns / 2
}

unsafe extern "C" fn log(
    node_host: *mut c_void,
    level: u32,
    target: ModuleHostTableUtf8,
    message: ModuleHostTableUtf8,
) {
    // SAFETY: the SDK passes live views.
    let (target, message) = unsafe { (target.as_str().unwrap(), message.as_str().unwrap()) };
    node_host_of(node_host).log_records.lock().unwrap().push((
        level,
        target.to_string(),
        message.to_string(),
    ));
}

unsafe extern "C" fn host_capabilities_json(_node_host: *mut c_void) -> ModuleHostTableUtf8 {
    ModuleHostTableUtf8::of("{\"platform\":\"stood-up\"}")
}

unsafe extern "C" fn import_dma_buf_as_surface_3(
    node_host: *mut c_void,
    dma_buf_import: *const ModuleHostTableDmaBufImport,
    imported_surface: *mut ModuleHostTableSurfaceHandle,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live request and a live out-pointer.
    let dma_buf_import = unsafe { &*dma_buf_import };
    let pixel_format = unsafe { dma_buf_import.pixel_format.as_str() }.unwrap_or("");
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "import fd={} {}x{} {pixel_format} planes={} offsets={:?} strides={:?} size={}",
            dma_buf_import.fd,
            dma_buf_import.width,
            dma_buf_import.height,
            dma_buf_import.plane_count,
            &dma_buf_import.plane_offsets_bytes[..dma_buf_import.plane_count as usize],
            &dma_buf_import.plane_strides_bytes[..dma_buf_import.plane_count as usize],
            dma_buf_import.allocation_byte_size,
        ));
    unsafe { (*imported_surface).value = 3 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn acquire_output_texture_as_surface_4(
    node_host: *mut c_void,
    request: *const ModuleHostTableNodeOutputTextureRequest,
    acquired_surface: *mut ModuleHostTableSurfaceHandle,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as above.
    let request = unsafe { &*request };
    let (pool_key, texture_format, usage) = unsafe {
        (
            request.pool_key.as_str().unwrap_or(""),
            request.texture_format.as_str().unwrap_or(""),
            request.usage.as_str().unwrap_or(""),
        )
    };
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "acquire {pool_key} depth={} {}x{} {texture_format} {usage}",
            request.rotation_depth, request.width, request.height,
        ));
    unsafe { (*acquired_surface).value = 4 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn convert_refusing_an_unknown_range(
    node_host: *mut c_void,
    source: ModuleHostTableSurfaceHandle,
    destination: ModuleHostTableSurfaceHandle,
    source_color: *const ModuleHostTableColorInfo,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as above.
    let source_color = unsafe { &*source_color };
    let (matrix, range) = unsafe {
        (
            source_color.matrix.as_str().unwrap_or(""),
            source_color.range.as_str().unwrap_or(""),
        )
    };
    if range == "tv" {
        unsafe { (*error_message).write("\"tv\" is not a range name") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "convert {} -> {} matrix={matrix:?} range={range:?}",
            source.value, destination.value,
        ));
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn import_iosurface_as_surface_5(
    node_host: *mut c_void,
    iosurface_import: *const ModuleHostTableIOSurfaceImport,
    imported_surface: *mut ModuleHostTableSurfaceHandle,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live request and a live out-pointer.
    let iosurface_import = unsafe { &*iosurface_import };
    let pixel_format = unsafe { iosurface_import.pixel_format.as_str() }.unwrap_or("");
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "import iosurface={:#x} {}x{} {pixel_format}",
            iosurface_import.iosurface as usize, iosurface_import.width, iosurface_import.height,
        ));
    unsafe { (*imported_surface).value = 5 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn lend_by_surface_id_as_surface_6(
    node_host: *mut c_void,
    surface_id: ModuleHostTableUtf8,
    lent_surface: *mut ModuleHostTableSurfaceHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live view and live out-pointers.
    let surface_id = unsafe { surface_id.as_str() }.unwrap_or("");
    if surface_id.is_empty() {
        unsafe { (*error_message).write("no surface id was named") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!("lend {surface_id}"));
    unsafe { (*lent_surface).value = 6 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn surface_id_naming_the_handle(
    _node_host: *mut c_void,
    surface: ModuleHostTableSurfaceHandle,
) -> ModuleHostTableUtf8 {
    match surface.value {
        3 => ModuleHostTableUtf8::of("imported-3"),
        4 => ModuleHostTableUtf8::of("pool-slot#4"),
        5 => ModuleHostTableUtf8::of("imported-5"),
        6 => ModuleHostTableUtf8::of("upstream-pool-slot#6"),
        8 => ModuleHostTableUtf8::of("upstream-pool-slot#8"),
        _ => ModuleHostTableUtf8::of(""),
    }
}

unsafe extern "C" fn record_a_released_surface(
    node_host: *mut c_void,
    surface: ModuleHostTableSurfaceHandle,
) {
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!("release {}", surface.value));
}

unsafe extern "C" fn read_into_host_memory_filling_every_byte_with_the_handle(
    node_host: *mut c_void,
    source: ModuleHostTableSurfaceHandle,
    host_memory_pixels_to_fill: *const ModuleHostTableHostMemoryPixelsToFill,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live description of memory it lends mutably.
    let to_fill = unsafe { &*host_memory_pixels_to_fill };
    let pixel_format = unsafe { to_fill.pixel_format.as_str() }.unwrap_or("");
    let pixels =
        unsafe { std::slice::from_raw_parts_mut(to_fill.pixels.bytes, to_fill.pixels.length) };
    pixels.fill(source.value as u8);
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "read {} {}x{} {pixel_format} stride={} bytes={}",
            source.value,
            to_fill.width,
            to_fill.height,
            to_fill.row_stride_bytes,
            pixels.len(),
        ));
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn write_from_host_memory_refusing_a_short_buffer(
    node_host: *mut c_void,
    destination: ModuleHostTableSurfaceHandle,
    host_memory_pixels: *const ModuleHostTableHostMemoryPixels,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live description and live out-pointers.
    let host_memory_pixels = unsafe { &*host_memory_pixels };
    let pixel_format = unsafe { host_memory_pixels.pixel_format.as_str() }.unwrap_or("");
    let pixels = unsafe { host_memory_pixels.pixels.as_slice() };
    if pixels.is_empty() {
        unsafe { (*error_message).write("no pixels were handed") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "write {} {}x{} {pixel_format} stride={} bytes={} first={}",
            destination.value,
            host_memory_pixels.width,
            host_memory_pixels.height,
            host_memory_pixels.row_stride_bytes,
            pixels.len(),
            pixels[0],
        ));
    MODULE_HOST_TABLE_STATUS_OK
}

/// The handle the stood-up host grants its one lease under.
const STOOD_UP_SERVICE_LEASE_HANDLE: u64 = 7;

static STOOD_UP_SERVICE_ENDPOINT: &str = "/run/tatolab/services/browser-host.sock";
static STOOD_UP_SERVICE_TENANT: &str = "lower-third";

unsafe extern "C" fn acquire_lease_7_on_a_service_named_browser_host(
    node_host: *mut c_void,
    service_name: ModuleHostTableUtf8,
    granted_lease: *mut ModuleHostTableSharedNativeServiceLease,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes live text and live out-pointers.
    let service_name = unsafe { service_name.as_str() }.unwrap_or("");
    node_host_of(node_host)
        .lease_calls
        .lock()
        .unwrap()
        .push(format!("acquire {service_name}"));
    if service_name != "browser-host" {
        unsafe { (*error_message).write(&format!("this runtime offers no {service_name}")) };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    unsafe {
        *granted_lease = ModuleHostTableSharedNativeServiceLease {
            lease: ModuleHostTableSharedNativeServiceLeaseHandle {
                value: STOOD_UP_SERVICE_LEASE_HANDLE,
            },
            endpoint_socket_path: ModuleHostTableUtf8::of(STOOD_UP_SERVICE_ENDPOINT),
            tenant_stream_id: ModuleHostTableUtf8::of(STOOD_UP_SERVICE_TENANT),
        };
    }
    MODULE_HOST_TABLE_STATUS_OK
}

/// Starting again on the first ask, running on the second, failed after.
unsafe extern "C" fn lease_state_starting_again_after_one_ask(
    node_host: *mut c_void,
    lease: ModuleHostTableSharedNativeServiceLeaseHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let mut lease_calls = node_host_of(node_host).lease_calls.lock().unwrap();
    let asks_before = lease_calls
        .iter()
        .filter(|call| call.starts_with("state"))
        .count();
    lease_calls.push(format!("state {}", lease.value));
    match asks_before {
        0 => MODULE_HOST_TABLE_STATUS_EMPTY,
        1 => MODULE_HOST_TABLE_STATUS_OK,
        _ => {
            unsafe { (*error_message).write("the browser host crashed 5 times in a row") };
            MODULE_HOST_TABLE_STATUS_ENDED
        }
    }
}

unsafe extern "C" fn record_a_released_lease(
    node_host: *mut c_void,
    lease: ModuleHostTableSharedNativeServiceLeaseHandle,
) {
    node_host_of(node_host)
        .lease_calls
        .lock()
        .unwrap()
        .push(format!("release {}", lease.value));
}

unsafe extern "C" fn record_a_node_failure(
    node_host: *mut c_void,
    reason: ModuleHostTableUtf8,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    node_host_of(node_host)
        .node_failure_reasons
        .lock()
        .unwrap()
        .push(unsafe { reason.as_str() }.unwrap().to_string());
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn payload_descriptor_of_a_packets_unit_named_h264(
    _node_host: *mut c_void,
    _read_message: *const ModuleHostTableReadMessage,
    payload_descriptor: *mut ModuleHostTablePayloadDescriptor,
) -> ModuleHostTableStatus {
    let mut body = [0u8; MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES];
    body[..4].copy_from_slice(b"h264");
    unsafe { *payload_descriptor = ModuleHostTablePayloadDescriptor { kind: 4, body } };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn lend_the_pooled_frame_as_surface_8(
    node_host: *mut c_void,
    _read_message: *const ModuleHostTableReadMessage,
    lent_surface: *mut ModuleHostTableSurfaceHandle,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push("lend pooled frame".to_string());
    unsafe { *lent_surface = ModuleHostTableSurfaceHandle { value: 8 } };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn output_port_write_described(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    payload: ModuleHostTableBytes,
    timestamp_ns: i64,
    starts_group: u32,
    payload_descriptor: *const ModuleHostTablePayloadDescriptor,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let payload_descriptor = unsafe { *payload_descriptor };
    node_host_of(node_host)
        .described_writes
        .lock()
        .unwrap()
        .push((
            WriteTheHostRecorded {
                port: port.value,
                payload: unsafe { payload.as_slice() }.to_vec(),
                timestamp_ns,
                starts_group: starts_group != 0,
            },
            payload_descriptor,
        ));
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn gpu_pool_slot_frame_naming_the_handle(
    _node_host: *mut c_void,
    surface: ModuleHostTableSurfaceHandle,
    gpu_pool_slot_frame: *mut ModuleHostTablePayloadDescriptor,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    if surface.value != 4 {
        unsafe { (*error_message).write("only a node output pool texture has a slot") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    let mut body = [0u8; MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES];
    body[16..20].copy_from_slice(&surface.value.to_le_bytes()[..4]);
    unsafe { *gpu_pool_slot_frame = ModuleHostTablePayloadDescriptor { kind: 1, body } };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn declare_given_up_groups_refusing_none(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    given_up_group_count: u64,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    if given_up_group_count == 0 {
        unsafe { (*error_message).write("no group was given up") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    node_host_of(node_host)
        .given_up_group_declarations
        .lock()
        .unwrap()
        .push((port.value, given_up_group_count));
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn a_verb_added_at_minor_13(_node_host: *mut c_void) -> u32 {
    13
}

/// v1.12's fields, then one verb a later minor appended.
#[repr(C)]
struct ModuleHostTableAtMinor13 {
    fields_of_minor_12: ModuleHostTable,
    a_verb_added_at_minor_13: unsafe extern "C" fn(*mut c_void) -> u32,
}

fn stood_up_node_host() -> &'static StoodUpNodeHost {
    Box::leak(Box::new(StoodUpNodeHost::default()))
}

fn v1_12_table_over(node_host: &'static StoodUpNodeHost) -> ModuleHostTable {
    ModuleHostTable {
        abi_major: 1,
        abi_minor: 12,
        table_size_bytes: std::mem::size_of::<ModuleHostTable>() as u64,
        node_host: std::ptr::from_ref(node_host).cast_mut().cast(),
        node_host_retain: retain,
        node_host_release: release,
        output_port_open: open_port_named_lent,
        output_port_write: write,
        output_port_set_stream_facts: refuse_every_stream_facts,
        input_port_open: open_port_named_lent,
        input_port_read: read_one_message_then_empty,
        input_message_release: count_a_released_read,
        input_wait_for_any_port: wait_times_out,
        stop_requested,
        monotonic_now_ns,
        log,
        host_capabilities_json,
        audio_capture_stream_open,
        audio_capture_stream_start,
        audio_capture_stream_stop,
        audio_capture_stream_liveness,
        audio_capture_stream_close,
        surface_import_dma_buf: import_dma_buf_as_surface_3,
        output_texture_acquire_from_node_output_pool: acquire_output_texture_as_surface_4,
        surface_convert_into: convert_refusing_an_unknown_range,
        surface_id: surface_id_naming_the_handle,
        surface_release: record_a_released_surface,
        audio_playback_stream_open,
        audio_playback_stream_start,
        audio_playback_stream_stop,
        audio_playback_stream_liveness,
        audio_playback_stream_close,
        surface_import_iosurface: import_iosurface_as_surface_5,
        surface_write_from_host_memory: write_from_host_memory_refusing_a_short_buffer,
        surface_lend_by_surface_id: lend_by_surface_id_as_surface_6,
        shared_native_service_lease_acquire: acquire_lease_7_on_a_service_named_browser_host,
        shared_native_service_lease_state: lease_state_starting_again_after_one_ask,
        shared_native_service_lease_release: record_a_released_lease,
        node_fail_unrecoverably: record_a_node_failure,
        virtual_camera_stream_open: virtual_camera_stream_open_refusing_an_empty_name,
        virtual_camera_stream_offer_extent,
        virtual_camera_stream_write_yuyv_frame:
            virtual_camera_stream_write_yuyv_frame_held_by_the_consumer,
        virtual_camera_stream_liveness: virtual_camera_stream_liveness_ended,
        virtual_camera_stream_close,
        surface_read_into_host_memory: read_into_host_memory_filling_every_byte_with_the_handle,
        stream_time_ns_of_a_machine_instant: stream_time_at_half_the_machine_instant,
        input_message_payload_descriptor: payload_descriptor_of_a_packets_unit_named_h264,
        input_message_lend_pooled_gpu_frame: lend_the_pooled_frame_as_surface_8,
        output_port_write_described,
        surface_gpu_pool_slot_frame: gpu_pool_slot_frame_naming_the_handle,
        output_port_declare_groups_given_up_before_the_next_write:
            declare_given_up_groups_refusing_none,
    }
}
