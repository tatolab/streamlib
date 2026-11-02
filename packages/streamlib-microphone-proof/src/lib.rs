// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-microphone-proof`: ALSA capture on its own thread into a
//! lock-free ring, drained by the node's Python one whole message at a time.

mod alsa_capture_library;
mod alsa_capture_thread;
mod audio_block_message;
mod captured_audio_block_ring;
mod monotonic_clock;
mod realtime_thread_priority;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use pyo3::exceptions::{PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::alsa_capture_library::{AlsaCaptureLibrary, OpenedCapturePcm, OpenedCapturePcmFormat};
use crate::alsa_capture_thread::{AlsaCaptureThreadReport, capture_into_the_ring_until_stopped};
use crate::audio_block_message::{
    CapturedF32BlockDescription, audio_block_message_payload_for_f32_block,
};
use crate::captured_audio_block_ring::{
    CapturedAudioBlockRing, NextCapturedAudioBlock, RingDrainedFromASecondThread,
};

/// Device periods the ALSA buffer holds, so one late read never overruns it.
const ALSA_BUFFER_PERIODS: u64 = 4;

/// One ALSA capture stream, its device thread, and the ring between them.
#[pyclass]
struct AlsaMicrophoneCapture {
    pcm_awaiting_start: Mutex<Option<OpenedCapturePcm>>,
    format: OpenedCapturePcmFormat,
    block_frames: u32,
    ring: Arc<CapturedAudioBlockRing>,
    stop_requested: Arc<AtomicBool>,
    report: Arc<AlsaCaptureThreadReport>,
    device_thread: Option<JoinHandle<()>>,
}

/// One captured block, already encoded as its audio-block message.
#[pyclass(frozen)]
struct CapturedAudioBlockMessage {
    payload: Vec<u8>,
    #[pyo3(get)]
    first_sample_timestamp_ns: i64,
    #[pyo3(get)]
    sample_count: u32,
}

#[pymethods]
impl CapturedAudioBlockMessage {
    /// The msgpack audio-block payload, ready for `outputs.write`.
    #[getter]
    fn payload<'python>(&self, python: Python<'python>) -> Bound<'python, PyBytes> {
        PyBytes::new(python, &self.payload)
    }
}

