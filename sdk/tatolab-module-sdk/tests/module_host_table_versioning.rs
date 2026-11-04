// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a module built against this SDK does with tables of other versions,
//! over a host stood up here in Rust.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32,
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16, MODULE_HOST_TABLE_STATUS_EMPTY,
    MODULE_HOST_TABLE_STATUS_ENDED, MODULE_HOST_TABLE_STATUS_OK, MODULE_HOST_TABLE_STATUS_REFUSED,
    MODULE_HOST_TABLE_STATUS_TIMED_OUT, MODULE_HOST_TABLE_TIMESTAMP_NOW,
    MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_EVERY_BUFFER_IS_HELD_BY_THE_CONSUMER,
    ModuleHostTableAudioBlockRequestedByDevice, ModuleHostTableAudioBlockRequestedByDeviceCallback,
    ModuleHostTableAudioCaptureStreamRequest, ModuleHostTableAudioPlaybackStreamRequest,
    ModuleHostTableAudioStreamFormat, ModuleHostTableAudioStreamHandle, ModuleHostTableBytes,
    ModuleHostTableCapturedAudioBlock, ModuleHostTableCapturedAudioBlockCallback,
    ModuleHostTableColorInfo, ModuleHostTableDmaBufImport, ModuleHostTableErrorMessage,
    ModuleHostTableHostMemoryPixels, ModuleHostTableIOSurfaceImport,
    ModuleHostTableNodeOutputTextureRequest, ModuleHostTableOpenedAudioPlaybackStream,
    ModuleHostTablePortHandle, ModuleHostTableReadMessage, ModuleHostTableSharedNativeServiceLease,
    ModuleHostTableSharedNativeServiceLeaseHandle, ModuleHostTableStatus,
    ModuleHostTableSurfaceHandle, ModuleHostTableUtf8, ModuleHostTableVirtualCameraStreamHandle,
};
use tatolab_module_sdk::{
    MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR, ModuleHost,
    ModuleHostAudioCaptureStreamRequest, ModuleHostAudioPlaybackStreamRequest,
    ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat, ModuleHostColorInfo, ModuleHostError,
    ModuleHostForeignDmaBuf, ModuleHostForeignIOSurface, ModuleHostHostMemoryPixels,
    ModuleHostLogLevel, ModuleHostNodeOutputTextureRequest, ModuleHostSharedNativeServiceState,
    ModuleHostTable, ModuleHostVirtualCameraFramePresentation,
    refuse_a_module_host_table_this_module_cannot_read,
};

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
    log_records: Mutex<Vec<(u32, String, String)>>,
    released_reads: AtomicUsize,
    stop_requested: AtomicU32,
    audio_capture: Mutex<StoodUpAudioCaptureStream>,
    surface_calls: Mutex<Vec<String>>,
    audio_playback: Mutex<StoodUpAudioPlaybackStream>,
    lease_calls: Mutex<Vec<String>>,
    node_failure_reasons: Mutex<Vec<String>>,
    virtual_camera_calls: Mutex<Vec<String>>,
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

unsafe extern "C" fn refuse_every_catalog_entry(
    _node_host: *mut c_void,
    _port: ModuleHostTablePortHandle,
    _section: ModuleHostTableUtf8,
    _entry_json: ModuleHostTableUtf8,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    unsafe { (*error_message).write("the entry does not parse") };
    MODULE_HOST_TABLE_STATUS_REFUSED
}

static ONE_PAYLOAD_TO_READ: [u8; 3] = [1, 2, 3];

