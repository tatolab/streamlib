// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-h264` — the module `tatolab.h264.processors`
//! imports as `tatolab.h264._native`.
//!
//! The engine never calls anything here. `H264Encoder` lends its `video`
//! input and `encoded_video` output to a thread of this wheel's Rust through
//! the module host table; the thread reads, encodes on a Vulkan device it
//! owns and writes with no Python attached.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to;
use tatolab_module_sdk::{ModuleHost, ModuleHostInputPort, ModuleHostOutputPort};

pub mod encoded_video_publication;
pub mod h264_codec_string;
pub mod h264_encoder_settings;
pub mod hang_legacy_container_media_frame;
#[cfg(target_os = "linux")]
pub mod owned_device_h264_encoder;
pub mod video_frame_message;

use crate::encoded_video_publication::EncodedVideoOutputPort;
use crate::h264_encoder_settings::H264EncoderSettings;
use crate::video_frame_message::VideoFrameMessage;

/// The input port frames arrive on.
pub const VIDEO_INPUT_PORT: &str = "video";
/// The output port media frames are written to.
pub const ENCODED_VIDEO_OUTPUT_PORT: &str = "encoded_video";

/// How long one wait for a frame lasts before the stop flag is read again.
const VIDEO_INPUT_WAIT: Duration = Duration::from_millis(100);

/// `H264Encoder`'s encoding thread, started over the module host table its
/// node lent `video` and `encoded_video` through.
#[pyclass]
struct H264EncoderThread {
    stop_requested: Arc<AtomicBool>,
    encoding_thread: Mutex<Option<EncodingThreadEnding>>,
}

struct EncodingThreadEnding {
    join_handle: std::thread::JoinHandle<()>,
    ended: mpsc::Receiver<()>,
}

#[pymethods]
impl H264EncoderThread {
    /// Start encoding the frames `video` receives onto `encoded_video`.
    #[new]
    #[pyo3(signature = (
        module_host_table,
        *,
        width=None,
        height=None,
        fps=None,
        bitrate_bps=None,
        keyframe_interval_seconds=None,
        effort_level=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn start(
        module_host_table: &Bound<'_, PyAny>,
        width: Option<u32>,
        height: Option<u32>,
        fps: Option<u32>,
        bitrate_bps: Option<u32>,
        keyframe_interval_seconds: Option<u32>,
        effort_level: Option<u32>,
    ) -> PyResult<Self> {
        let settings = H264EncoderSettings {
            width,
            height,
            fps,
            bitrate_bps,
            keyframe_interval_seconds,
            effort_level,
        };
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        let video_input = module_host
            .open_input_port(VIDEO_INPUT_PORT)
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))?;
        let encoded_video_output = module_host
            .open_output_port(ENCODED_VIDEO_OUTPUT_PORT)
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))?;
        forward_this_modules_tracing_records_to(&module_host);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let stop_requested_of_the_thread = Arc::clone(&stop_requested);
        let (thread_ended, ended) = mpsc::channel();
        let join_handle = std::thread::Builder::new()
            .name("h264-encoder".to_owned())
            .spawn(move || {
                encode_until_stopped(
                    &module_host,
                    &video_input,
                    &encoded_video_output,
                    settings,
                    &stop_requested_of_the_thread,
                );
                let _ = thread_ended.send(());
            })
            .map_err(|spawn_failure| {
                PyRuntimeError::new_err(format!(
                    "the H.264 encoder's thread did not start: {spawn_failure}"
                ))
            })?;
        Ok(Self {
            stop_requested,
            encoding_thread: Mutex::new(Some(EncodingThreadEnding { join_handle, ended })),
        })
    }

    /// Ask the thread to stop and wait up to `timeout_seconds` for it: `True`
    /// when it ended.
    fn stop_and_join(&self, python: Python<'_>, timeout_seconds: f64) -> bool {
        self.stop_requested.store(true, Ordering::Release);
        let timeout = Duration::try_from_secs_f64(timeout_seconds).unwrap_or(Duration::ZERO);
        python.detach(|| {
            let Ok(mut encoding_thread) = self.encoding_thread.lock() else {
                return false;
            };
            let Some(ending) = encoding_thread.take() else {
                return true;
            };
            match ending.ended.recv_timeout(timeout) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = ending.join_handle.join();
                    true
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    *encoding_thread = Some(ending);
                    false
                }
            }
        })
    }
}

