// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use pyo3::create_exception;
use pyo3::exceptions::{PyRuntimeError, PyTimeoutError};
use pyo3::prelude::*;
use streamlib_owned_device_video_codec_nodes::video_coding_sessions::VideoCodingSessionNotOpened;
use streamlib_owned_device_video_codec_nodes::video_decoder_settings::VideoDecoderSettings;
use streamlib_owned_device_video_codec_nodes::video_encoder_settings::VideoEncoderSettings;
use streamlib_owned_device_video_codec_nodes::{
    VideoCodecNodeIdentity, VideoCodecNodeSession, VideoCodecNodeThread,
};
use tatolab_module_sdk::{ModuleHost, ModuleHostCallTooSlowForAFrame};

create_exception!(
    _native,
    VideoCodingSessionRefused,
    PyRuntimeError,
    "The codec refused a session: its settings, the device, or the video codec host. The \
     sessions already open keep coding."
);
create_exception!(
    _native,
    VideoCodingThreadEnded,
    PyRuntimeError,
    "The coding thread has ended, so no session opens on it."
);
create_exception!(
    _native,
    VideoCodingSessionUnanswered,
    PyTimeoutError,
    "The coding thread did not open a session in time; it closes the session if it opens after."
);

fn raised(not_opened: VideoCodingSessionNotOpened) -> PyErr {
    let words = not_opened.to_string();
    match not_opened {
        VideoCodingSessionNotOpened::Refused(_) => VideoCodingSessionRefused::new_err(words),
        VideoCodingSessionNotOpened::CodingThreadEnded { .. } => {
            VideoCodingThreadEnded::new_err(words)
        }
        VideoCodingSessionNotOpened::Unanswered { .. } => {
            VideoCodingSessionUnanswered::new_err(words)
        }
    }
}

/// `H265Encoder`'s encoding thread, started over the module host table its
/// node lent `video` and `encoded_video` through, and encoding with the newest
/// session opened on it.
#[pyclass(frozen)]
struct H265EncoderThread {
    module_host: ModuleHost,
    encoding_thread: VideoCodecNodeThread<VideoEncoderSettings>,
}

#[pymethods]
impl H265EncoderThread {
    /// Start reading `video` and writing `encoded_video`; frames are drained
    /// until a session opens.
    #[new]
    fn start(module_host_table: &Bound<'_, PyAny>) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        module_host.refuse_while_a_frame_runs(
            ModuleHostCallTooSlowForAFrame::StartsASession,
            "H265EncoderThread",
        )?;
        let encoding_thread = VideoCodecNodeThread::start_encoder(
            VideoCodecNodeIdentity::H265_ENCODER,
            module_host.clone(),
        )
        .map_err(VideoCodingThreadEnded::new_err)?;
        Ok(Self {
            module_host,
            encoding_thread,
        })
    }

    /// Open a session of these settings beside the ones open, and encode
    /// every frame with it from then on.
    #[pyo3(signature = (
        *,
        width=None,
        height=None,
        fps=None,
        bitrate_bps=None,
        keyframe_interval_seconds=None,
        effort_level=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn open_session(
        &self,
        python: Python<'_>,
        width: Option<u32>,
        height: Option<u32>,
        fps: Option<u32>,
        bitrate_bps: Option<u32>,
        keyframe_interval_seconds: Option<u32>,
        effort_level: Option<u32>,
    ) -> PyResult<H265EncodeSession> {
        self.module_host.refuse_while_a_frame_runs(
            ModuleHostCallTooSlowForAFrame::StartsASession,
            "H265EncoderThread.open_session",
        )?;
        let settings = VideoEncoderSettings {
            width,
            height,
            fps,
            bitrate_bps,
            keyframe_interval_seconds,
            effort_level,
        };
        let session = python
            .detach(|| self.encoding_thread.open_session(settings))
            .map_err(raised)?;
        Ok(H265EncodeSession { session })
    }
}

/// A session open on an `H265EncoderThread`, closed by `close()` or when let go.
#[pyclass(frozen)]
struct H265EncodeSession {
    session: VideoCodecNodeSession<VideoEncoderSettings>,
}

#[pymethods]
impl H265EncodeSession {
    /// Close the session: the thread drops it before its next frame.
    fn close(&self) {
        self.session.close();
    }
}

/// `H265Decoder`'s decoding thread, started over the module host table its
/// node lent `encoded_video` and `video` through, and moving to the newest
/// session opened on it at a group start.
#[pyclass(frozen)]
struct H265DecoderThread {
    module_host: ModuleHost,
    decoding_thread: VideoCodecNodeThread<VideoDecoderSettings>,
}

#[pymethods]
impl H265DecoderThread {
    /// Start reading `encoded_video` and writing `video`; access units are
    /// drained until a session opens.
    #[new]
    fn start(module_host_table: &Bound<'_, PyAny>) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        module_host.refuse_while_a_frame_runs(
            ModuleHostCallTooSlowForAFrame::StartsASession,
            "H265DecoderThread",
        )?;
        let decoding_thread = VideoCodecNodeThread::start_decoder(
            VideoCodecNodeIdentity::H265_DECODER,
            module_host.clone(),
        )
        .map_err(VideoCodingThreadEnded::new_err)?;
        Ok(Self {
            module_host,
            decoding_thread,
        })
    }

    /// Open a session of these settings beside the ones open; the thread
    /// moves to it at the next group start.
    #[pyo3(signature = (*, max_width=None, max_height=None))]
    fn open_session(
        &self,
        python: Python<'_>,
        max_width: Option<u32>,
        max_height: Option<u32>,
    ) -> PyResult<H265DecodeSession> {
        self.module_host.refuse_while_a_frame_runs(
            ModuleHostCallTooSlowForAFrame::StartsASession,
            "H265DecoderThread.open_session",
        )?;
        let settings = VideoDecoderSettings {
            max_width,
            max_height,
        };
        let session = python
            .detach(|| self.decoding_thread.open_session(settings))
            .map_err(raised)?;
        Ok(H265DecodeSession { session })
    }
}

/// A session open on an `H265DecoderThread`, closed by `close()` or when let go.
#[pyclass(frozen)]
struct H265DecodeSession {
    session: VideoCodecNodeSession<VideoDecoderSettings>,
}

#[pymethods]
impl H265DecodeSession {
    /// Close the session: the thread drops it before its next media frame.
    fn close(&self) {
        self.session.close();
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<H265EncoderThread>()?;
    module.add_class::<H265EncodeSession>()?;
    module.add_class::<H265DecoderThread>()?;
    module.add_class::<H265DecodeSession>()?;
    let python = module.py();
    module.add(
        "VideoCodingSessionRefused",
        python.get_type::<VideoCodingSessionRefused>(),
    )?;
    module.add(
        "VideoCodingThreadEnded",
        python.get_type::<VideoCodingThreadEnded>(),
    )?;
    module.add(
        "VideoCodingSessionUnanswered",
        python.get_type::<VideoCodingSessionUnanswered>(),
    )?;
    Ok(())
}