unsafe extern "C" fn read_one_message_then_empty(
    _node_host: *mut c_void,
    _port: ModuleHostTablePortHandle,
    read_message: *mut ModuleHostTableReadMessage,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    static READ_ALREADY: AtomicU32 = AtomicU32::new(0);
    if READ_ALREADY.swap(1, Ordering::SeqCst) == 1 {
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

unsafe extern "C" fn audio_capture_stream_open(
    node_host: *mut c_void,
    request: *const ModuleHostTableAudioCaptureStreamRequest,
    opened_stream: *mut ModuleHostTableAudioStreamHandle,
    opened_format: *mut ModuleHostTableAudioStreamFormat,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live request and live out-pointers.
    let request = unsafe { &*request };
    let device_id = unsafe { request.device_id.as_str() }.unwrap().to_string();
    if device_id == "unplugged" {
        unsafe { (*error_message).write("audio device 'unplugged' is not attached") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    let mut audio_capture = node_host_of(node_host).audio_capture.lock().unwrap();
    audio_capture.requested_device_id = Some(device_id);
    audio_capture.requested_format = Some((
        request.requested_format.sample_rate,
        request.requested_format.channels,
        request.requested_format.sample_format,
    ));
    unsafe {
        (*opened_stream).value = STOOD_UP_AUDIO_STREAM_HANDLE;
        *opened_format = ModuleHostTableAudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16,
        };
    }
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn audio_capture_stream_start(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
    block_callback: ModuleHostTableCapturedAudioBlockCallback,
    callback_context: *mut c_void,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    assert_eq!(stream.value, STOOD_UP_AUDIO_STREAM_HANDLE);
    node_host_of(node_host)
        .audio_capture
        .lock()
        .unwrap()
        .installed_callback = Some((block_callback, callback_context as usize));
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn audio_capture_stream_stop(
    node_host: *mut c_void,
    _stream: ModuleHostTableAudioStreamHandle,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    node_host_of(node_host)
        .audio_capture
        .lock()
        .unwrap()
        .installed_callback = None;
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn audio_capture_stream_liveness(
    node_host: *mut c_void,
    _stream: ModuleHostTableAudioStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    match &node_host_of(node_host)
        .audio_capture
        .lock()
        .unwrap()
        .ended_with
    {
        Some(failure) => {
            unsafe { (*error_message).write(failure) };
            MODULE_HOST_TABLE_STATUS_ENDED
        }
        None => MODULE_HOST_TABLE_STATUS_OK,
    }
}

unsafe extern "C" fn audio_capture_stream_close(
    node_host: *mut c_void,
    _stream: ModuleHostTableAudioStreamHandle,
) {
    let mut audio_capture = node_host_of(node_host).audio_capture.lock().unwrap();
    audio_capture.installed_callback = None;
    audio_capture.closed = true;
}

unsafe extern "C" fn audio_playback_stream_open(
    node_host: *mut c_void,
    request: *const ModuleHostTableAudioPlaybackStreamRequest,
    opened_stream: *mut ModuleHostTableAudioStreamHandle,
    opened_playback_stream: *mut ModuleHostTableOpenedAudioPlaybackStream,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live request and live out-pointers.
    let request = unsafe { &*request };
    let device_id = unsafe { request.device_id.as_str() }.unwrap().to_string();
    if device_id == "unplugged" {
        unsafe { (*error_message).write("audio device 'unplugged' is not attached") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    let mut audio_playback = node_host_of(node_host).audio_playback.lock().unwrap();
    audio_playback.requested_device_id = Some(device_id);
    audio_playback.requested_format = Some((
        request.requested_format.sample_rate,
        request.requested_format.channels,
        request.requested_format.sample_format,
    ));
    let opened_sample_format = match request.requested_format.sample_format {
        MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY => MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32,
        requested => requested,
    };
    unsafe {
        (*opened_stream).value = STOOD_UP_AUDIO_STREAM_HANDLE;
        *opened_playback_stream = ModuleHostTableOpenedAudioPlaybackStream {
            stream_format: ModuleHostTableAudioStreamFormat {
                sample_rate: request.requested_format.sample_rate.max(44_100),
                channels: request.requested_format.channels.max(1),
                sample_format: opened_sample_format,
            },
            device_period_in_per_channel_samples: if request.requested_format.channels == 0 {
                0
            } else {
                256
            },
        };
    }
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn audio_playback_stream_start(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
    fill_callback: ModuleHostTableAudioBlockRequestedByDeviceCallback,
    callback_context: *mut c_void,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    assert_eq!(stream.value, STOOD_UP_AUDIO_STREAM_HANDLE);
    node_host_of(node_host)
        .audio_playback
        .lock()
        .unwrap()
        .installed_callback = Some((fill_callback, callback_context as usize));
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn audio_playback_stream_stop(
    node_host: *mut c_void,
    _stream: ModuleHostTableAudioStreamHandle,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    node_host_of(node_host)
        .audio_playback
        .lock()
        .unwrap()
        .installed_callback = None;
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn audio_playback_stream_liveness(
    node_host: *mut c_void,
    _stream: ModuleHostTableAudioStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    match &node_host_of(node_host)
        .audio_playback
        .lock()
        .unwrap()
        .ended_with
    {
        Some(failure) => {
            unsafe { (*error_message).write(failure) };
            MODULE_HOST_TABLE_STATUS_ENDED
        }
        None => MODULE_HOST_TABLE_STATUS_OK,
    }
}

unsafe extern "C" fn audio_playback_stream_close(
    node_host: *mut c_void,
    _stream: ModuleHostTableAudioStreamHandle,
) {
    let mut audio_playback = node_host_of(node_host).audio_playback.lock().unwrap();
    audio_playback.installed_callback = None;
    audio_playback.closed = true;
}

/// Ask for one device buffer of `byte_count` bytes the way the host's device
/// thread does, through whatever callback is installed; `None` when none is.
fn ask_for_one_block_through_the_installed_callback(
    node_host: &StoodUpNodeHost,
    byte_count: usize,
    sample_count: u32,
) -> Option<Vec<u8>> {
    let (fill_callback, callback_context) = node_host
        .audio_playback
        .lock()
        .unwrap()
        .installed_callback?;
    let mut device_buffer = vec![0xEEu8; byte_count];
    let requested_block = ModuleHostTableAudioBlockRequestedByDevice {
        interleaved_sample_bytes_to_fill: device_buffer.as_mut_ptr(),
        interleaved_sample_byte_count: device_buffer.len(),
        sample_count,
        reserved: 0,
    };
    unsafe { fill_callback(callback_context as *mut c_void, &requested_block) };
    Some(device_buffer)
}

/// Deliver one block the way the host's device thread does, through whatever
/// callback is installed; `false` when none is.
fn deliver_one_block_through_the_installed_callback(
    node_host: &StoodUpNodeHost,
    interleaved_sample_bytes: &[u8],
    sample_count: u32,
    first_sample_timestamp_ns: i64,
) -> bool {
    let Some((block_callback, callback_context)) =
        node_host.audio_capture.lock().unwrap().installed_callback
    else {
        return false;
    };
    let captured_block = ModuleHostTableCapturedAudioBlock {
        interleaved_sample_bytes: ModuleHostTableBytes::of(interleaved_sample_bytes),
        sample_count,
        reserved: 0,
        first_sample_timestamp_ns,
    };
    unsafe { block_callback(callback_context as *mut c_void, &captured_block) };
    true
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

unsafe extern "C" fn virtual_camera_stream_open_refusing_an_empty_name(
    node_host: *mut c_void,
    camera_name: ModuleHostTableUtf8,
    opened_camera: *mut ModuleHostTableVirtualCameraStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live view and live out-pointers.
    let camera_name = unsafe { camera_name.as_str() }.unwrap_or("");
    if camera_name.is_empty() {
        unsafe { (*error_message).write("no PipeWire daemon answers") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    node_host_of(node_host)
        .virtual_camera_calls
        .lock()
        .unwrap()
        .push(format!("open {camera_name}"));
    unsafe { (*opened_camera).value = 9 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn virtual_camera_stream_offer_extent(
    node_host: *mut c_void,
    camera: ModuleHostTableVirtualCameraStreamHandle,
    width: u32,
    height: u32,
    frames_per_second: u32,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    node_host_of(node_host)
        .virtual_camera_calls
        .lock()
        .unwrap()
        .push(format!(
            "offer {} {width}x{height} fps={frames_per_second}",
            camera.value
        ));
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn virtual_camera_stream_write_yuyv_frame_held_by_the_consumer(
    node_host: *mut c_void,
    camera: ModuleHostTableVirtualCameraStreamHandle,
    yuyv_rows: ModuleHostTableBytes,
    stride_bytes: u32,
    timestamp_ns: i64,
    frame_presentation: *mut u32,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes live rows and a live out-pointer.
    let yuyv_rows = unsafe { yuyv_rows.as_slice() };
    node_host_of(node_host)
        .virtual_camera_calls
        .lock()
        .unwrap()
        .push(format!(
            "write {} bytes={} stride={stride_bytes} at={timestamp_ns}",
            camera.value,
            yuyv_rows.len()
        ));
    unsafe {
        *frame_presentation =
            MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_EVERY_BUFFER_IS_HELD_BY_THE_CONSUMER
    };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn virtual_camera_stream_liveness_ended(
    _node_host: *mut c_void,
    _camera: ModuleHostTableVirtualCameraStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    unsafe { (*error_message).write("the PipeWire daemon went away") };
    MODULE_HOST_TABLE_STATUS_ENDED
}

unsafe extern "C" fn virtual_camera_stream_close(
    node_host: *mut c_void,
    camera: ModuleHostTableVirtualCameraStreamHandle,
) {
    node_host_of(node_host)
        .virtual_camera_calls
        .lock()
        .unwrap()
        .push(format!("close {}", camera.value));
}

unsafe extern "C" fn a_verb_added_at_minor_9(_node_host: *mut c_void) -> u32 {
    11
}

/// v1.8's fields, then one verb a later minor appended.
#[repr(C)]
struct ModuleHostTableAtMinor9 {
    fields_of_minor_8: ModuleHostTable,
    a_verb_added_at_minor_9: unsafe extern "C" fn(*mut c_void) -> u32,
}

fn stood_up_node_host() -> &'static StoodUpNodeHost {
    Box::leak(Box::new(StoodUpNodeHost::default()))
}

fn v1_8_table_over(node_host: &'static StoodUpNodeHost) -> ModuleHostTable {
    ModuleHostTable {
        abi_major: 1,
        abi_minor: 8,
        table_size_bytes: std::mem::size_of::<ModuleHostTable>() as u64,
        node_host: std::ptr::from_ref(node_host).cast_mut().cast(),
        node_host_retain: retain,
        node_host_release: release,
        output_port_open: open_port_named_lent,
        output_port_write: write,
        output_port_set_catalog_entry: refuse_every_catalog_entry,
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
    }
}

#[test]
fn this_sdk_reads_table_1_8() {
    assert_eq!(
        (MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR),
        (1, 8)
    );
}

#[test]
fn a_module_built_against_1_8_loads_on_a_runtime_lending_1_9_and_reaches_its_ports() {
    let node_host = stood_up_node_host();
    let mut fields_of_minor_8 = v1_8_table_over(node_host);
    fields_of_minor_8.abi_minor = 9;
    fields_of_minor_8.table_size_bytes = std::mem::size_of::<ModuleHostTableAtMinor9>() as u64;
    let table_at_minor_9 = ModuleHostTableAtMinor9 {
        fields_of_minor_8,
        a_verb_added_at_minor_9,
    };
    assert_eq!(
        unsafe { (table_at_minor_9.a_verb_added_at_minor_9)(std::ptr::null_mut()) },
        11
    );

    let module_host = unsafe {
        ModuleHost::from_module_host_table_pointer(
            std::ptr::from_ref(&table_at_minor_9).cast::<ModuleHostTable>(),
        )
    }
    .expect("a 1.8 module loads on a 1.9 runtime");

    assert_eq!(module_host.module_host_table_version(), (1, 9));
    let output_port = module_host
        .open_output_port("lent")
        .expect("the lent port opens");
    output_port.write(b"frame", Some(5), false).unwrap();
    output_port.write(b"stamped now", None, true).unwrap();
    assert_eq!(
        *node_host.writes.lock().unwrap(),
        vec![
            WriteTheHostRecorded {
                port: 7,
                payload: b"frame".to_vec(),
                timestamp_ns: 5,
                starts_group: false,
            },
            WriteTheHostRecorded {
                port: 7,
                payload: b"stamped now".to_vec(),
                timestamp_ns: MODULE_HOST_TABLE_TIMESTAMP_NOW,
                starts_group: true,
            },
        ]
    );
}

#[test]
fn a_runtime_of_another_major_is_refused_naming_both_versions() {
    let node_host = stood_up_node_host();
    let mut table = v1_8_table_over(node_host);
    table.abi_major = 2;
    table.abi_minor = 0;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert_eq!(
        refusal,
        ModuleHostError::ModuleHostTableMajorMismatch {
            runtime_major: 2,
            runtime_minor: 0,
            module_major: 1,
            module_minor: 8,
        }
    );
    let message = refusal.to_string();
    assert!(message.contains("module host table 1.8"), "{message}");
    assert!(message.contains("lends 2.0"), "{message}");
    assert_eq!(
        node_host.references.load(Ordering::SeqCst),
        0,
        "a refused table is never retained"
    );
}

#[test]
fn a_runtime_older_than_the_minor_a_module_needs_is_refused_naming_the_update() {
    let refusal =
        refuse_a_module_host_table_this_module_cannot_read(1, 0, 4096, 1, 1, 128).unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "this module needs module host table 1.1, and this runtime lends 1.0 — update the runtime"
    );
}

#[test]
fn a_1_8_module_is_refused_on_a_runtime_lending_1_1() {
    let node_host = stood_up_node_host();
    let mut table = v1_8_table_over(node_host);
    table.abi_minor = 1;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert!(matches!(
        refusal,
        ModuleHostError::ModuleHostTableOlderThanTheModuleNeeds {
            runtime_minor: 1,
            module_minor: 8,
            ..
        }
    ));
}

#[test]
fn the_surface_verbs_carry_their_requests_and_a_surface_is_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let imported = module_host
        .import_dma_buf(&ModuleHostForeignDmaBuf {
            fd: 9,
            width: 64,
            height: 16,
            pixel_format: "nv12_full_range",
            allocation_byte_size: 3_456,
            plane_offsets_bytes: &[0, 2_304],
            plane_strides_bytes: &[128, 128],
            drm_format_modifier: 0,
        })
        .expect("the import is taken");
    let output_texture = module_host
        .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
            pool_key: "video",
            rotation_depth: 3,
            width: 64,
            height: 16,
            texture_format: "rgba8_unorm",
            usage: &["storage_binding", "texture_binding"],
        })
        .expect("the acquire is taken");
    assert_eq!(imported.surface_id(), "imported-3");
    assert_eq!(output_texture.surface_id(), "pool-slot#4");

    module_host
        .convert_surface_into(
            &imported,
            &output_texture,
            &ModuleHostColorInfo {
                matrix: Some("bt709"),
                ..ModuleHostColorInfo::default()
            },
        )
        .expect("the conversion is taken");
    let refusal = module_host
        .convert_surface_into(
            &imported,
            &output_texture,
            &ModuleHostColorInfo {
                range: Some("tv"),
                ..ModuleHostColorInfo::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "surface_convert_into: \"tv\" is not a range name"
    );
    drop((output_texture, imported));

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "import fd=9 64x16 nv12_full_range planes=2 offsets=[0, 2304] strides=[128, 128] \
             size=3456"
                .to_string(),
            "acquire video depth=3 64x16 rgba8_unorm storage_binding,texture_binding".to_string(),
            "convert 3 -> 4 matrix=\"bt709\" range=\"\"".to_string(),
            "release 4".to_string(),
            "release 3".to_string(),
        ]
    );
    assert_eq!(node_host.references.load(Ordering::SeqCst), 1);
}

#[test]
fn an_import_of_more_planes_than_the_table_carries_is_refused_before_the_host() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .import_dma_buf(&ModuleHostForeignDmaBuf {
            fd: 9,
            width: 64,
            height: 16,
            pixel_format: "nv12_full_range",
            allocation_byte_size: 3_456,
            plane_offsets_bytes: &[0, 1, 2, 3, 4],
            plane_strides_bytes: &[128; 5],
            drm_format_modifier: 0,
        })
        .unwrap_err();
    assert!(refusal.to_string().contains("1 to 4 planes"), "{refusal}");
    assert!(node_host.surface_calls.lock().unwrap().is_empty());
}

#[test]
fn a_host_memory_write_carries_its_geometry_and_refuses_a_buffer_short_of_it() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let output_texture = module_host
        .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
            pool_key: "video",
            rotation_depth: 3,
            width: 4,
            height: 2,
            texture_format: "rgba8_unorm",
            usage: &["texture_binding"],
        })
        .expect("the acquire is taken");
    let painted = [7u8; 40];

    module_host
        .write_surface_from_host_memory(
            &output_texture,
            &ModuleHostHostMemoryPixels {
                pixels: &painted,
                pixel_format: "bgra32",
                width: 4,
                height: 2,
                row_stride_bytes: 24,
            },
        )
        .expect("the write is taken");
    let refusal = module_host
        .write_surface_from_host_memory(
            &output_texture,
            &ModuleHostHostMemoryPixels {
                pixels: &painted[..39],
                pixel_format: "bgra32",
                width: 4,
                height: 2,
                row_stride_bytes: 24,
            },
        )
        .unwrap_err();
    assert!(refusal.to_string().contains("40 bytes"), "{refusal}");
    drop(output_texture);

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "acquire video depth=3 4x2 rgba8_unorm texture_binding".to_string(),
            "write 4 4x2 bgra32 stride=24 bytes=40 first=7".to_string(),
            "release 4".to_string(),
        ]
    );
}

#[test]
fn a_table_smaller_than_its_minor_states_is_refused() {
    let node_host = stood_up_node_host();
    let mut table = v1_8_table_over(node_host);
    table.table_size_bytes = 16;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert!(matches!(
        refusal,
        ModuleHostError::ModuleHostTableSmallerThanItsMinorStates {
            table_size_bytes: 16,
            ..
        }
    ));
}

#[test]
fn a_null_table_is_refused() {
    let refusal =
        unsafe { ModuleHost::from_module_host_table_pointer(std::ptr::null()) }.unwrap_err();
    assert_eq!(refusal, ModuleHostError::NoModuleHostTable);
}

#[test]
fn every_handle_on_the_node_host_holds_one_reference_until_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);

    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let output_port = module_host.open_output_port("lent").unwrap();
    let clone = module_host.clone();
    assert_eq!(node_host.references.load(Ordering::SeqCst), 3);

    drop((module_host, output_port, clone));
    assert_eq!(node_host.references.load(Ordering::SeqCst), 0);
}

#[test]
fn a_refused_verb_carries_the_hosts_message_and_the_verb_it_refused() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host.open_output_port("never lent").unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "output_port_open: port \"never lent\" was not lent"
    );

    let entry_refusal = module_host
        .open_output_port("lent")
        .unwrap()
        .set_catalog_entry("video", "{}")
        .unwrap_err();
    assert_eq!(
        entry_refusal.to_string(),
        "output_port_set_catalog_entry: the entry does not parse"
    );
}

#[test]
fn a_read_message_is_handed_back_to_the_host_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let input_port = module_host.open_input_port("lent").unwrap();

    let message = input_port.read().unwrap().expect("one message waits");
    assert_eq!(message.payload(), &[1, 2, 3]);
    assert_eq!(message.timestamp_ns(), 42);
    assert!(message.starts_group());
    assert_eq!(message.lost_since_last_read(), 3);
    assert_eq!(node_host.released_reads.load(Ordering::SeqCst), 0);
    drop(message);
    assert_eq!(node_host.released_reads.load(Ordering::SeqCst), 1);

    assert!(input_port.read().unwrap().is_none());
}

#[test]
fn the_clock_stop_flag_wait_log_and_capabilities_reach_the_host() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    assert_eq!(module_host.monotonic_now_ns(), 1_000);
    assert!(!module_host.stop_requested());
    node_host.stop_requested.store(1, Ordering::SeqCst);
    assert!(module_host.stop_requested());
    assert!(
        !module_host
            .wait_for_any_input_port(std::time::Duration::from_millis(1))
            .unwrap()
    );
    assert_eq!(
        module_host.host_capabilities_json(),
        "{\"platform\":\"stood-up\"}"
    );

    module_host.log(ModuleHostLogLevel::Warn, "my_module", "a record");
    assert_eq!(
        *node_host.log_records.lock().unwrap(),
        vec![(2, "my_module".to_string(), "a record".to_string())]
    );
}