/// `encoded_video` as the publication writes it.
struct EncodedVideoLentByTheModuleHost<'port>(&'port ModuleHostOutputPort);

impl EncodedVideoOutputPort for EncodedVideoLentByTheModuleHost<'_> {
    fn set_catalog_entry(&self, section: &str, entry_json: &str) -> Result<(), String> {
        self.0
            .set_catalog_entry(section, entry_json)
            .map_err(|refusal| refusal.to_string())
    }

    fn write(
        &self,
        media_frame: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
    ) -> Result<(), String> {
        self.0
            .write(media_frame, Some(timestamp_ns), starts_group)
            .map_err(|refusal| refusal.to_string())
    }
}

/// The frame source the thread encodes from: the owned device on Linux, and
/// on any other platform a refusal naming why every frame is discarded.
#[cfg(target_os = "linux")]
type H264EncoderOnThisPlatform = owned_device_h264_encoder::OwnedDeviceH264Encoder;

#[cfg(target_os = "linux")]
fn open_the_encoder(settings: H264EncoderSettings) -> Result<H264EncoderOnThisPlatform, String> {
    H264EncoderOnThisPlatform::open(settings)
}

#[cfg(not(target_os = "linux"))]
struct H264EncoderOnThisPlatform;

#[cfg(not(target_os = "linux"))]
impl H264EncoderOnThisPlatform {
    fn encode_and_publish(
        &mut self,
        _frame: &VideoFrameMessage,
        _encoded_video: &dyn EncodedVideoOutputPort,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
fn open_the_encoder(_settings: H264EncoderSettings) -> Result<H264EncoderOnThisPlatform, String> {
    Err("H264Encoder encodes with Vulkan Video, on Linux only".to_owned())
}

/// Read, encode and write until the node stops. An encoder that cannot open
/// keeps draining `video`, so upstream still sees a live consumer.
fn encode_until_stopped(
    module_host: &ModuleHost,
    video_input: &ModuleHostInputPort,
    encoded_video_output: &ModuleHostOutputPort,
    settings: H264EncoderSettings,
    stop_requested: &AtomicBool,
) {
    let mut encoder = match open_the_encoder(settings) {
        Ok(encoder) => Some(encoder),
        Err(refusal) => {
            tracing::error!("{refusal}; every frame is discarded");
            None
        }
    };
    let encoded_video = EncodedVideoLentByTheModuleHost(encoded_video_output);
    let mut frames_refused: u64 = 0;
    while !stop_requested.load(Ordering::Acquire) && !module_host.stop_requested() {
        if let Err(refusal) = module_host.wait_for_any_input_port(VIDEO_INPUT_WAIT) {
            tracing::error!("H264Encoder: waiting for a frame was refused: {refusal}");
            return;
        }
        loop {
            let message = match video_input.read() {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(refusal) => {
                    tracing::error!("H264Encoder: reading `video` was refused: {refusal}");
                    return;
                }
            };
            let Some(encoder) = encoder.as_mut() else {
                continue;
            };
            let encoded = VideoFrameMessage::from_msgpack(message.payload())
                .and_then(|frame| encoder.encode_and_publish(&frame, &encoded_video));
            if let Err(refusal) = encoded {
                frames_refused += 1;
                if frames_refused.is_power_of_two() {
                    tracing::error!(frames_refused, "{refusal}");
                }
            }
        }
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<H264EncoderThread>()?;
    Ok(())
}
