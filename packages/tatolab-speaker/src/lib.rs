// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab.speaker`: a playback stream the engine's audio
//! device backend opens in this node's helper process in the format the node
//! plays, a thread that reads each block off the node's lent `audio` port
//! through the module host table into a bounded ring, and the device callback
//! that fills each device buffer from it. Python never runs per block.

mod audio_block_draining;
mod audio_samples_awaiting_playback_ring;
mod channels_block_to_play;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use tatolab_module_sdk::{
    ModuleHost, ModuleHostAudioBlockRequestedByDevice, ModuleHostAudioPlaybackStream,
    ModuleHostAudioPlaybackStreamRequest, ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat,
    ModuleHostLogLevel,
};

use audio_block_draining::{AudioBlockDraining, SPEAKER_LOG_TARGET};
use audio_samples_awaiting_playback_ring::AudioSamplesAwaitingPlaybackRing;

/// The one port a block to play arrives on.
const AUDIO_INPUT_PORT: &str = "audio";

/// Device periods the ring holds before the drain thread waits for room: the
/// depth a sample-stream link carries, roughly 170 ms at 48 kHz and a
/// 512-sample period.
const DEVICE_PERIODS_THE_RING_HOLDS: usize = 16;

/// The smallest ring worth having, whatever a stream reports; a block larger
/// than the ring is queued in pieces, never refused.
const SMALLEST_USABLE_RING_BYTES: usize = 8 * 1024;

/// Device periods of silence between underrun warnings, roughly three seconds
/// at a ten-millisecond period.
const SILENT_PERIODS_BETWEEN_UNDERRUN_WARNINGS: u64 = 300;

/// Bound on the wait for the drain thread to exit, well inside the helper's
/// teardown budget; past it the thread is detached.
const DRAIN_THREAD_EXIT_GRACE: Duration = Duration::from_secs(2);

/// The ring a stream queues into and the silence between its underrun
/// warnings, both stated in one device period so they move together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpeakerSizingFromTheDevicePeriod {
    ring_byte_capacity: usize,
    silence_bytes_between_underrun_warnings: u64,
}

impl SpeakerSizingFromTheDevicePeriod {
    /// The period the stream reports, else ten milliseconds of its format,
    /// never below one sample.
    fn for_format_and_reported_period(
        stream_format: ModuleHostAudioStreamFormat,
        reported_device_period_in_per_channel_samples: Option<u32>,
    ) -> Self {
        let device_period_in_per_channel_samples = reported_device_period_in_per_channel_samples
            .filter(|&period| period > 0)
            .unwrap_or(stream_format.sample_rate / 100)
            .max(1);
        let device_period_in_bytes = device_period_in_per_channel_samples as usize
            * stream_format.channels as usize
            * stream_format.sample_format.bytes_per_sample();
        Self {
            ring_byte_capacity: (device_period_in_bytes * DEVICE_PERIODS_THE_RING_HOLDS)
                .max(SMALLEST_USABLE_RING_BYTES),
            silence_bytes_between_underrun_warnings: device_period_in_bytes.max(1) as u64
                * SILENT_PERIODS_BETWEEN_UNDERRUN_WARNINGS,
        }
    }
}

/// Playback in progress: the ring the device callback fills from and the
/// thread draining `audio` into it.
struct AudioBlockPlayback {
    samples_awaiting_playback: Arc<AudioSamplesAwaitingPlaybackRing>,
    is_draining: Arc<AtomicBool>,
    drain_thread: JoinHandle<()>,
    /// Behind a lock only so the node class can be shared across threads.
    drain_thread_finished: Mutex<Receiver<()>>,
}

/// Silence counted across every playback that has ended.
#[derive(Default)]
struct SilenceCountsOfEndedPlaybacks {
    underrun_byte_count: u64,
    silence_before_playback_began_byte_count: u64,
}

/// One node's speaker: a playback stream opened through the module host table,
/// fed from the node's lent `audio` port.
#[pyclass(name = "SpeakerPlayback", module = "tatolab.speaker._native")]
struct SpeakerPlayback {
    module_host: ModuleHost,
    playback_stream: ModuleHostAudioPlaybackStream,
    playback: Option<AudioBlockPlayback>,
    played_block_count: Arc<AtomicU64>,
    refused_block_count: Arc<AtomicU64>,
    silence_counts_of_ended_playbacks: SilenceCountsOfEndedPlaybacks,
}

fn sample_format_named(sample_format: &str) -> PyResult<ModuleHostAudioSampleFormat> {
    match sample_format {
        "f32" => Ok(ModuleHostAudioSampleFormat::F32),
        "i16" => Ok(ModuleHostAudioSampleFormat::I16),
        other => Err(PyValueError::new_err(format!(
            "SpeakerSink: a sample format is 'f32' or 'i16', got {other:?}"
        ))),
    }
}