#[cfg(feature = "tracing")]
#[test]
fn the_modules_own_tracing_records_reach_the_hosts_log_with_their_fields() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to(&module_host);
    tracing::info!(target: "tatolab_webrtc", attempt = 2, "reconnecting");
    tatolab_module_sdk::module_host_tracing::stop_forwarding_this_modules_tracing_records();
    tracing::info!(target: "tatolab_webrtc", "not forwarded");

    assert_eq!(
        *node_host.log_records.lock().unwrap(),
        vec![(
            3,
            "tatolab_webrtc".to_string(),
            "reconnecting attempt=2".to_string()
        )]
    );
}

/// A runtime lending 1.0 has no audio or surface verbs, so a module built
/// against 1.8 never reads past the end of its table.
#[test]
fn a_module_built_against_1_8_is_refused_on_a_runtime_lending_1_0() {
    let node_host = stood_up_node_host();
    let mut table = v1_8_table_over(node_host);
    table.abi_minor = 0;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "this module needs module host table 1.8, and this runtime lends 1.0 — update the runtime"
    );
}

/// A runtime lending 1.6 has no service leases, so a module built against
/// 1.8 never reads past the end of its table.
#[test]
fn a_module_built_against_1_8_is_refused_on_a_runtime_lending_1_7() {
    let node_host = stood_up_node_host();
    let mut table = v1_8_table_over(node_host);
    table.abi_minor = 7;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "this module needs module host table 1.8, and this runtime lends 1.7 — update the runtime"
    );
}

