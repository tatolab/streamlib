// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `streamlib-webrtc` — the module the wheel's two
//! `@processor` classes import as `streamlib_webrtc._native`.
//!
//! The engine never calls anything here. A processor extension's per-frame work
//! is its own package's Rust, reached directly from its own Python.

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::time::Duration;

pub mod encoded_stream_ordering;
pub mod error;
pub mod h264_rtp_depacketiser;
pub mod h264_sequence_parameter_set;
mod h264_test_bitstreams;
pub mod http_signalling;
pub mod monotonic_clock;
pub mod opus_packet;
pub mod received_media_assembly;
pub mod session_description;
pub mod transport_stack;
pub mod whep_session;
pub mod whip_session;

use crate::error::WebRtcExtensionError;
use crate::received_media_assembly::{ReceivedOpusPacket, ReceivedVideoAccessUnit};
use crate::whep_session::{ReceivedMedia, WhepPlayingSession};
use crate::whip_session::{PublishedMediaSet, WhipPublishingSession};

/// Only the first frame of a stream, so that no two frames share a presentation
/// timestamp. Every later one advances by the gap the bags' own stamps state.
const FIRST_FRAME_NOMINAL_DURATION: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// Bring up the tokio runtime and the TLS provider this wheel's sessions share.
#[pyfunction]
fn bring_up_the_transport_stack() -> PyResult<()> {
    transport_stack::bring_up()?;
    Ok(())
}

/// Publishes encoded media to a WHIP endpoint.
#[pyclass]
struct WhipSession {
    endpoint_url: String,
    bearer_token: Option<String>,
    session: Option<WhipPublishingSession>,
    previous_video_stamp_ns: Option<i64>,
}

#[pymethods]
impl WhipSession {
    /// Constructs without connecting: opening the session is what the first
    /// bag does, so a relay round trip never runs inside `setup()`.
    #[new]
    #[pyo3(signature = (endpoint_url, bearer_token=None))]
    fn new(endpoint_url: String, bearer_token: Option<String>) -> Self {
        Self {
            endpoint_url,
            bearer_token,
            session: None,
            previous_video_stamp_ns: None,
        }
    }

    /// Offer the media set the publisher's links settled on, and set the answer.
    #[pyo3(signature = (*, video, audio, audio_channels=None))]
    fn connect(
        &mut self,
        python: Python<'_>,
        video: bool,
        audio: bool,
        audio_channels: Option<u32>,
    ) -> PyResult<()> {
        if self.session.is_some() {
            return Err(WebRtcExtensionError::Refused {
                what: "this WHIP session is already connected".to_owned(),
            }
            .into());
        }
        let endpoint_url = self.endpoint_url.clone();
        let bearer_token = self.bearer_token.clone();
        let media = PublishedMediaSet {
            video,
            audio,
            audio_channels,
        };

        let connected = python.detach(|| {
            transport_stack::transport_runtime()?.block_on(WhipPublishingSession::connect(
                endpoint_url,
                bearer_token,
                media,
            ))
        })?;
        self.session = Some(connected);
        Ok(())
    }

    /// Send one whole Annex-B access unit.
    ///
    /// The RTP clock advances by the gap to the *previous* frame, because the
    /// payloader applies a sample's duration to the frame after it and a
    /// publisher cannot see the next frame without delaying this one. The
    /// stream's rate is therefore exact and its RTP numbering trails real time
    /// by one frame — which the receiver's Sender Reports carry consistently.
    fn write_video_access_unit(
        &mut self,
        python: Python<'_>,
        annex_b_access_unit: &[u8],
        timestamp_ns: i64,
    ) -> PyResult<()> {
        let duration = match self.previous_video_stamp_ns.replace(timestamp_ns) {
            Some(previous) => {
                Duration::from_nanos(timestamp_ns.saturating_sub(previous).max(0) as u64)
            }
            None => FIRST_FRAME_NOMINAL_DURATION,
        };
        let session = self.connected_session()?;
        let access_unit = bytes::Bytes::copy_from_slice(annex_b_access_unit);

        python.detach(|| {
            transport_stack::transport_runtime()?
                .block_on(session.write_video_access_unit(access_unit, duration))
        })?;
        Ok(())
    }

    /// Send one Opus packet, its RTP advance taken from the packet's own sample
    /// count rather than from an assumed 20 ms frame.
    fn write_audio_packet(
        &mut self,
        python: Python<'_>,
        opus_packet: &[u8],
        sample_count: u32,
    ) -> PyResult<()> {
        let duration = Duration::from_nanos(
            u64::from(sample_count) * 1_000_000_000
                / u64::from(opus_packet::OPUS_WIRE_SAMPLE_RATE_HZ),
        );
        let session = self.connected_session()?;
        let packet = bytes::Bytes::copy_from_slice(opus_packet);

        python.detach(|| {
            transport_stack::transport_runtime()?
                .block_on(session.write_audio_packet(packet, duration))
        })?;
        Ok(())
    }

    /// Close the peer connection and DELETE the session.
    fn close(&mut self, python: Python<'_>) -> PyResult<()> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        python.detach(|| {
            if let Ok(runtime) = transport_stack::transport_runtime() {
                runtime.block_on(session.close());
            }
        });
        Ok(())
    }

    #[getter]
    fn is_connected(&self) -> bool {
        self.session.is_some()
    }
}

impl WhipSession {
    fn connected_session(&self) -> PyResult<&WhipPublishingSession> {
        self.session
            .as_ref()
            .ok_or_else(|| WebRtcExtensionError::NotConnected { protocol: "WHIP" }.into())
    }
}

/// Plays encoded media back from a WHEP endpoint.
#[pyclass]
struct WhepSession {
    endpoint_url: String,
    bearer_token: Option<String>,
    session: Option<WhepPlayingSession>,
}

