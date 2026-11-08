// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-h265` — the module `tatolab.h265.processors`
//! imports as `tatolab.h265._native`.
//!
//! The engine never calls anything here. `H265Encoder` and `H265Decoder` lend
//! their ports to a thread of this wheel's Rust through the module host table;
//! the thread reads, codes on a Vulkan device it owns and writes with no
//! Python attached.

use std::time::Duration;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib_owned_device_video_codec_nodes::video_decoder_settings::VideoDecoderSettings;
use streamlib_owned_device_video_codec_nodes::video_encoder_settings::VideoEncoderSettings;
use streamlib_owned_device_video_codec_nodes::{VideoCodecNodeIdentity, VideoCodecNodeThread};
use tatolab_module_sdk::ModuleHost;

/// `H265Encoder`'s encoding thread, started over the module host table its
/// node lent `video` and `encoded_video` through.
#[pyclass]
struct H265EncoderThread {
    encoding_thread: VideoCodecNodeThread,
}

#[pymethods]
impl H265EncoderThread {
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
        let settings = VideoEncoderSettings {
            width,
            height,
            fps,
            bitrate_bps,
            keyframe_interval_seconds,
            effort_level,
        };
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        let encoding_thread = VideoCodecNodeThread::start_encoder(
            VideoCodecNodeIdentity::H265_ENCODER,
            module_host,
            settings,
        )
        .map_err(PyRuntimeError::new_err)?;
        Ok(Self { encoding_thread })
    }

    /// Ask the thread to stop and wait up to `timeout_seconds` for it: `True`
    /// when it ended.
    fn stop_and_join(&self, python: Python<'_>, timeout_seconds: f64) -> bool {
        let timeout = Duration::try_from_secs_f64(timeout_seconds).unwrap_or(Duration::ZERO);
        python.detach(|| self.encoding_thread.stop_and_join(timeout))
    }
}

/// `H265Decoder`'s decoding thread, started over the module host table its
/// node lent `encoded_video` and `video` through.
#[pyclass]
struct H265DecoderThread {
    decoding_thread: VideoCodecNodeThread,
}

#[pymethods]
impl H265DecoderThread {
    /// Start decoding the access units `encoded_video` receives onto `video`.
    #[new]
    #[pyo3(signature = (module_host_table, *, max_width=None, max_height=None))]
    fn start(
        module_host_table: &Bound<'_, PyAny>,
        max_width: Option<u32>,
        max_height: Option<u32>,
    ) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        let decoding_thread = VideoCodecNodeThread::start_decoder(
            VideoCodecNodeIdentity::H265_DECODER,
            module_host,
            VideoDecoderSettings {
                max_width,
                max_height,
            },
        )
        .map_err(PyRuntimeError::new_err)?;
        Ok(Self { decoding_thread })
    }

    /// Ask the thread to stop and wait up to `timeout_seconds` for it: `True`
    /// when it ended.
    fn stop_and_join(&self, python: Python<'_>, timeout_seconds: f64) -> bool {
        let timeout = Duration::try_from_secs_f64(timeout_seconds).unwrap_or(Duration::ZERO);
        python.detach(|| self.decoding_thread.stop_and_join(timeout))
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<H265EncoderThread>()?;
    module.add_class::<H265DecoderThread>()?;
    Ok(())
}