#[test]
fn a_capture_stream_carries_the_request_and_reports_the_format_it_opened_in() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest {
            device_id: Some("hw:1"),
            sample_rate: Some(48_000),
            channels: None,
            sample_format: Some(ModuleHostAudioSampleFormat::F32),
        })
        .expect("the stood-up host opens every device but `unplugged`");

    assert_eq!(
        capture_stream.stream_format(),
        ModuleHostAudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: ModuleHostAudioSampleFormat::I16,
        }
    );
    let audio_capture = node_host.audio_capture.lock().unwrap();
    assert_eq!(audio_capture.requested_device_id.as_deref(), Some("hw:1"));
    assert_eq!(
        audio_capture.requested_format,
        Some((48_000, 0, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32))
    );
}

#[test]
fn a_capture_request_naming_nothing_asks_for_the_default_device_in_its_own_format() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let _capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
        .unwrap();

    let audio_capture = node_host.audio_capture.lock().unwrap();
    assert_eq!(audio_capture.requested_device_id.as_deref(), Some(""));
    assert_eq!(
        audio_capture.requested_format,
        Some((0, 0, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY))
    );
}

#[test]
fn a_capture_stream_the_host_refuses_carries_its_message() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest {
            device_id: Some("unplugged"),
            ..ModuleHostAudioCaptureStreamRequest::default()
        })
        .unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "audio_capture_stream_open: audio device 'unplugged' is not attached"
    );
}