#[pymethods]
impl AlsaMicrophoneCapture {
    /// Open `pcm_name` for interleaved `f32` capture; nothing is read until `start()`.
    #[new]
    fn new(
        python: Python<'_>,
        pcm_name: &str,
        sample_rate: u32,
        channels: u32,
        block_frames: u32,
        ring_capacity_blocks: usize,
    ) -> PyResult<Self> {
        if sample_rate == 0 || channels == 0 || block_frames == 0 || ring_capacity_blocks == 0 {
            return Err(PyValueError::new_err(
                "sample_rate, channels, block_frames and ring_capacity_blocks must all be positive",
            ));
        }
        let buffer_latency_us = u32::try_from(
            u64::from(block_frames) * ALSA_BUFFER_PERIODS * 1_000_000 / u64::from(sample_rate),
        )
        .map_err(|_| PyValueError::new_err("block_frames is too large for an ALSA buffer"))?;
        let (pcm, format) = python
            .detach(|| {
                let library = Arc::new(AlsaCaptureLibrary::load()?);
                library.open_capture_pcm(pcm_name, sample_rate, channels, buffer_latency_us)
            })
            .map_err(PyOSError::new_err)?;
        Ok(Self {
            pcm_awaiting_start: Mutex::new(Some(pcm)),
            format,
            block_frames,
            ring: Arc::new(CapturedAudioBlockRing::with_capacity(ring_capacity_blocks)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            report: Arc::new(AlsaCaptureThreadReport::default()),
            device_thread: None,
        })
    }

    /// Start the device thread.
    fn start(&mut self) -> PyResult<()> {
        let Some(pcm) = self.pcm_awaiting_start_taken() else {
            return Err(PyRuntimeError::new_err(
                "this capture already started; open a new one to capture again",
            ));
        };
        let format = self.format;
        let block_frames = self.block_frames;
        let ring = Arc::clone(&self.ring);
        let stop_requested = Arc::clone(&self.stop_requested);
        let report = Arc::clone(&self.report);
        let device_thread = std::thread::Builder::new()
            .name("alsa-capture".to_string())
            .spawn(move || {
                capture_into_the_ring_until_stopped(
                    pcm,
                    format,
                    block_frames,
                    &ring,
                    &stop_requested,
                    &report,
                );
            })
            .map_err(|error| {
                PyRuntimeError::new_err(format!("could not spawn the device thread: {error}"))
            })?;
        self.device_thread = Some(device_thread);
        Ok(())
    }

    /// The next captured block's message, waiting up to `timeout_ms` with the GIL released;
    /// `None` on a timeout or once the capture has ended and the ring is empty.
    fn next_message(
        &self,
        python: Python<'_>,
        timeout_ms: u64,
    ) -> PyResult<Option<CapturedAudioBlockMessage>> {
        let format = self.format;
        let ring = &self.ring;
        python.detach(|| {
            let next = ring
                .wait_for_next_block(Duration::from_millis(timeout_ms))
                .map_err(|RingDrainedFromASecondThread| {
                    PyRuntimeError::new_err(
                        "next_message was called from a second thread; one thread drains a capture",
                    )
                })?;
            let NextCapturedAudioBlock::Block(block) = next else {
                return Ok(None);
            };
            let payload = audio_block_message_payload_for_f32_block(
                &block.interleaved_f32_sample_bytes,
                CapturedF32BlockDescription {
                    sample_rate: format.sample_rate,
                    channels: format.channels,
                    sample_count: block.sample_count,
                    first_sample_timestamp_ns: block.first_sample_timestamp_ns,
                },
            )
            .map_err(|error| {
                PyRuntimeError::new_err(format!("encoding a block failed: {error}"))
            })?;
            Ok(Some(CapturedAudioBlockMessage {
                payload,
                first_sample_timestamp_ns: block.first_sample_timestamp_ns,
                sample_count: block.sample_count,
            }))
        })
    }

    /// Stop the device thread and close the device; what it already captured stays readable.
    fn stop(&mut self, python: Python<'_>) {
        self.stop_requested.store(true, Ordering::Release);
        drop(self.pcm_awaiting_start_taken());
        if let Some(device_thread) = self.device_thread.take() {
            python.detach(|| {
                let _ = device_thread.join();
            });
        }
        self.ring.end_hand_off();
    }

    /// Whether the device thread has ended, stopped or failed.
    #[getter]
    fn capture_has_ended(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
            || self
                .device_thread
                .as_ref()
                .is_some_and(|device_thread| device_thread.is_finished())
    }

    /// Blocks the ring dropped because the drain fell behind.
    #[getter]
    fn ring_dropped_block_count(&self) -> u64 {
        self.ring.dropped_block_count()
    }

    /// Device overruns (`-EPIPE`) the device thread recovered from.
    #[getter]
    fn device_overrun_count(&self) -> u64 {
        self.report.device_overrun_count.load(Ordering::Relaxed)
    }

    /// Blocks the device thread read off the device.
    #[getter]
    fn captured_block_count(&self) -> u64 {
        self.report.captured_block_count.load(Ordering::Relaxed)
    }

    /// How the device thread is scheduled, once it has started.
    #[getter]
    fn device_thread_priority(&self) -> Option<String> {
        self.report
            .device_thread_priority
            .lock()
            .ok()
            .and_then(|priority| priority.clone())
    }

    /// Why the device thread ended on its own, if it did.
    #[getter]
    fn failure_that_ended_the_capture(&self) -> Option<String> {
        self.report
            .failure_that_ended_the_capture
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
    }

    #[getter]
    fn sample_rate(&self) -> u32 {
        self.format.sample_rate
    }

    #[getter]
    fn channels(&self) -> u32 {
        self.format.channels
    }

    /// The ALSA buffer the device negotiated, in frames.
    #[getter]
    fn buffer_frames(&self) -> u64 {
        self.format.buffer_frames
    }

    /// The ALSA period the device negotiated, in frames.
    #[getter]
    fn period_frames(&self) -> u64 {
        self.format.period_frames
    }
}

impl AlsaMicrophoneCapture {
    fn pcm_awaiting_start_taken(&self) -> Option<OpenedCapturePcm> {
        self.pcm_awaiting_start
            .lock()
            .map_or(None, |mut pcm_awaiting_start| pcm_awaiting_start.take())
    }
}

/// Make the calling OS thread realtime the way the engine does; how it ended up scheduled.
#[pyfunction]
fn make_the_calling_thread_realtime() -> String {
    realtime_thread_priority::make_the_calling_thread_realtime().description()
}

/// Nanoseconds on the clock this wheel stamps blocks with.
#[pyfunction]
fn monotonic_now_ns() -> i64 {
    monotonic_clock::monotonic_now_ns()
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<AlsaMicrophoneCapture>()?;
    module.add_class::<CapturedAudioBlockMessage>()?;
    module.add_function(wrap_pyfunction!(monotonic_now_ns, module)?)?;
    module.add_function(wrap_pyfunction!(make_the_calling_thread_realtime, module)?)?;
    Ok(())
}
