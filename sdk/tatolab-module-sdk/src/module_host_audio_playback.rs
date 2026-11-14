// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Safe Rust over the table's audio playback verbs: a device stream the
//! engine's audio backend opens for the module in the format it asks for,
//! asking a closure for samples on the backend's own device thread.

use std::ffi::c_void;

use crate::module_host::{ModuleHost, ModuleHostError, refuse_unless_ok};
use crate::module_host_audio_capture::{ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat};
use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY, MODULE_HOST_TABLE_STATUS_ENDED,
    ModuleHostTableAudioBlockRequestedByDevice, ModuleHostTableAudioPlaybackStreamRequest,
    ModuleHostTableAudioStreamFormat, ModuleHostTableAudioStreamHandle,
    ModuleHostTableErrorMessage, ModuleHostTableOpenedAudioPlaybackStream, ModuleHostTableUtf8,
};

/// What to open a playback stream on; each `None` format field takes the
/// device's own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModuleHostAudioPlaybackStreamRequest<'device_id> {
    /// The backend-named device; `None` takes the backend's default.
    pub device_id: Option<&'device_id str>,
    /// The sample rate every block handed to the stream is at.
    pub sample_rate: Option<u32>,
    /// The channel count every block handed to the stream is interleaved by.
    pub channels: Option<u32>,
    /// The scalar encoding every block handed to the stream is in.
    pub sample_format: Option<ModuleHostAudioSampleFormat>,
}

/// One block a playback device asks for, borrowed for the length of the
/// hand-off: the buffer is the device's.
#[derive(Debug)]
pub struct ModuleHostAudioBlockRequestedByDevice<'samples> {
    /// Interleaved little-endian scalars in the stream's format, to be filled
    /// whole; what is left unwritten plays as whatever was there.
    pub interleaved_sample_bytes_to_fill: &'samples mut [u8],
    /// Per-channel samples the buffer has room for.
    pub sample_count: u32,
}

/// What a playback stream calls each time its device needs samples, on the
/// host's device thread: it must not block.
pub type ModuleHostAudioBlockForPlaybackHandOff =
    Box<dyn Fn(ModuleHostAudioBlockRequestedByDevice<'_>) + Send + Sync>;

/// A playback stream the engine's audio backend opened for this module; closed
/// when it drops.
pub struct ModuleHostAudioPlaybackStream {
    module_host: ModuleHost,
    handle: ModuleHostTableAudioStreamHandle,
    stream_format: ModuleHostAudioStreamFormat,
    device_period_in_per_channel_samples: Option<u32>,
    /// Boxed twice so the pointer the host holds stays put while the hand-off
    /// is installed.
    installed_hand_off: Option<Box<ModuleHostAudioBlockForPlaybackHandOff>>,
}

impl std::fmt::Debug for ModuleHostAudioPlaybackStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleHostAudioPlaybackStream")
            .field("handle", &self.handle)
            .field("stream_format", &self.stream_format)
            .field(
                "device_period_in_per_channel_samples",
                &self.device_period_in_per_channel_samples,
            )
            .field("requesting", &self.installed_hand_off.is_some())
            .finish()
    }
}

