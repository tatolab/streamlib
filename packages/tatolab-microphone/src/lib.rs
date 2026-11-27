// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The native half of `tatolab.microphone`: a capture stream the engine's audio
//! device backend opens in this node's helper process, a bounded hand-off from
//! its device thread, and a thread that publishes each block on the node's
//! `audio` port through the module host table. Python never runs per block.

mod captured_audio_block_hand_off_ring;
mod captured_audio_block_publishing;
mod channels_block_of_a_captured_block;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use tatolab_module_sdk::{
    ModuleHost, ModuleHostAudioCaptureStream, ModuleHostAudioCaptureStreamRequest,
    ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat, ModuleHostCallTooSlowForAFrame,
    ModuleHostCapturedAudioBlock, ModuleHostLogLevel,
};

use captured_audio_block_hand_off_ring::CapturedAudioBlockHandOffRing;
use captured_audio_block_publishing::{CapturedAudioBlockPublishing, MICROPHONE_LOG_TARGET};

/// The one port a captured block is published on.
const AUDIO_OUTPUT_PORT: &str = "audio";

/// Blocks the hand-off ring holds before it drops the oldest: roughly 170 ms
/// at a 48 kHz, 512-sample quantum, the depth a sample-stream link carries.
const MAX_CAPTURED_BLOCKS_AWAITING_PUBLISH: usize = 16;

/// Bound on the wait for the publishing thread to exit, well inside the
/// helper's teardown budget; past it the thread is detached.
const PUBLISH_THREAD_EXIT_GRACE: Duration = Duration::from_secs(2);

/// A delivery in progress: the ring the device callback hands off into and the
/// thread publishing out of it.
struct CapturedAudioBlockDelivery {
    hand_off_ring: Arc<CapturedAudioBlockHandOffRing>,
    is_publishing: Arc<AtomicBool>,
    publish_thread: JoinHandle<()>,
    /// Behind a lock only so the node class can be shared across threads.
    publish_thread_finished: Mutex<Receiver<()>>,
}

/// One node's microphone: a capture stream opened through the module host
/// table, published on the node's lent `audio` port.
#[pyclass(name = "MicrophoneCapture", module = "tatolab.microphone._native")]
struct MicrophoneCapture {
    module_host: ModuleHost,
    /// `None` once closed, its device given back.
    capture_stream: Option<ModuleHostAudioCaptureStream>,
    stream_format: ModuleHostAudioStreamFormat,
    /// Why the device stopped serving the capture, as it stood when the capture closed.
    failure_that_ended_the_closed_capture: Option<String>,
    delivery: Option<CapturedAudioBlockDelivery>,
    published_block_count: Arc<AtomicU64>,
    /// Drops of every delivery that has ended, beside the current one's.
    dropped_block_count_of_ended_deliveries: u64,
}