#[test]
fn captured_blocks_reach_the_hand_off_until_delivery_stops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let mut capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
        .unwrap();
    let handed_off = std::sync::Arc::new(Mutex::new(Vec::new()));
    let handed_off_in_the_hand_off = std::sync::Arc::clone(&handed_off);

    capture_stream
        .start_delivering_to(Box::new(move |captured_block| {
            handed_off_in_the_hand_off.lock().unwrap().push((
                captured_block.interleaved_sample_bytes.to_vec(),
                captured_block.sample_count,
                captured_block.first_sample_timestamp_ns,
            ));
        }))
        .unwrap();
    assert!(deliver_one_block_through_the_installed_callback(
        node_host,
        &[1, 2, 3, 4],
        1,
        77
    ));
    capture_stream.stop_delivering().unwrap();

    assert!(!deliver_one_block_through_the_installed_callback(
        node_host,
        &[5, 6, 7, 8],
        1,
        78
    ));
    assert_eq!(*handed_off.lock().unwrap(), vec![(vec![1, 2, 3, 4], 1, 77)]);
}

#[test]
fn a_capture_stream_reports_the_failure_that_ended_it() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
        .unwrap();
    assert_eq!(
        capture_stream.failure_that_ended_the_stream().unwrap(),
        None
    );

    node_host.audio_capture.lock().unwrap().ended_with = Some("the device was unplugged".into());

    assert_eq!(
        capture_stream.failure_that_ended_the_stream().unwrap(),
        Some("the device was unplugged".to_string())
    );
}

