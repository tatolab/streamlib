// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The table's audio capture verbs: capture streams the engine's audio device
//! backend opens in this helper for one node's native code, delivering each
//! block to the module's callback on the backend's own device thread.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use streamlib::sdk::context::{
    AudioCaptureStream, AudioDeviceStreamRequest, AudioSampleFormat, AudioStreamFormat,
    CaptureDeviceAccessPrompting, CapturedAudioBlockFromDevice, DeviceStreamLivenessReport,
    RequestedAudioStreamFormat, SharedAudioClock, open_capture_stream_in_requested_format,
    platform_audio_clock_for_one_stream, probe_audio_device_backend,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32,
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16, MODULE_HOST_TABLE_STATUS_ENDED,
    MODULE_HOST_TABLE_STATUS_OK, ModuleHostTableAudioCaptureStreamRequest,
    ModuleHostTableAudioStreamFormat, ModuleHostTableAudioStreamHandle, ModuleHostTableBytes,
    ModuleHostTableCapturedAudioBlock, ModuleHostTableCapturedAudioBlockCallback,
    ModuleHostTableErrorMessage, ModuleHostTableStatus,
};

use super::{node_host_of, refuse};

/// The capture streams one node's native code opened, by handle.
pub(super) struct AudioCaptureStreamsOpenedForOneNode {
    /// Slot `n` is handle `n + 1`; a closed slot stays empty, so a stale handle
    /// names nothing rather than a later stream.
    opened_streams: Mutex<Vec<Option<Arc<AudioCaptureStreamOpenedForAModule>>>>,
    capture_device_access_prompting: CaptureDeviceAccessPrompting,
}

pub(super) struct AudioCaptureStreamOpenedForAModule {
    capture_stream: Mutex<Box<dyn AudioCaptureStream>>,
    liveness_report: DeviceStreamLivenessReport,
    /// Paces the null backend's silence; the stream's own, so closing it stops
    /// a clock nothing else ticks on.
    deviceless_pacing_clock: SharedAudioClock,
}

/// The module's callback and the context it asked to be called with.
#[derive(Clone, Copy)]
struct ModuleCapturedAudioBlockCallback {
    block_callback: ModuleHostTableCapturedAudioBlockCallback,
    callback_context: *mut c_void,
}

// SAFETY: the table's contract is that a module's block callback is called on
// the host's device thread with the context the module handed over.
unsafe impl Send for ModuleCapturedAudioBlockCallback {}
unsafe impl Sync for ModuleCapturedAudioBlockCallback {}

impl ModuleCapturedAudioBlockCallback {
    fn hand_over(&self, captured: CapturedAudioBlockFromDevice<'_>) {
        let captured_block = ModuleHostTableCapturedAudioBlock {
            interleaved_sample_bytes: ModuleHostTableBytes::of(captured.interleaved_sample_bytes),
            sample_count: captured.sample_count,
            reserved: 0,
            first_sample_timestamp_ns: captured.first_sample_timestamp_ns,
        };
        // SAFETY: the module installed this callback for this context, and
        // the block is live for the call.
        unsafe { (self.block_callback)(self.callback_context, &captured_block) };
    }
}

pub(super) fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AudioCaptureStreamsOpenedForOneNode {
    pub(super) fn under(capture_device_access_prompting: CaptureDeviceAccessPrompting) -> Self {
        Self {
            opened_streams: Mutex::new(Vec::new()),
            capture_device_access_prompting,
        }
    }

    pub(super) fn open(
        &self,
        node_id: &str,
        device_id: Option<String>,
        requested_stream_format: RequestedAudioStreamFormat,
    ) -> Result<(ModuleHostTableAudioStreamHandle, AudioStreamFormat), String> {
        let audio_device_backend = probe_audio_device_backend();
        let deviceless_pacing_clock = platform_audio_clock_for_one_stream();
        let capture_stream = open_capture_stream_in_requested_format(
            audio_device_backend.as_ref(),
            &AudioDeviceStreamRequest {
                device_id: device_id.clone(),
                deviceless_pacing_clock: Arc::clone(&deviceless_pacing_clock),
                capture_device_access_prompting: self.capture_device_access_prompting,
                requested_format: requested_stream_format,
            },
        )
        .map_err(|refusal| refusal.to_string())?;
        let stream_format = capture_stream.stream_format();
        tracing::info!(
            node_id,
            audio_backend = audio_device_backend.backend_name(),
            device_id = device_id.as_deref().unwrap_or("<default>"),
            sample_rate = stream_format.sample_rate,
            channels = stream_format.channels,
            sample_format = ?stream_format.sample_format,
            "a module opened an audio capture stream"
        );
        let opened_stream = Arc::new(AudioCaptureStreamOpenedForAModule {
            liveness_report: capture_stream.liveness_report(),
            capture_stream: Mutex::new(capture_stream),
            deviceless_pacing_clock,
        });
        let mut opened_streams = locked(&self.opened_streams);
        opened_streams.push(Some(opened_stream));
        Ok((
            ModuleHostTableAudioStreamHandle {
                value: opened_streams.len() as u64,
            },
            stream_format,
        ))
    }

    pub(super) fn opened_stream(
        &self,
        stream: ModuleHostTableAudioStreamHandle,
    ) -> Result<Arc<AudioCaptureStreamOpenedForAModule>, String> {
        usize::try_from(stream.value)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .and_then(|slot| locked(&self.opened_streams).get(slot).cloned().flatten())
            .ok_or_else(|| {
                format!(
                    "audio stream handle {} names no capture stream this table has open",
                    stream.value
                )
            })
    }

    pub(super) fn close(&self, stream: ModuleHostTableAudioStreamHandle) {
        let closed_stream = usize::try_from(stream.value)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .and_then(|slot| {
                locked(&self.opened_streams)
                    .get_mut(slot)
                    .and_then(Option::take)
            });
        if let Some(closed_stream) = closed_stream {
            closed_stream.stop_delivering_and_log_a_failure();
        }
    }
}