#[pymethods]
impl SpeakerPlayback {
    /// Open the playback stream on `device_id`, or the backend's default, in
    /// the format every block on `audio` arrives in, for the node whose module
    /// host table this is; its `audio` port must be lent.
    #[new]
    #[pyo3(signature = (module_host_table, sample_rate, channels, sample_format, device_id = None))]
    fn new(
        python: Python<'_>,
        module_host_table: &Bound<'_, PyAny>,
        sample_rate: u32,
        channels: u32,
        sample_format: &str,
        device_id: Option<String>,
    ) -> PyResult<Self> {
        let sample_format = sample_format_named(sample_format)?;
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))?;
        let playback_stream = python
            .detach(|| {
                module_host.open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest {
                    device_id: device_id.as_deref(),
                    sample_rate: Some(sample_rate),
                    channels: Some(channels),
                    sample_format: Some(sample_format),
                })
            })
            .map_err(|refusal| PyRuntimeError::new_err(format!("SpeakerSink: {refusal}")))?;
        Ok(Self {
            module_host,
            playback_stream,
            playback: None,
            played_block_count: Arc::new(AtomicU64::new(0)),
            refused_block_count: Arc::new(AtomicU64::new(0)),
            silence_counts_of_ended_playbacks: SilenceCountsOfEndedPlaybacks::default(),
        })
    }

    /// The rate every block is played at, in hertz.
    #[getter]
    fn sample_rate(&self) -> u32 {
        self.playback_stream.stream_format().sample_rate
    }

    /// The channel count every block is interleaved by.
    #[getter]
    fn channels(&self) -> u32 {
        self.playback_stream.stream_format().channels
    }

    /// The scalar encoding every block carries: `f32` or `i16`.
    #[getter]
    fn sample_format(&self) -> &'static str {
        match self.playback_stream.stream_format().sample_format {
            ModuleHostAudioSampleFormat::F32 => "f32",
            ModuleHostAudioSampleFormat::I16 => "i16",
        }
    }

    /// Per-channel samples the device asks for in one cycle, or `None` where
    /// the backend does not report its device's period.
    #[getter]
    fn device_period_in_per_channel_samples(&self) -> Option<u32> {
        self.playback_stream.device_period_in_per_channel_samples()
    }

    /// Blocks queued for the device so far.
    #[getter]
    fn played_block_count(&self) -> u64 {
        self.played_block_count.load(Ordering::Relaxed)
    }

    /// Blocks on `audio` not played because they were not audio blocks in the
    /// stream's format.
    #[getter]
    fn refused_block_count(&self) -> u64 {
        self.refused_block_count.load(Ordering::Relaxed)
    }

    /// Bytes of silence the device was given because the graph fell behind
    /// after playback began.
    #[getter]
    fn underrun_byte_count(&self) -> u64 {
        self.silence_counts_of_ended_playbacks.underrun_byte_count
            + self.playback.as_ref().map_or(0, |playback| {
                playback.samples_awaiting_playback.underrun_byte_count()
            })
    }

    /// Bytes of silence the device was given while the first periods were still
    /// being queued: the cost of starting, not a fault.
    #[getter]
    fn silence_before_playback_began_byte_count(&self) -> u64 {
        self.silence_counts_of_ended_playbacks
            .silence_before_playback_began_byte_count
            + self.playback.as_ref().map_or(0, |playback| {
                playback
                    .samples_awaiting_playback
                    .silence_played_before_the_cushion_filled_byte_count()
            })
    }

    /// Why the device stopped serving the playback, or `None` while it serves it.
    #[getter]
    fn failure_that_ended_the_playback(&self) -> PyResult<Option<String>> {
        self.playback_stream
            .failure_that_ended_the_stream()
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
    }

    /// Begin draining `audio` and playing; a playback already started is left
    /// as it is.
    fn start(&mut self, python: Python<'_>) -> PyResult<()> {
        if self.playback.is_some() {
            return Ok(());
        }
        let audio_input_port = self
            .module_host
            .open_input_port(AUDIO_INPUT_PORT)
            .map_err(|refusal| PyRuntimeError::new_err(format!("SpeakerSink: {refusal}")))?;
        let playback_stream_format = self.playback_stream.stream_format();
        let sizing = SpeakerSizingFromTheDevicePeriod::for_format_and_reported_period(
            playback_stream_format,
            self.playback_stream.device_period_in_per_channel_samples(),
        );
        let samples_awaiting_playback = Arc::new(
            AudioSamplesAwaitingPlaybackRing::with_byte_capacity(sizing.ring_byte_capacity),
        );
        let is_draining = Arc::new(AtomicBool::new(true));
        let draining = AudioBlockDraining {
            module_host: self.module_host.clone(),
            audio_input_port,
            playback_stream_liveness: self.playback_stream.liveness(),
            playback_stream_format,
            samples_awaiting_playback: Arc::clone(&samples_awaiting_playback),
            silence_bytes_between_underrun_warnings: sizing.silence_bytes_between_underrun_warnings,
            is_draining: Arc::clone(&is_draining),
            played_block_count: Arc::clone(&self.played_block_count),
            refused_block_count: Arc::clone(&self.refused_block_count),
        };
        let (drain_thread_finished_sender, drain_thread_finished) = sync_channel(1);
        let drain_thread = std::thread::Builder::new()
            .name("speaker-drain".to_string())
            .spawn(move || {
                draining.drain_until_stopped();
                let _ = drain_thread_finished_sender.send(());
            })
            .map_err(|spawn_failure| {
                PyRuntimeError::new_err(format!(
                    "SpeakerSink: the drain thread could not start: {spawn_failure}"
                ))
            })?;
        let samples_awaiting_playback_in_the_device_callback =
            Arc::clone(&samples_awaiting_playback);
        let playback_stream = &mut self.playback_stream;
        let started = python.detach(|| {
            playback_stream.start_requesting_from(Box::new(
                move |requested_block: ModuleHostAudioBlockRequestedByDevice<'_>| {
                    samples_awaiting_playback_in_the_device_callback
                        .fill_one_device_period(requested_block.interleaved_sample_bytes_to_fill);
                },
            ))
        });
        self.playback = Some(AudioBlockPlayback {
            samples_awaiting_playback,
            is_draining,
            drain_thread,
            drain_thread_finished: Mutex::new(drain_thread_finished),
        });
        if let Err(refusal) = started {
            self.stop(python);
            return Err(PyRuntimeError::new_err(format!("SpeakerSink: {refusal}")));
        }
        Ok(())
    }

    /// Stop playing and end the drain thread within its grace.
    fn stop(&mut self, python: Python<'_>) {
        python.detach(|| self.stop_playing_and_draining());
    }
}

