// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Safe Rust over the table's audio capture verbs: a device stream the
//! engine's audio backend opens for the module, delivering blocks to a
//! closure on the backend's own device thread.

use std::ffi::c_void;

use crate::module_host::{ModuleHost, ModuleHostError, refuse_unless_ok};
use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32,
    MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16, MODULE_HOST_TABLE_STATUS_ENDED,
    ModuleHostTableAudioCaptureStreamRequest, ModuleHostTableAudioStreamFormat,
    ModuleHostTableAudioStreamHandle, ModuleHostTableCapturedAudioBlock,
    ModuleHostTableErrorMessage, ModuleHostTableUtf8,
};

/// How the scalars an audio stream carries are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleHostAudioSampleFormat {
    /// 32-bit little-endian float.
    F32,
    /// 16-bit little-endian signed integer.
    I16,
}

impl ModuleHostAudioSampleFormat {
    /// Bytes one scalar occupies.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::I16 => 2,
        }
    }

    pub(crate) fn as_module_host_table_sample_format(self) -> u32 {
        match self {
            Self::F32 => MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32,
            Self::I16 => MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16,
        }
    }

    pub(crate) fn of_module_host_table_sample_format(sample_format: u32) -> Option<Self> {
        match sample_format {
            MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32 => Some(Self::F32),
            MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16 => Some(Self::I16),
            _ => None,
        }
    }
}

/// The rate, channel count and scalar encoding an opened stream delivers in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostAudioStreamFormat {
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Channel count every block is interleaved by.
    pub channels: u32,
    /// How the scalars in a block are encoded.
    pub sample_format: ModuleHostAudioSampleFormat,
}

/// What to open a capture stream on; each `None` takes the device's own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModuleHostAudioCaptureStreamRequest<'device_id> {
    /// The backend-named device; `None` takes the backend's default.
    pub device_id: Option<&'device_id str>,
    /// The sample rate the stream must open at.
    pub sample_rate: Option<u32>,
    /// The channel count the stream must open with.
    pub channels: Option<u32>,
    /// The scalar encoding the stream must open with.
    pub sample_format: Option<ModuleHostAudioSampleFormat>,
}

/// One block a capture device delivered, borrowed for the length of the
/// hand-off; a hand-off that keeps the samples copies them.
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostCapturedAudioBlock<'samples> {
    /// Interleaved little-endian scalars in the stream's format.
    pub interleaved_sample_bytes: &'samples [u8],
    /// Per-channel samples in the block.
    pub sample_count: u32,
    /// The monotonic instant of the block's first sample as the device timed it.
    pub first_sample_timestamp_ns: i64,
}

/// What a capture stream calls with each block, on the host's device thread:
/// it must not block.
pub type ModuleHostCapturedAudioBlockHandOff =
    Box<dyn Fn(ModuleHostCapturedAudioBlock<'_>) + Send + Sync>;

/// A capture stream the engine's audio backend opened for this module; closed
/// when it drops.
pub struct ModuleHostAudioCaptureStream {
    module_host: ModuleHost,
    handle: ModuleHostTableAudioStreamHandle,
    stream_format: ModuleHostAudioStreamFormat,
    /// Boxed twice so the pointer the host holds stays put while the hand-off
    /// is installed.
    installed_hand_off: Option<Box<ModuleHostCapturedAudioBlockHandOff>>,
}

impl std::fmt::Debug for ModuleHostAudioCaptureStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleHostAudioCaptureStream")
            .field("handle", &self.handle)
            .field("stream_format", &self.stream_format)
            .field("delivering", &self.installed_hand_off.is_some())
            .finish()
    }
}