impl ModuleHost {
    /// Open a playback stream through the engine's audio device backend in the
    /// requested format, which the backend converts to the device's.
    ///
    /// Refused on a runtime lending a table older than minor 3.
    pub fn open_audio_playback_stream(
        &self,
        request: &ModuleHostAudioPlaybackStreamRequest<'_>,
    ) -> Result<ModuleHostAudioPlaybackStream, ModuleHostError> {
        let table = &self.module_host_table;
        let table_request = ModuleHostTableAudioPlaybackStreamRequest {
            device_id: ModuleHostTableUtf8::of(request.device_id.unwrap_or("")),
            requested_format: ModuleHostTableAudioStreamFormat {
                sample_rate: request.sample_rate.unwrap_or(0),
                channels: request.channels.unwrap_or(0),
                sample_format: request.sample_format.map_or(
                    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY,
                    ModuleHostAudioSampleFormat::as_module_host_table_sample_format,
                ),
            },
            reserved: 0,
        };
        let mut handle = ModuleHostTableAudioStreamHandle { value: 0 };
        let mut opened_playback_stream = ModuleHostTableOpenedAudioPlaybackStream {
            stream_format: ModuleHostTableAudioStreamFormat {
                sample_rate: 0,
                channels: 0,
                sample_format: MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY,
            },
            device_period_in_per_channel_samples: 0,
        };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self`, and every pointer is to
        // a live local for the length of the call.
        let status = unsafe {
            (table.audio_playback_stream_open)(
                table.node_host,
                &table_request,
                &mut handle,
                &mut opened_playback_stream,
                &mut error_message,
            )
        };
        refuse_unless_ok("audio_playback_stream_open", status, &error_message)?;
        let opened_format = opened_playback_stream.stream_format;
        let Some(sample_format) = ModuleHostAudioSampleFormat::of_module_host_table_sample_format(
            opened_format.sample_format,
        ) else {
            // SAFETY: closes the stream just opened, once.
            unsafe { (table.audio_playback_stream_close)(table.node_host, handle) };
            return Err(ModuleHostError::VerbRefused {
                verb: "audio_playback_stream_open",
                refusal: format!(
                    "the host opened a stream in sample format {}, which this module does \
                     not know",
                    opened_format.sample_format
                ),
            });
        };
        Ok(ModuleHostAudioPlaybackStream {
            module_host: self.clone(),
            handle,
            stream_format: ModuleHostAudioStreamFormat {
                sample_rate: opened_format.sample_rate,
                channels: opened_format.channels,
                sample_format,
            },
            device_period_in_per_channel_samples: Some(
                opened_playback_stream.device_period_in_per_channel_samples,
            )
            .filter(|&period| period > 0),
            installed_hand_off: None,
        })
    }
}

impl ModuleHostAudioPlaybackStream {
    /// The format every block handed to this stream must be in.
    pub fn stream_format(&self) -> ModuleHostAudioStreamFormat {
        self.stream_format
    }

    /// Per-channel samples the device asks for in one cycle, or `None` where
    /// the backend does not report its device's period.
    pub fn device_period_in_per_channel_samples(&self) -> Option<u32> {
        self.device_period_in_per_channel_samples
    }

    /// Begin asking `hand_off` for samples, replacing any hand-off an earlier
    /// call installed.
    pub fn start_requesting_from(
        &mut self,
        hand_off: ModuleHostAudioBlockForPlaybackHandOff,
    ) -> Result<(), ModuleHostError> {
        let hand_off = Box::new(hand_off);
        let callback_context = std::ptr::from_ref(hand_off.as_ref())
            .cast_mut()
            .cast::<c_void>();
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained; the context points at a hand-off
        // this stream keeps alive until the host no longer calls it, which is
        // after a later start, a stop or the close returns.
        let status = unsafe {
            (table.audio_playback_stream_start)(
                table.node_host,
                self.handle,
                ask_the_installed_hand_off_to_fill_a_block,
                callback_context,
                &mut error_message,
            )
        };
        refuse_unless_ok("audio_playback_stream_start", status, &error_message)?;
        self.installed_hand_off = Some(hand_off);
        Ok(())
    }

    /// Stop asking; the hand-off is not called again once this returns.
    pub fn stop_requesting(&mut self) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained; the buffer is live for the call.
        let status = unsafe {
            (table.audio_playback_stream_stop)(table.node_host, self.handle, &mut error_message)
        };
        refuse_unless_ok("audio_playback_stream_stop", status, &error_message)?;
        self.installed_hand_off = None;
        Ok(())
    }

    /// Why the device stopped serving this stream, or `None` while it serves it.
    pub fn failure_that_ended_the_stream(&self) -> Result<Option<String>, ModuleHostError> {
        self.liveness().failure_that_ended_the_stream()
    }

    /// The stream's liveness, readable from whatever thread does the module's
    /// work while this stream is held elsewhere.
    pub fn liveness(&self) -> ModuleHostAudioPlaybackStreamLiveness {
        ModuleHostAudioPlaybackStreamLiveness {
            module_host: self.module_host.clone(),
            handle: self.handle,
        }
    }
}

/// Whether a playback stream is still served by its device; once the stream is
/// closed it names nothing and every read is refused.
#[derive(Debug, Clone)]
pub struct ModuleHostAudioPlaybackStreamLiveness {
    module_host: ModuleHost,
    handle: ModuleHostTableAudioStreamHandle,
}

impl ModuleHostAudioPlaybackStreamLiveness {
    /// Why the device stopped serving the stream, or `None` while it serves it.
    pub fn failure_that_ended_the_stream(&self) -> Result<Option<String>, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained; the buffer is live for the call.
        let status = unsafe {
            (table.audio_playback_stream_liveness)(table.node_host, self.handle, &mut error_message)
        };
        if status == MODULE_HOST_TABLE_STATUS_ENDED {
            return Ok(Some(error_message.message()));
        }
        refuse_unless_ok("audio_playback_stream_liveness", status, &error_message)?;
        Ok(None)
    }
}

impl Drop for ModuleHostAudioPlaybackStream {
    fn drop(&mut self) {
        let table = &self.module_host.module_host_table;
        // SAFETY: closes the stream this value opened, once; the host stops
        // calling the hand-off before it returns, so the hand-off drops after.
        unsafe { (table.audio_playback_stream_close)(table.node_host, self.handle) };
        self.installed_hand_off = None;
    }
}

unsafe extern "C" fn ask_the_installed_hand_off_to_fill_a_block(
    callback_context: *mut c_void,
    requested_block: *const ModuleHostTableAudioBlockRequestedByDevice,
) {
    // SAFETY: the context is the hand-off `start_requesting_from` installed,
    // kept alive while the host may call it; the block is live for this call.
    let (hand_off, requested_block) = unsafe {
        (
            &*callback_context
                .cast_const()
                .cast::<ModuleHostAudioBlockForPlaybackHandOff>(),
            &*requested_block,
        )
    };
    let interleaved_sample_bytes_to_fill: &mut [u8] =
        if requested_block.interleaved_sample_byte_count == 0 {
            &mut []
        } else {
            // SAFETY: the host hands a buffer of this length, writable and
            // borrowed by nothing else, for this call.
            unsafe {
                std::slice::from_raw_parts_mut(
                    requested_block.interleaved_sample_bytes_to_fill,
                    requested_block.interleaved_sample_byte_count,
                )
            }
        };
    hand_off(ModuleHostAudioBlockRequestedByDevice {
        interleaved_sample_bytes_to_fill,
        sample_count: requested_block.sample_count,
    });
}