#[pymethods]
impl WhepSession {
    #[new]
    #[pyo3(signature = (endpoint_url, bearer_token=None))]
    fn new(endpoint_url: String, bearer_token: Option<String>) -> Self {
        Self {
            endpoint_url,
            bearer_token,
            session: None,
        }
    }

    /// Connect and begin draining. Called from the processor's own thread, not
    /// from `setup()`, so a relay outage cannot spend the start-up budget.
    fn connect(&mut self, python: Python<'_>) -> PyResult<()> {
        if self.session.is_some() {
            return Err(WebRtcExtensionError::Refused {
                what: "this WHEP session is already connected".to_owned(),
            }
            .into());
        }
        let endpoint_url = self.endpoint_url.clone();
        let bearer_token = self.bearer_token.clone();

        let connected = python.detach(|| {
            transport_stack::transport_runtime()?
                .block_on(WhepPlayingSession::connect(endpoint_url, bearer_token))
        })?;
        self.session = Some(connected);
        Ok(())
    }

    /// The next assembled access unit or Opus packet, or `None` if none arrived
    /// within `timeout_ms` — which is how the reading thread stays responsive
    /// to a stop it has been asked for.
    fn next_media(&mut self, python: Python<'_>, timeout_ms: u64) -> PyResult<Option<Py<PyAny>>> {
        let Some(session) = self.session.as_mut() else {
            return Err(WebRtcExtensionError::NotConnected { protocol: "WHEP" }.into());
        };

        let received = python.detach(|| {
            transport_stack::transport_runtime().map(|runtime| {
                runtime.block_on(session.next_media(Duration::from_millis(timeout_ms)))
            })
        })?;

        Ok(match received {
            Some(ReceivedMedia::Video(access_unit)) => {
                Some(Py::new(python, PlayedVideoAccessUnit::from(access_unit))?.into_any())
            }
            Some(ReceivedMedia::Audio(packet)) => {
                Some(Py::new(python, PlayedOpusPacket::from(packet))?.into_any())
            }
            None => None,
        })
    }

    /// Close the peer connection and DELETE the session.
    fn close(&mut self, python: Python<'_>) -> PyResult<()> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        python.detach(|| {
            if let Ok(runtime) = transport_stack::transport_runtime() {
                runtime.block_on(session.close());
            }
        });
        Ok(())
    }

    #[getter]
    fn is_connected(&self) -> bool {
        self.session.is_some()
    }
}

/// One access unit off a WHEP stream, with every key the video wire contract
/// requires taken from the stream itself.
#[pyclass]
struct PlayedVideoAccessUnit {
    inner: ReceivedVideoAccessUnit,
}

impl From<ReceivedVideoAccessUnit> for PlayedVideoAccessUnit {
    fn from(inner: ReceivedVideoAccessUnit) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl PlayedVideoAccessUnit {
    #[getter]
    fn bitstream<'python>(&self, python: Python<'python>) -> Bound<'python, PyBytes> {
        PyBytes::new(python, &self.inner.annex_b_access_unit)
    }
    #[getter]
    fn is_sync_point(&self) -> bool {
        self.inner.is_sync_point
    }
    #[getter]
    fn group_index(&self) -> u64 {
        self.inner.group_index
    }
    #[getter]
    fn sequence_index(&self) -> u64 {
        self.inner.sequence_index
    }
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width
    }
    #[getter]
    fn height(&self) -> u32 {
        self.inner.height
    }
    #[getter]
    fn timestamp_ns(&self) -> i64 {
        self.inner.timestamp_ns
    }
    /// The bag's `color` sub-map, or `None` where the stream's VUI described no
    /// colour at all. An axis the bag vocabulary does not model is left out
    /// rather than guessed at.
    #[getter]
    fn color(&self) -> Option<std::collections::HashMap<&'static str, &'static str>> {
        let color = self.inner.color.as_ref()?;
        let mut axes = std::collections::HashMap::new();
        for (name, value) in [
            ("primaries", color.primaries),
            ("transfer", color.transfer),
            ("matrix", color.matrix),
            ("range", color.range),
        ] {
            if let Some(value) = value {
                axes.insert(name, value);
            }
        }
        Some(axes)
    }
}

/// One Opus packet off a WHEP stream, described by the packet's own TOC byte.
#[pyclass]
struct PlayedOpusPacket {
    inner: ReceivedOpusPacket,
}

impl From<ReceivedOpusPacket> for PlayedOpusPacket {
    fn from(inner: ReceivedOpusPacket) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl PlayedOpusPacket {
    #[getter]
    fn bitstream<'python>(&self, python: Python<'python>) -> Bound<'python, PyBytes> {
        PyBytes::new(python, &self.inner.opus_packet)
    }
    #[getter]
    fn group_index(&self) -> u64 {
        self.inner.group_index
    }
    #[getter]
    fn sequence_index(&self) -> u64 {
        self.inner.sequence_index
    }
    #[getter]
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate
    }
    #[getter]
    fn channels(&self) -> u32 {
        self.inner.channels
    }
    #[getter]
    fn sample_count(&self) -> u32 {
        self.inner.sample_count
    }
    #[getter]
    fn pre_skip(&self) -> u32 {
        self.inner.pre_skip
    }
    #[getter]
    fn timestamp_ns(&self) -> i64 {
        self.inner.timestamp_ns
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(bring_up_the_transport_stack, module)?)?;
    module.add_class::<WhipSession>()?;
    module.add_class::<WhepSession>()?;
    module.add_class::<PlayedVideoAccessUnit>()?;
    module.add_class::<PlayedOpusPacket>()?;
    Ok(())
}
