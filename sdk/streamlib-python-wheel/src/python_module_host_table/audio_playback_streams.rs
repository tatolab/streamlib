// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The table's audio playback verbs: playback streams the engine's audio
//! device backend opens in this helper for one node's native code, in the
//! format the module plays, asking the module's callback to fill each device
//! buffer on the backend's own device thread.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use streamlib::sdk::context::{
    AudioBlockRequestedByDevice, AudioDeviceStreamRequest, AudioPlaybackStream, AudioStreamFormat,
    CaptureDeviceAccessPrompting, DeviceStreamLivenessReport, RequestedAudioStreamFormat,
    SharedAudioClock, open_playback_stream_in_requested_format,
    platform_audio_clock_for_one_stream, probe_audio_device_backend,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_ENDED, MODULE_HOST_TABLE_STATUS_OK,
    ModuleHostTableAudioBlockRequestedByDevice, ModuleHostTableAudioBlockRequestedByDeviceCallback,
    ModuleHostTableAudioPlaybackStreamRequest, ModuleHostTableAudioStreamHandle,
    ModuleHostTableErrorMessage, ModuleHostTableOpenedAudioPlaybackStream, ModuleHostTableStatus,
};

use super::audio_capture_streams::{
    locked, module_host_table_audio_stream_format_of, requested_audio_stream_format_of,
};
use super::{node_host_of, refuse};

/// The playback streams one node's native code opened, by handle.
#[derive(Default)]
pub(super) struct AudioPlaybackStreamsOpenedForOneNode {
    /// Slot `n` is handle `n + 1`; a closed slot stays empty, so a stale handle
    /// names nothing rather than a later stream.
    opened_streams: Mutex<Vec<Option<Arc<AudioPlaybackStreamOpenedForAModule>>>>,
}

pub(super) struct AudioPlaybackStreamOpenedForAModule {
    playback_stream: Mutex<Box<dyn AudioPlaybackStream>>,
    liveness_report: DeviceStreamLivenessReport,
    /// Paces the null backend's requests; the stream's own, so closing it
    /// stops a clock nothing else ticks on.
    deviceless_pacing_clock: SharedAudioClock,
}

/// The module's callback and the context it asked to be called with.
#[derive(Clone, Copy)]
struct ModuleAudioBlockRequestedByDeviceCallback {
    fill_callback: ModuleHostTableAudioBlockRequestedByDeviceCallback,
    callback_context: *mut c_void,
}

// SAFETY: the table's contract is that a module's fill callback is called on
// the host's device thread with the context the module handed over.
unsafe impl Send for ModuleAudioBlockRequestedByDeviceCallback {}
unsafe impl Sync for ModuleAudioBlockRequestedByDeviceCallback {}

impl ModuleAudioBlockRequestedByDeviceCallback {
    fn ask_to_fill(&self, requested: AudioBlockRequestedByDevice<'_>) {
        let requested_block = ModuleHostTableAudioBlockRequestedByDevice {
            interleaved_sample_bytes_to_fill: requested
                .interleaved_sample_bytes_to_fill
                .as_mut_ptr(),
            interleaved_sample_byte_count: requested.interleaved_sample_bytes_to_fill.len(),
            sample_count: requested.sample_count,
            reserved: 0,
        };
        // SAFETY: the module installed this callback for this context, and
        // the device's buffer is live and borrowed by nothing else for the call.
        unsafe { (self.fill_callback)(self.callback_context, &requested_block) };
    }
}

impl AudioPlaybackStreamsOpenedForOneNode {
    pub(super) fn open(
        &self,
        node_id: &str,
        device_id: Option<String>,
        requested_stream_format: RequestedAudioStreamFormat,
    ) -> Result<
        (
            ModuleHostTableAudioStreamHandle,
            AudioStreamFormat,
            Option<u32>,
        ),
        String,
    > {
        let audio_device_backend = probe_audio_device_backend();
        let deviceless_pacing_clock = platform_audio_clock_for_one_stream();
        let playback_stream = open_playback_stream_in_requested_format(
            audio_device_backend.as_ref(),
            &AudioDeviceStreamRequest {
                device_id: device_id.clone(),
                deviceless_pacing_clock: Arc::clone(&deviceless_pacing_clock),
                capture_device_access_prompting:
                    CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
                requested_format: requested_stream_format,
            },
        )
        .map_err(|refusal| refusal.to_string())?;
        let stream_format = playback_stream.stream_format();
        let device_period_in_per_channel_samples =
            playback_stream.device_period_in_per_channel_samples();
        tracing::info!(
            node_id,
            audio_backend = audio_device_backend.backend_name(),
            device_id = device_id.as_deref().unwrap_or("<default>"),
            sample_rate = stream_format.sample_rate,
            channels = stream_format.channels,
            sample_format = ?stream_format.sample_format,
            device_period_in_per_channel_samples,
            "a module opened an audio playback stream"
        );
        let opened_stream = Arc::new(AudioPlaybackStreamOpenedForAModule {
            liveness_report: playback_stream.liveness_report(),
            playback_stream: Mutex::new(playback_stream),
            deviceless_pacing_clock,
        });
        let mut opened_streams = locked(&self.opened_streams);
        opened_streams.push(Some(opened_stream));
        Ok((
            ModuleHostTableAudioStreamHandle {
                value: opened_streams.len() as u64,
            },
            stream_format,
            device_period_in_per_channel_samples,
        ))
    }

