// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The stood-up host's audio capture and playback streams and its virtual
//! camera, and the device thread's calls into a module's callbacks.

use super::*;

pub(super) unsafe extern "C" fn audio_capture_stream_open(
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

pub(super) unsafe extern "C" fn audio_capture_stream_start(
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

pub(super) unsafe extern "C" fn audio_capture_stream_stop(
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

pub(super) unsafe extern "C" fn audio_capture_stream_liveness(
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

pub(super) unsafe extern "C" fn audio_capture_stream_close(
    node_host: *mut c_void,
    _stream: ModuleHostTableAudioStreamHandle,
) {
    let mut audio_capture = node_host_of(node_host).audio_capture.lock().unwrap();
    audio_capture.installed_callback = None;
    audio_capture.closed = true;
}

pub(super) unsafe extern "C" fn audio_playback_stream_open(
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

pub(super) unsafe extern "C" fn audio_playback_stream_start(
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

pub(super) unsafe extern "C" fn audio_playback_stream_stop(
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

pub(super) unsafe extern "C" fn audio_playback_stream_liveness(
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

pub(super) unsafe extern "C" fn audio_playback_stream_close(
    node_host: *mut c_void,
    _stream: ModuleHostTableAudioStreamHandle,
) {
    let mut audio_playback = node_host_of(node_host).audio_playback.lock().unwrap();
    audio_playback.installed_callback = None;
    audio_playback.closed = true;
}

/// Ask for one device buffer of `byte_count` bytes the way the host's device
/// thread does, through whatever callback is installed; `None` when none is.
pub(super) fn ask_for_one_block_through_the_installed_callback(
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
pub(super) fn deliver_one_block_through_the_installed_callback(
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

pub(super) unsafe extern "C" fn virtual_camera_stream_open_refusing_an_empty_name(
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

pub(super) unsafe extern "C" fn virtual_camera_stream_offer_extent(
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

pub(super) unsafe extern "C" fn virtual_camera_stream_write_yuyv_frame_held_by_the_consumer(
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

pub(super) unsafe extern "C" fn virtual_camera_stream_liveness_ended(
    _node_host: *mut c_void,
    _camera: ModuleHostTableVirtualCameraStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    unsafe { (*error_message).write("the PipeWire daemon went away") };
    MODULE_HOST_TABLE_STATUS_ENDED
}

pub(super) unsafe extern "C" fn virtual_camera_stream_close(
    node_host: *mut c_void,
    camera: ModuleHostTableVirtualCameraStreamHandle,
) {
    node_host_of(node_host)
        .virtual_camera_calls
        .lock()
        .unwrap()
        .push(format!("close {}", camera.value));
}