#[test]
fn a_dropped_capture_stream_is_closed_and_releases_its_reference() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let mut capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
        .unwrap();
    capture_stream
        .start_delivering_to(Box::new(|_captured_block| {}))
        .unwrap();
    assert_eq!(node_host.references.load(Ordering::SeqCst), 2);

    drop(capture_stream);

    let audio_capture = node_host.audio_capture.lock().unwrap();
    assert!(audio_capture.closed);
    assert!(audio_capture.installed_callback.is_none());
    drop(audio_capture);
    assert_eq!(node_host.references.load(Ordering::SeqCst), 1);
}

#[test]
fn a_playback_stream_carries_the_request_and_reports_the_format_and_period_it_opened_with() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest {
            device_id: Some("hw:1"),
            sample_rate: Some(48_000),
            channels: Some(2),
            sample_format: Some(ModuleHostAudioSampleFormat::I16),
        })
        .expect("the stood-up host opens every device but `unplugged`");

    assert_eq!(
        playback_stream.stream_format(),
        ModuleHostAudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: ModuleHostAudioSampleFormat::I16,
        }
    );
    assert_eq!(
        playback_stream.device_period_in_per_channel_samples(),
        Some(256)
    );
    let audio_playback = node_host.audio_playback.lock().unwrap();
    assert_eq!(audio_playback.requested_device_id.as_deref(), Some("hw:1"));
    assert_eq!(
        audio_playback.requested_format,
        Some((48_000, 2, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16))
    );
}