impl ModuleHost {
    /// Open a capture stream through the engine's audio device backend, under
    /// the stream's capture-prompting policy.
    pub fn open_audio_capture_stream(
        &self,
        request: &ModuleHostAudioCaptureStreamRequest<'_>,
    ) -> Result<ModuleHostAudioCaptureStream, ModuleHostError> {
        let table = &self.module_host_table;
        let table_request = ModuleHostTableAudioCaptureStreamRequest {
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
        let mut opened_format = ModuleHostTableAudioStreamFormat {
            sample_rate: 0,
            channels: 0,
            sample_format: MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY,
        };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self`, and every pointer is to
        // a live local for the length of the call.
        let status = unsafe {
            (table.audio_capture_stream_open)(
                table.node_host,
                &table_request,
                &mut handle,
                &mut opened_format,
                &mut error_message,
            )
        };
        refuse_unless_ok("audio_capture_stream_open", status, &error_message)?;
        let Some(sample_format) = ModuleHostAudioSampleFormat::of_module_host_table_sample_format(
            opened_format.sample_format,
        ) else {
            // SAFETY: closes the stream just opened, once.
            unsafe { (table.audio_capture_stream_close)(table.node_host, handle) };
            return Err(ModuleHostError::VerbRefused {
                verb: "audio_capture_stream_open",
                refusal: format!(
                    "the host opened a stream in sample format {}, which this module does \
                     not know",
                    opened_format.sample_format
                ),
            });
        };
        Ok(ModuleHostAudioCaptureStream {
            module_host: self.clone(),
            handle,
            stream_format: ModuleHostAudioStreamFormat {
                sample_rate: opened_format.sample_rate,
                channels: opened_format.channels,
                sample_format,
            },
            installed_hand_off: None,
        })
    }
}

impl ModuleHostAudioCaptureStream {
    /// The format every block this stream delivers is in.
    pub fn stream_format(&self) -> ModuleHostAudioStreamFormat {
        self.stream_format
    }

    /// Begin handing every captured block to `hand_off`, replacing any hand-off
    /// an earlier call installed.
    pub fn start_delivering_to(
        &mut self,
        hand_off: ModuleHostCapturedAudioBlockHandOff,
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
            (table.audio_capture_stream_start)(
                table.node_host,
                self.handle,
                hand_a_captured_block_to_the_installed_hand_off,
                callback_context,
                &mut error_message,
            )
        };
        refuse_unless_ok("audio_capture_stream_start", status, &error_message)?;
        self.installed_hand_off = Some(hand_off);
        Ok(())
    }

    /// Stop delivering; the hand-off is not called again once this returns.
    pub fn stop_delivering(&mut self) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained; the buffer is live for the call.
        let status = unsafe {
            (table.audio_capture_stream_stop)(table.node_host, self.handle, &mut error_message)
        };
        refuse_unless_ok("audio_capture_stream_stop", status, &error_message)?;
        self.installed_hand_off = None;
        Ok(())
    }

    /// Why the device stopped serving this stream, or `None` while it serves it.
    pub fn failure_that_ended_the_stream(&self) -> Result<Option<String>, ModuleHostError> {
        self.liveness().failure_that_ended_the_stream()
    }

    /// The stream's liveness, readable from whatever thread does the module's
    /// work while this stream is held elsewhere.
    pub fn liveness(&self) -> ModuleHostAudioCaptureStreamLiveness {
        ModuleHostAudioCaptureStreamLiveness {
            module_host: self.module_host.clone(),
            handle: self.handle,
        }
    }
}

/// Whether a capture stream is still served by its device; once the stream is
/// closed it names nothing and every read is refused.
#[derive(Debug, Clone)]
pub struct ModuleHostAudioCaptureStreamLiveness {
    module_host: ModuleHost,
    handle: ModuleHostTableAudioStreamHandle,
}

impl ModuleHostAudioCaptureStreamLiveness {
    /// Why the device stopped serving the stream, or `None` while it serves it.
    pub fn failure_that_ended_the_stream(&self) -> Result<Option<String>, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained; the buffer is live for the call.
        let status = unsafe {
            (table.audio_capture_stream_liveness)(table.node_host, self.handle, &mut error_message)
        };
        if status == MODULE_HOST_TABLE_STATUS_ENDED {
            return Ok(Some(error_message.message()));
        }
        refuse_unless_ok("audio_capture_stream_liveness", status, &error_message)?;
        Ok(None)
    }
}

impl Drop for ModuleHostAudioCaptureStream {
    fn drop(&mut self) {
        let table = &self.module_host.module_host_table;
        // SAFETY: closes the stream this value opened, once; the host stops
        // calling the hand-off before it returns, so the hand-off drops after.
        unsafe { (table.audio_capture_stream_close)(table.node_host, self.handle) };
        self.installed_hand_off = None;
    }
}

unsafe extern "C" fn hand_a_captured_block_to_the_installed_hand_off(
    callback_context: *mut c_void,
    captured_block: *const ModuleHostTableCapturedAudioBlock,
) {
    // SAFETY: the context is the hand-off `start_delivering_to` installed, kept
    // alive while the host may call it; the block is live for this call.
    let (hand_off, captured_block) = unsafe {
        (
            &*callback_context
                .cast_const()
                .cast::<ModuleHostCapturedAudioBlockHandOff>(),
            &*captured_block,
        )
    };
    hand_off(ModuleHostCapturedAudioBlock {
        // SAFETY: the host hands bytes live for this call.
        interleaved_sample_bytes: unsafe { captured_block.interleaved_sample_bytes.as_slice() },
        sample_count: captured_block.sample_count,
        first_sample_timestamp_ns: captured_block.first_sample_timestamp_ns,
    });
}