impl AudioCaptureStreamOpenedForAModule {
    fn stop_delivering_and_log_a_failure(&self) {
        if let Err(stop_failure) = locked(&self.capture_stream).stop_delivering() {
            tracing::warn!(error = %stop_failure, "a module's audio capture stream failed to stop");
        }
    }
}

impl Drop for AudioCaptureStreamOpenedForAModule {
    fn drop(&mut self) {
        self.stop_delivering_and_log_a_failure();
        if let Err(clock_stop_failure) = self.deviceless_pacing_clock.stop() {
            tracing::warn!(
                error = %clock_stop_failure,
                "a module's audio capture stream could not stop its pacing clock"
            );
        }
    }
}

pub(super) fn requested_audio_stream_format_of(
    requested_format: ModuleHostTableAudioStreamFormat,
) -> Result<RequestedAudioStreamFormat, String> {
    let sample_format = match requested_format.sample_format {
        MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY => None,
        MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32 => Some(AudioSampleFormat::F32),
        MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16 => Some(AudioSampleFormat::I16),
        unknown => {
            return Err(format!(
                "a requested sample format is 0 (any), 1 (f32) or 2 (i16), got {unknown}"
            ));
        }
    };
    Ok(RequestedAudioStreamFormat {
        sample_rate: (requested_format.sample_rate != 0).then_some(requested_format.sample_rate),
        channels: (requested_format.channels != 0).then_some(requested_format.channels),
        sample_format,
    })
}

pub(super) fn module_host_table_audio_stream_format_of(
    stream_format: AudioStreamFormat,
) -> ModuleHostTableAudioStreamFormat {
    ModuleHostTableAudioStreamFormat {
        sample_rate: stream_format.sample_rate,
        channels: stream_format.channels,
        sample_format: match stream_format.sample_format {
            AudioSampleFormat::F32 => MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32,
            AudioSampleFormat::I16 => MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16,
        },
    }
}

pub(super) unsafe extern "C" fn audio_capture_stream_open(
    node_host: *mut c_void,
    request: *const ModuleHostTableAudioCaptureStreamRequest,
    opened_stream: *mut ModuleHostTableAudioStreamHandle,
    opened_format: *mut ModuleHostTableAudioStreamFormat,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    // SAFETY: the SDK hands a request and slots live for the call, or null.
    let (Some(request), Some(opened_stream), Some(opened_format)) = (unsafe {
        (
            request.as_ref(),
            opened_stream.as_mut(),
            opened_format.as_mut(),
        )
    }) else {
        return unsafe {
            refuse(
                error_message,
                "an audio capture open is handed a request and slots for the stream and its format",
            )
        };
    };
    // SAFETY: the SDK hands a view live for the call.
    let device_id = match unsafe { request.device_id.as_str() } {
        Ok("") => None,
        Ok(device_id) => Some(device_id.to_string()),
        Err(not_utf8) => {
            return unsafe { refuse(error_message, &format!("a device id is UTF-8: {not_utf8}")) };
        }
    };
    let opened = requested_audio_stream_format_of(request.requested_format).and_then(
        |requested_stream_format| {
            node_host.audio_capture_streams.open(
                &node_host.node_id,
                device_id,
                requested_stream_format,
            )
        },
    );
    match opened {
        Ok((handle, stream_format)) => {
            *opened_stream = handle;
            *opened_format = module_host_table_audio_stream_format_of(stream_format);
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn audio_capture_stream_start(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
    block_callback: ModuleHostTableCapturedAudioBlockCallback,
    callback_context: *mut c_void,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let opened_stream = match node_host.audio_capture_streams.opened_stream(stream) {
        Ok(opened_stream) => opened_stream,
        Err(refusal) => return unsafe { refuse(error_message, &refusal) },
    };
    let module_callback = ModuleCapturedAudioBlockCallback {
        block_callback,
        callback_context,
    };
    let mut capture_stream = locked(&opened_stream.capture_stream);
    // Stopped first, so the callback being replaced is never called once this
    // returns, whatever a backend does with a hand-off it replaces.
    let started = capture_stream.stop_delivering().and_then(|()| {
        capture_stream.start_delivering_to(Box::new(move |captured| {
            module_callback.hand_over(captured);
        }))
    });
    match started {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(start_failure) => unsafe { refuse(error_message, &start_failure.to_string()) },
    }
}

pub(super) unsafe extern "C" fn audio_capture_stream_stop(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let stopped = node_host
        .audio_capture_streams
        .opened_stream(stream)
        .and_then(|opened_stream| {
            locked(&opened_stream.capture_stream)
                .stop_delivering()
                .map_err(|stop_failure| stop_failure.to_string())
        });
    match stopped {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn audio_capture_stream_liveness(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    match node_host.audio_capture_streams.opened_stream(stream) {
        Ok(opened_stream) => match opened_stream
            .liveness_report
            .failure_that_ended_the_stream()
        {
            None => MODULE_HOST_TABLE_STATUS_OK,
            Some(failure) => {
                // SAFETY: the SDK hands a buffer live for the call, or null.
                if let Some(error_message) = unsafe { error_message.as_mut() } {
                    error_message.write(&failure.to_string());
                }
                MODULE_HOST_TABLE_STATUS_ENDED
            }
        },
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn audio_capture_stream_close(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
) {
    let node_host = unsafe { node_host_of(node_host) };
    node_host.audio_capture_streams.close(stream);
}