#[test]
fn a_playback_stream_whose_host_reports_no_period_reports_none() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
        .unwrap();

    assert_eq!(playback_stream.device_period_in_per_channel_samples(), None);
    assert_eq!(
        node_host.audio_playback.lock().unwrap().requested_format,
        Some((0, 0, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY))
    );
}

#[test]
fn a_playback_stream_the_host_refuses_carries_its_message() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest {
            device_id: Some("unplugged"),
            ..ModuleHostAudioPlaybackStreamRequest::default()
        })
        .unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "audio_playback_stream_open: audio device 'unplugged' is not attached"
    );
}

#[test]
fn the_hand_off_fills_the_devices_own_buffer_until_requesting_stops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let mut playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
        .unwrap();
    let asked_sample_counts = std::sync::Arc::new(Mutex::new(Vec::new()));
    let asked_sample_counts_in_the_hand_off = std::sync::Arc::clone(&asked_sample_counts);

    playback_stream
        .start_requesting_from(Box::new(move |requested_block| {
            requested_block.interleaved_sample_bytes_to_fill.fill(0x11);
            asked_sample_counts_in_the_hand_off
                .lock()
                .unwrap()
                .push(requested_block.sample_count);
        }))
        .unwrap();
    assert_eq!(
        ask_for_one_block_through_the_installed_callback(node_host, 8, 2),
        Some(vec![0x11; 8]),
        "what the hand-off wrote is what the device's buffer holds"
    );
    playback_stream.stop_requesting().unwrap();

    assert_eq!(
        ask_for_one_block_through_the_installed_callback(node_host, 8, 2),
        None
    );
    assert_eq!(*asked_sample_counts.lock().unwrap(), vec![2]);
}

#[test]
fn a_playback_stream_reports_the_failure_that_ended_it() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
        .unwrap();
    let liveness = playback_stream.liveness();
    assert_eq!(liveness.failure_that_ended_the_stream().unwrap(), None);

    node_host.audio_playback.lock().unwrap().ended_with = Some("the sink went away".into());

    assert_eq!(
        liveness.failure_that_ended_the_stream().unwrap(),
        Some("the sink went away".to_string())
    );
}

#[test]
fn a_dropped_playback_stream_is_closed_and_releases_its_reference() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let mut playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
        .unwrap();
    playback_stream
        .start_requesting_from(Box::new(|_requested_block| {}))
        .unwrap();
    assert_eq!(node_host.references.load(Ordering::SeqCst), 2);

    drop(playback_stream);

    let audio_playback = node_host.audio_playback.lock().unwrap();
    assert!(audio_playback.closed);
    assert!(audio_playback.installed_callback.is_none());
    drop(audio_playback);
    assert_eq!(node_host.references.load(Ordering::SeqCst), 1);
}