#[pymethods]
impl MicrophoneCapture {
    /// Open the capture stream on `device_id`, or the backend's default, for
    /// the node whose module host table this is; its `audio` port must be lent.
    #[new]
    #[pyo3(signature = (module_host_table, device_id = None))]
    fn new(
        python: Python<'_>,
        module_host_table: &Bound<'_, PyAny>,
        device_id: Option<String>,
    ) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))?;
        module_host.refuse_while_a_frame_runs(
            ModuleHostCallTooSlowForAFrame::OpensADevice,
            "MicrophoneCapture",
        )?;
        let capture_stream = python
            .detach(|| {
                module_host.open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest {
                    device_id: device_id.as_deref(),
                    ..ModuleHostAudioCaptureStreamRequest::default()
                })
            })
            .map_err(|refusal| PyRuntimeError::new_err(format!("MicrophoneSource: {refusal}")))?;
        Ok(Self {
            module_host,
            stream_format: capture_stream.stream_format(),
            capture_stream: Some(capture_stream),
            failure_that_ended_the_closed_capture: None,
            delivery: None,
            published_block_count: Arc::new(AtomicU64::new(0)),
            dropped_block_count_of_ended_deliveries: 0,
        })
    }

    /// The rate every block is captured at, in hertz.
    #[getter]
    fn sample_rate(&self) -> u32 {
        self.stream_format.sample_rate
    }

    /// The channel count every block is interleaved by.
    #[getter]
    fn channels(&self) -> u32 {
        self.stream_format.channels
    }

    /// The scalar encoding on the wire: `f32` or `i16`.
    #[getter]
    fn sample_format(&self) -> &'static str {
        match self.stream_format.sample_format {
            ModuleHostAudioSampleFormat::F32 => "f32",
            ModuleHostAudioSampleFormat::I16 => "i16",
        }
    }

    /// Blocks published on `audio` so far.
    #[getter]
    fn published_block_count(&self) -> u64 {
        self.published_block_count.load(Ordering::Relaxed)
    }

    /// Blocks dropped at the device edge so far because publishing fell behind.
    #[getter]
    fn dropped_block_count(&self) -> u64 {
        self.dropped_block_count_of_ended_deliveries
            + self
                .delivery
                .as_ref()
                .map_or(0, |delivery| delivery.hand_off_ring.dropped_block_count())
    }

    /// Why the device stopped serving the capture, or `None` while it serves it.
    #[getter]
    fn failure_that_ended_the_capture(&self) -> PyResult<Option<String>> {
        let Some(capture_stream) = &self.capture_stream else {
            return Ok(self.failure_that_ended_the_closed_capture.clone());
        };
        capture_stream
            .failure_that_ended_the_stream()
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
    }

    /// Begin capturing and publishing; a capture already started is left as it is, and a closed
    /// one is refused.
    fn start(&mut self, python: Python<'_>) -> PyResult<()> {
        if self.delivery.is_some() {
            return Ok(());
        }
        let Some(capture_stream) = self.capture_stream.as_mut() else {
            return Err(PyRuntimeError::new_err(
                "MicrophoneSource: this capture is closed and its device given back; open a new one",
            ));
        };
        let capture_stream_liveness = capture_stream.liveness();
        let audio_output_port = self
            .module_host
            .open_output_port(AUDIO_OUTPUT_PORT)
            .map_err(|refusal| PyRuntimeError::new_err(format!("MicrophoneSource: {refusal}")))?;
        let hand_off_ring = Arc::new(CapturedAudioBlockHandOffRing::with_capacity(
            MAX_CAPTURED_BLOCKS_AWAITING_PUBLISH,
        ));
        let is_publishing = Arc::new(AtomicBool::new(true));
        let publishing = CapturedAudioBlockPublishing {
            module_host: self.module_host.clone(),
            audio_output_port,
            capture_stream_liveness,
            stream_format: self.stream_format,
            hand_off_ring: Arc::clone(&hand_off_ring),
            is_publishing: Arc::clone(&is_publishing),
            published_block_count: Arc::clone(&self.published_block_count),
        };
        let (publish_thread_finished_sender, publish_thread_finished) = sync_channel(1);
        let publish_thread = std::thread::Builder::new()
            .name("microphone-publish".to_string())
            .spawn(move || {
                publishing.publish_until_stopped();
                let _ = publish_thread_finished_sender.send(());
            })
            .map_err(|spawn_failure| {
                PyRuntimeError::new_err(format!(
                    "MicrophoneSource: the publishing thread could not start: {spawn_failure}"
                ))
            })?;
        let hand_off_ring_in_the_device_callback = Arc::clone(&hand_off_ring);
        let started = python.detach(|| {
            capture_stream.start_delivering_to(Box::new(
                move |captured: ModuleHostCapturedAudioBlock<'_>| {
                    hand_off_ring_in_the_device_callback.hand_off_from_device_callback(
                        captured.interleaved_sample_bytes.to_vec(),
                        captured.sample_count,
                        captured.first_sample_timestamp_ns,
                    );
                },
            ))
        });
        self.delivery = Some(CapturedAudioBlockDelivery {
            hand_off_ring,
            is_publishing,
            publish_thread,
            publish_thread_finished: Mutex::new(publish_thread_finished),
        });
        if let Err(refusal) = started {
            self.close(python);
            return Err(PyRuntimeError::new_err(format!(
                "MicrophoneSource: {refusal}"
            )));
        }
        Ok(())
    }

    /// Stop capturing, publish what was already captured, end the publishing
    /// thread within its grace, give the device back and log what the capture
    /// published. Idempotent.
    fn close(&mut self, python: Python<'_>) {
        python.detach(|| self.stop_capturing_and_publishing());
    }
}

impl MicrophoneCapture {
    fn stop_capturing_and_publishing(&mut self) {
        let Some(mut capture_stream) = self.capture_stream.take() else {
            return;
        };
        if let Err(refusal) = capture_stream.stop_delivering() {
            self.module_host.log(
                ModuleHostLogLevel::Warn,
                MICROPHONE_LOG_TARGET,
                &format!("MicrophoneSource: the capture stream failed to stop: {refusal}"),
            );
        }
        self.end_the_delivery();
        let failure_that_ended_the_capture = capture_stream.failure_that_ended_the_stream();
        drop(capture_stream);
        self.failure_that_ended_the_closed_capture = failure_that_ended_the_capture
            .as_ref()
            .ok()
            .cloned()
            .flatten();
        self.module_host.log(
            ModuleHostLogLevel::Info,
            MICROPHONE_LOG_TARGET,
            &format!(
                "MicrophoneSource: closed. published_blocks={} dropped_blocks={} \
                 capture_device_failure={failure_that_ended_the_capture:?}",
                self.published_block_count.load(Ordering::Relaxed),
                self.dropped_block_count_of_ended_deliveries,
            ),
        );
    }

    /// End the delivery in progress, if one is, within its publishing thread's grace.
    fn end_the_delivery(&mut self) {
        let Some(delivery) = self.delivery.take() else {
            return;
        };
        delivery.is_publishing.store(false, Ordering::Release);
        delivery.hand_off_ring.end_hand_off();
        let publish_thread_finished = delivery
            .publish_thread_finished
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        match publish_thread_finished.recv_timeout(PUBLISH_THREAD_EXIT_GRACE) {
            Ok(()) => {
                let _ = delivery.publish_thread.join();
            }
            Err(_) => self.module_host.log(
                ModuleHostLogLevel::Warn,
                MICROPHONE_LOG_TARGET,
                "MicrophoneSource: the publishing thread did not exit within its grace and was \
                 detached",
            ),
        }
        self.dropped_block_count_of_ended_deliveries +=
            delivery.hand_off_ring.dropped_block_count();
    }
}

impl Drop for MicrophoneCapture {
    fn drop(&mut self) {
        self.stop_capturing_and_publishing();
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<MicrophoneCapture>()?;
    Ok(())
}