    pub(super) fn opened_stream(
        &self,
        stream: ModuleHostTableAudioStreamHandle,
    ) -> Result<Arc<AudioPlaybackStreamOpenedForAModule>, String> {
        usize::try_from(stream.value)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .and_then(|slot| locked(&self.opened_streams).get(slot).cloned().flatten())
            .ok_or_else(|| {
                format!(
                    "audio stream handle {} names no playback stream this table has open",
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
            closed_stream.stop_requesting_and_log_a_failure();
        }
    }
}

impl AudioPlaybackStreamOpenedForAModule {
    fn stop_requesting_and_log_a_failure(&self) {
        if let Err(stop_failure) = locked(&self.playback_stream).stop_requesting() {
            tracing::warn!(error = %stop_failure, "a module's audio playback stream failed to stop");
        }
    }
}

impl Drop for AudioPlaybackStreamOpenedForAModule {
    fn drop(&mut self) {
        self.stop_requesting_and_log_a_failure();
        if let Err(clock_stop_failure) = self.deviceless_pacing_clock.stop() {
            tracing::warn!(
                error = %clock_stop_failure,
                "a module's audio playback stream could not stop its pacing clock"
            );
        }
    }
}

pub(super) unsafe extern "C" fn audio_playback_stream_open(
    node_host: *mut c_void,
    request: *const ModuleHostTableAudioPlaybackStreamRequest,
    opened_stream: *mut ModuleHostTableAudioStreamHandle,
    opened_playback_stream: *mut ModuleHostTableOpenedAudioPlaybackStream,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    // SAFETY: the SDK hands a request and slots live for the call, or null.
    let (Some(request), Some(opened_stream), Some(opened_playback_stream)) = (unsafe {
        (
            request.as_ref(),
            opened_stream.as_mut(),
            opened_playback_stream.as_mut(),
        )
    }) else {
        return unsafe {
            refuse(
                error_message,
                "an audio playback open is handed a request and slots for the stream and what it \
                 opened with",
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
            node_host.audio_playback_streams.open(
                &node_host.node_id,
                device_id,
                requested_stream_format,
            )
        },
    );
    match opened {
        Ok((handle, stream_format, device_period_in_per_channel_samples)) => {
            *opened_stream = handle;
            *opened_playback_stream = ModuleHostTableOpenedAudioPlaybackStream {
                stream_format: module_host_table_audio_stream_format_of(stream_format),
                device_period_in_per_channel_samples: device_period_in_per_channel_samples
                    .unwrap_or(0),
            };
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn audio_playback_stream_start(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
    fill_callback: ModuleHostTableAudioBlockRequestedByDeviceCallback,
    callback_context: *mut c_void,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let opened_stream = match node_host.audio_playback_streams.opened_stream(stream) {
        Ok(opened_stream) => opened_stream,
        Err(refusal) => return unsafe { refuse(error_message, &refusal) },
    };
    let module_callback = ModuleAudioBlockRequestedByDeviceCallback {
        fill_callback,
        callback_context,
    };
    let mut playback_stream = locked(&opened_stream.playback_stream);
    // Stopped first, so the callback being replaced is never called once this
    // returns, whatever a backend does with a hand-off it replaces.
    let started = playback_stream.stop_requesting().and_then(|()| {
        playback_stream.start_requesting_from(Box::new(move |requested| {
            module_callback.ask_to_fill(requested);
        }))
    });
    match started {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(start_failure) => unsafe { refuse(error_message, &start_failure.to_string()) },
    }
}

pub(super) unsafe extern "C" fn audio_playback_stream_stop(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let stopped = node_host
        .audio_playback_streams
        .opened_stream(stream)
        .and_then(|opened_stream| {
            locked(&opened_stream.playback_stream)
                .stop_requesting()
                .map_err(|stop_failure| stop_failure.to_string())
        });
    match stopped {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn audio_playback_stream_liveness(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    match node_host.audio_playback_streams.opened_stream(stream) {
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

pub(super) unsafe extern "C" fn audio_playback_stream_close(
    node_host: *mut c_void,
    stream: ModuleHostTableAudioStreamHandle,
) {
    let node_host = unsafe { node_host_of(node_host) };
    node_host.audio_playback_streams.close(stream);
}