#[test]
fn an_iosurface_import_carries_its_surface_and_is_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let stand_in_iosurface = std::ptr::without_provenance::<c_void>(0x1000);

    let imported = module_host
        .import_iosurface(&ModuleHostForeignIOSurface {
            iosurface: stand_in_iosurface,
            pixel_format: "nv12_video_range",
            width: 1280,
            height: 720,
        })
        .expect("the import is taken");
    assert_eq!(imported.surface_id(), "imported-5");
    drop(imported);

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "import iosurface=0x1000 1280x720 nv12_video_range".to_string(),
            "release 5".to_string(),
        ]
    );
}

#[test]
fn an_iosurface_import_of_a_null_surface_is_refused_before_the_host() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .import_iosurface(&ModuleHostForeignIOSurface {
            iosurface: std::ptr::null(),
            pixel_format: "nv12_video_range",
            width: 1280,
            height: 720,
        })
        .unwrap_err();
    assert!(
        refusal.to_string().contains("null IOSurfaceRef"),
        "{refusal}"
    );
    assert!(node_host.surface_calls.lock().unwrap().is_empty());
}

#[test]
fn a_surface_a_message_names_is_lent_by_its_id_and_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let lent = module_host
        .lend_surface_a_message_names("upstream-pool-slot#6")
        .expect("the lend is taken");
    assert_eq!(lent.surface_id(), "upstream-pool-slot#6");
    drop(lent);
    let refusal = module_host.lend_surface_a_message_names("").unwrap_err();
    assert!(
        refusal.to_string().contains("no surface id was named"),
        "{refusal}"
    );

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "lend upstream-pool-slot#6".to_string(),
            "release 6".to_string(),
        ]
    );
}

#[test]
fn a_lease_names_its_endpoint_and_tenant_reports_its_state_and_is_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let lease = module_host
        .lease_shared_native_service("browser-host")
        .expect("the host grants the lease");
    assert_eq!(
        lease.endpoint_socket_path(),
        std::path::Path::new(STOOD_UP_SERVICE_ENDPOINT)
    );
    assert_eq!(lease.tenant_stream_id(), STOOD_UP_SERVICE_TENANT);
    assert_eq!(
        lease.state().unwrap(),
        ModuleHostSharedNativeServiceState::StartingAgain
    );
    assert_eq!(
        lease.state().unwrap(),
        ModuleHostSharedNativeServiceState::Running
    );
    assert_eq!(
        lease.state().unwrap(),
        ModuleHostSharedNativeServiceState::Failed(
            "the browser host crashed 5 times in a row".to_string()
        )
    );
    drop(lease);

    assert_eq!(
        *node_host.lease_calls.lock().unwrap(),
        [
            "acquire browser-host",
            "state 7",
            "state 7",
            "state 7",
            "release 7"
        ]
    );
}

#[test]
fn a_lease_the_host_refuses_carries_its_message_and_releases_nothing() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .lease_shared_native_service("rive-host")
        .unwrap_err();

    assert!(
        refusal
            .to_string()
            .contains("this runtime offers no rive-host"),
        "{refusal}"
    );
    assert_eq!(
        *node_host.lease_calls.lock().unwrap(),
        ["acquire rive-host"]
    );
}

#[test]
fn a_node_fails_itself_with_the_reason_it_gives() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    module_host
        .fail_the_node_unrecoverably("the page's renderer ended 6 times in 60s")
        .expect("the host records the failure");

    assert_eq!(
        *node_host.node_failure_reasons.lock().unwrap(),
        ["the page's renderer ended 6 times in 60s"]
    );
}

#[test]
fn a_virtual_camera_offers_an_extent_writes_rows_and_is_closed_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let camera = module_host
        .open_virtual_camera_stream("desk cam")
        .expect("the camera opens");
    camera.offer_extent(4, 2, None).unwrap();
    let presentation = camera.write_yuyv_frame(&[0; 16], 8, 42).unwrap();
    assert_eq!(
        presentation,
        ModuleHostVirtualCameraFramePresentation::EveryBufferIsHeldByTheConsumer
    );
    assert_eq!(
        camera.failure_that_ended_the_stream().unwrap().as_deref(),
        Some("the PipeWire daemon went away")
    );
    drop(camera);

    assert_eq!(
        *node_host.virtual_camera_calls.lock().unwrap(),
        vec![
            "open desk cam".to_string(),
            "offer 9 4x2 fps=0".to_string(),
            "write 9 bytes=16 stride=8 at=42".to_string(),
            "close 9".to_string(),
        ]
    );
}

#[test]
fn a_virtual_camera_the_host_cannot_open_is_refused_in_its_words() {
    let node_host = stood_up_node_host();
    let table = v1_8_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host.open_virtual_camera_stream("").unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "virtual_camera_stream_open: no PipeWire daemon answers"
    );
    assert!(node_host.virtual_camera_calls.lock().unwrap().is_empty());
}