impl SpeakerPlayback {
    fn stop_playing_and_draining(&mut self) {
        if let Err(refusal) = self.playback_stream.stop_requesting() {
            self.module_host.log(
                ModuleHostLogLevel::Warn,
                SPEAKER_LOG_TARGET,
                &format!("SpeakerSink: the playback stream failed to stop: {refusal}"),
            );
        }
        let Some(playback) = self.playback.take() else {
            return;
        };
        playback.is_draining.store(false, Ordering::Release);
        playback.samples_awaiting_playback.end_playback();
        let drain_thread_finished = playback
            .drain_thread_finished
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        match drain_thread_finished.recv_timeout(DRAIN_THREAD_EXIT_GRACE) {
            Ok(()) => {
                let _ = playback.drain_thread.join();
            }
            Err(_) => self.module_host.log(
                ModuleHostLogLevel::Warn,
                SPEAKER_LOG_TARGET,
                "SpeakerSink: the drain thread did not exit within its grace and was detached",
            ),
        }
        let ended_ring = &playback.samples_awaiting_playback;
        self.silence_counts_of_ended_playbacks.underrun_byte_count +=
            ended_ring.underrun_byte_count();
        self.silence_counts_of_ended_playbacks
            .silence_before_playback_began_byte_count +=
            ended_ring.silence_played_before_the_cushion_filled_byte_count();
    }
}

impl Drop for SpeakerPlayback {
    fn drop(&mut self) {
        self.stop_playing_and_draining();
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<SpeakerPlayback>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT_48_KHZ_STEREO_F32: ModuleHostAudioStreamFormat = ModuleHostAudioStreamFormat {
        sample_rate: 48_000,
        channels: 2,
        sample_format: ModuleHostAudioSampleFormat::F32,
    };

    #[test]
    fn a_stream_reporting_its_device_period_sizes_the_ring_and_warnings_from_it() {
        let one_period_in_bytes = 512 * 2 * 4;
        assert_eq!(
            SpeakerSizingFromTheDevicePeriod::for_format_and_reported_period(
                AT_48_KHZ_STEREO_F32,
                Some(512)
            ),
            SpeakerSizingFromTheDevicePeriod {
                ring_byte_capacity: one_period_in_bytes * DEVICE_PERIODS_THE_RING_HOLDS,
                silence_bytes_between_underrun_warnings: one_period_in_bytes as u64
                    * SILENT_PERIODS_BETWEEN_UNDERRUN_WARNINGS,
            }
        );
    }

    #[test]
    fn a_stream_reporting_no_period_is_sized_from_ten_milliseconds() {
        for reported_period in [None, Some(0)] {
            assert_eq!(
                SpeakerSizingFromTheDevicePeriod::for_format_and_reported_period(
                    AT_48_KHZ_STEREO_F32,
                    reported_period
                )
                .ring_byte_capacity,
                480 * 2 * 4 * DEVICE_PERIODS_THE_RING_HOLDS
            );
        }
    }

    #[test]
    fn a_stream_at_a_rate_no_period_derives_from_still_gets_a_usable_ring() {
        assert_eq!(
            SpeakerSizingFromTheDevicePeriod::for_format_and_reported_period(
                ModuleHostAudioStreamFormat {
                    sample_rate: 1,
                    channels: 1,
                    sample_format: ModuleHostAudioSampleFormat::I16,
                },
                None
            )
            .ring_byte_capacity,
            SMALLEST_USABLE_RING_BYTES
        );
    }

    #[test]
    fn a_sample_format_is_named_as_the_wire_names_it() {
        assert_eq!(
            sample_format_named("f32").unwrap(),
            ModuleHostAudioSampleFormat::F32
        );
        assert_eq!(
            sample_format_named("i16").unwrap(),
            ModuleHostAudioSampleFormat::I16
        );
    }
}
