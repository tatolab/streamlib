// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio device seam: the one path anything opens an audio stream through.
//!
//! It sits beside the audio clock rather than inside a built-in, so audio
//! reaches hardware the way every other device class does — through a
//! handle-shaped primitive. The built-ins, the null backend and every test
//! open their streams here; there is no second audio device path.

use std::sync::{Arc, OnceLock};

use super::SharedAudioClock;
use super::silent_null_audio_device_backend::SilentNullAudioDeviceBackend;
use crate::core::Result;

/// How the scalars a capture stream delivers are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCaptureSampleFormat {
    /// 32-bit little-endian float.
    F32,
    /// 16-bit little-endian signed integer.
    I16,
}

impl AudioCaptureSampleFormat {
    /// Bytes one scalar occupies in a delivered payload.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            AudioCaptureSampleFormat::F32 => 4,
            AudioCaptureSampleFormat::I16 => 2,
        }
    }
}

/// What a capture stream delivers, fixed for the stream's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCaptureStreamFormat {
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Channel count every delivered payload is interleaved by.
    pub channels: u32,
    /// How to read the scalars in a delivered payload.
    pub sample_format: AudioCaptureSampleFormat,
}

impl AudioCaptureStreamFormat {
    /// Bytes a block of `sample_count` per-channel samples occupies once
    /// interleaved.
    pub fn interleaved_byte_count_for(self, sample_count: u32) -> usize {
        sample_count as usize * self.channels as usize * self.sample_format.bytes_per_sample()
    }
}

/// One block of samples as a device delivered it, borrowed for the length of
/// the hand-off — a callee that keeps the samples copies them.
#[derive(Debug, Clone, Copy)]
pub struct CapturedAudioBlockFromDevice<'a> {
    /// Interleaved little-endian scalars, read according to the stream's
    /// [`AudioCaptureStreamFormat::sample_format`].
    pub interleaved_sample_bytes: &'a [u8],
    /// Per-channel sample count: the payload carries `sample_count × channels`
    /// scalars.
    pub sample_count: u32,
    /// Monotonic timestamp of the block's first sample as the device timed it,
    /// never the instant of delivery — it is what makes joining audio to a
    /// camera frame a subtraction.
    pub first_sample_timestamp_ns: i64,
}

/// What a capture stream calls with each block it captures.
///
/// Runs on the backend's own callback thread, so it must not block, and it
/// must not re-enter the stream it was installed on: a backend is free to hold
/// the stream's own lock across this call, so calling back into
/// [`AudioCaptureStream::stop_delivering`] from here deadlocks it.
pub type CapturedAudioBlockHandOff = Box<dyn Fn(CapturedAudioBlockFromDevice<'_>) + Send + Sync>;

/// What a caller asks a backend to open a capture stream for.
pub struct AudioCaptureStreamRequest {
    /// Backend-named device. `None` takes the backend's default; a name the
    /// backend cannot open is an error, never a quiet landing on a different
    /// device.
    pub device_id: Option<String>,
    /// The clock a backend with no device paces its blocks from. A backend
    /// whose device provides the cadence ignores it, so device ticks and timer
    /// ticks never interleave.
    pub deviceless_pacing_clock: SharedAudioClock,
}

/// A capture stream a backend opened.
pub trait AudioCaptureStream: Send {
    /// The rate, channel count and scalar encoding of every block this stream
    /// delivers.
    fn stream_format(&self) -> AudioCaptureStreamFormat;

    /// Begin delivering captured blocks to `hand_off`, replacing any hand-off
    /// an earlier call installed.
    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()>;

    /// Stop delivering. The hand-off is not called again once this returns.
    fn stop_delivering(&mut self) -> Result<()>;
}

/// The audio device seam every audio stream is opened through.
pub trait AudioDeviceBackend: Send + Sync {
    /// The arm's name, for the one probe log line and for error text.
    fn backend_name(&self) -> &'static str;

    /// Open a capture stream against the named device, or the backend's
    /// default when none is named.
    fn open_capture_stream(
        &self,
        request: &AudioCaptureStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>>;
}

/// Shared handle to the backend the chain probed.
pub type SharedAudioDeviceBackend = Arc<dyn AudioDeviceBackend>;

static PROBED_AUDIO_DEVICE_BACKEND: OnceLock<SharedAudioDeviceBackend> = OnceLock::new();

/// Probe the audio backend chain once per process and log the arm it chose.
///
/// No configuration dial selects an arm and no environment variable overrides
/// the probe. The last arm needs no audio library at all, so the chain always
/// resolves to something and a machine with no audio runs the graph rather than
/// failing to start.
pub fn probe_audio_device_backend() -> SharedAudioDeviceBackend {
    Arc::clone(PROBED_AUDIO_DEVICE_BACKEND.get_or_init(|| {
        let backend = first_audio_device_backend_arm_that_opens();
        tracing::info!(
            audio_backend = backend.backend_name(),
            "audio device backend chain probed"
        );
        backend
    }))
}

/// Walk the chain in order and take the first arm that actually opens.
///
/// An arm is chosen by opening, not by loading: a library that resolves but
/// yields no usable connection — `libpipewire` present with no daemon
/// answering, the ordinary container case — demotes exactly as a missing
/// library does. Probing on presence alone would strand precisely the machines
/// the chain exists to serve.
fn first_audio_device_backend_arm_that_opens() -> SharedAudioDeviceBackend {
    if let Some(backend) = platform_audio_device_backend_that_opens() {
        return backend;
    }
    Arc::new(SilentNullAudioDeviceBackend)
}

#[cfg(target_os = "linux")]
fn platform_audio_device_backend_that_opens() -> Option<SharedAudioDeviceBackend> {
    use crate::linux::pipewire_audio_device_backend::PipeWireAudioDeviceBackend;

    match PipeWireAudioDeviceBackend::load_and_connect() {
        Ok(backend) => Some(Arc::new(backend)),
        Err(reason) => {
            // Info rather than warn: a machine with no audio server is a
            // supported environment, and the next arm is the answer rather
            // than the consolation prize. The reason is what tells a reader
            // whether the library was absent or the daemon was.
            tracing::info!(
                audio_backend = "pipewire",
                %reason,
                "audio device backend chain: demoting to the next arm"
            );
            None
        }
    }
}

/// The platform floor is Linux; every other target lands on the null backend,
/// which needs no audio library and captures silence.
#[cfg(not(target_os = "linux"))]
fn platform_audio_device_backend_that_opens() -> Option<SharedAudioDeviceBackend> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chain_is_probed_once_and_hands_back_the_same_backend_every_time() {
        let first = probe_audio_device_backend();
        let second = probe_audio_device_backend();
        assert!(
            Arc::ptr_eq(&first, &second),
            "the chain is probed once per process, so every caller shares one backend"
        );
    }

    /// The chain resolves on any machine, which is what lets the wheel import
    /// and run in `manylinux_2_28` and in a headless container — neither of
    /// which carries an audio library at all.
    #[test]
    fn the_chain_always_lands_on_an_arm_however_little_audio_the_machine_has() {
        let backend = probe_audio_device_backend();
        assert!(
            ["pipewire", "silent-null"].contains(&backend.backend_name()),
            "the chain resolved to an arm nothing declares: {}",
            backend.backend_name()
        );
    }

    #[test]
    fn a_scalars_width_is_the_width_the_wire_dtype_names() {
        assert_eq!(AudioCaptureSampleFormat::F32.bytes_per_sample(), 4);
        assert_eq!(AudioCaptureSampleFormat::I16.bytes_per_sample(), 2);
    }
}
