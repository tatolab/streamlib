// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-webrtc` — the module the wheel's two
//! `@node` classes import as `tatolab.webrtc._native`.
//!
//! The engine never calls anything here. A processor extension's per-frame work
//! is its own package's Rust, reached directly from its own Python.

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub mod error;
pub mod h264_rtp_depacketiser;
pub mod h264_sequence_parameter_set;
mod h264_test_bitstreams;
pub mod hang_legacy_container_media_frame;
pub mod http_signalling;
mod http_test_responder;
pub mod monotonic_clock;
pub mod opus_identification_header;
pub mod opus_packet;
pub mod received_media_assembly;
pub mod session_description;
pub mod transport_stack;
pub mod webrtc_peer_connection;
pub mod whep_session;
mod whip_publish_loopback;
pub mod whip_session;

use crate::error::WebRtcExtensionError;
use crate::opus_identification_header::hex_encoded_opus_identification_header_for_rtp_stream;
use crate::received_media_assembly::{ReceivedOpusPacket, ReceivedVideoAccessUnit};
use crate::whep_session::{ReceivedMedia, WhepPlayingSession};
use crate::whip_session::{PublishedMediaSet, WhipPublishingSession};

/// Only the first frame of a stream, so that no two frames share a presentation
/// timestamp. Every later one advances by the gap the messages' own stamps state.
const FIRST_FRAME_NOMINAL_DURATION: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// Publishes encoded media to a WHIP endpoint.
///
/// One thread owns a session for its whole life — the helper dispatches
/// `process`, `stop` and `teardown` on one thread — and that, not the lock, is
/// what makes the lifecycle safe. The lock is taken inside `detach` in every
/// method regardless, because taking it with the GIL held while another thread
/// holds it across a detached network call is a deadlock and not a wait: the
/// detached thread cannot re-attach to release it.
#[pyclass]
struct WhipSession {
    endpoint_url: String,
    bearer_token: Option<String>,
    publishing: Mutex<PublishingState>,
}

#[derive(Default)]
struct PublishingState {
    session: Option<WhipPublishingSession>,
    previous_video_stamp_ns: Option<i64>,
}

#[pymethods]
impl WhipSession {
    /// Constructs without connecting: opening the session is what the first
    /// media frame does, so a relay round trip never runs inside `setup()`.
    #[new]
    #[pyo3(signature = (endpoint_url, bearer_token=None))]
    fn new(endpoint_url: String, bearer_token: Option<String>) -> Self {
        Self {
            endpoint_url,
            bearer_token,
            publishing: Mutex::new(PublishingState::default()),
        }
    }

    /// Offer the media set the publisher's links settled, and set the answer.
    #[pyo3(signature = (*, video, audio, audio_channels=None))]
    fn connect(
        &self,
        python: Python<'_>,
        video: bool,
        audio: bool,
        audio_channels: Option<u32>,
    ) -> PyResult<()> {
        let endpoint_url = self.endpoint_url.clone();
        let bearer_token = self.bearer_token.clone();
        let media = PublishedMediaSet {
            video,
            audio,
            audio_channels,
        };

        python.detach(|| {
            let mut publishing = self.locked_state()?;
            if publishing.session.is_some() {
                return Err(WebRtcExtensionError::Refused {
                    what: "this WHIP session is already connected".to_owned(),
                });
            }
            publishing.session = Some(transport_stack::transport_runtime()?.block_on(
                WhipPublishingSession::connect(endpoint_url, bearer_token, media),
            )?);
            Ok(())
        })?;
        Ok(())
    }

    /// Send the Annex-B access unit one media frame carries after its varint
    /// timestamp, on the RTP clock `timestamp_ns` places it — the envelope's
    /// stamp, which a local reader is promised; the varint's is not read.
    ///
    /// The RTP clock advances by the gap to the *previous* frame, because the
    /// payloader applies a sample's duration to the frame after it and a
    /// publisher cannot see the next frame without delaying this one. The
    /// stream's rate is exact and its RTP numbering trails real time by one
    /// frame, so video presents roughly one frame interval later than audio,
    /// whose duration comes from each packet's own sample count and is exact.
    fn write_video_media_frame(
        &self,
        python: Python<'_>,
        media_frame: &[u8],
        timestamp_ns: i64,
    ) -> PyResult<()> {
        // Copied while the GIL is held: the slice borrows Python's buffer.
        let media_frame = bytes::Bytes::copy_from_slice(media_frame);

        python.detach(|| {
            let mut publishing = self.locked_state()?;
            let duration = match publishing.previous_video_stamp_ns {
                Some(previous) => {
                    Duration::from_nanos(timestamp_ns.saturating_sub(previous).max(0) as u64)
                }
                None => FIRST_FRAME_NOMINAL_DURATION,
            };
            let session = connected(publishing.session.as_ref())?;
            transport_stack::transport_runtime()?
                .block_on(session.write_video_media_frame(media_frame, duration))?;
            // Only after the write landed: a frame the track never saw must not
            // move the anchor, or the clock stays short by that frame's gap for
            // the rest of the stream.
            publishing.previous_video_stamp_ns = Some(timestamp_ns);
            Ok::<(), WebRtcExtensionError>(())
        })?;
        Ok(())
    }

    /// Send the Opus packet one media frame carries after its varint
    /// timestamp, its RTP advance taken from the packet's own TOC rather than
    /// from an assumed 20 ms frame.
    fn write_audio_media_frame(&self, python: Python<'_>, media_frame: &[u8]) -> PyResult<()> {
        // Copied while the GIL is held: the slice borrows Python's buffer.
        let media_frame = bytes::Bytes::copy_from_slice(media_frame);

        python.detach(|| {
            let publishing = self.locked_state()?;
            let session = connected(publishing.session.as_ref())?;
            transport_stack::transport_runtime()?
                .block_on(session.write_audio_media_frame(media_frame))
        })?;
        Ok(())
    }

    /// Close the peer connection and DELETE the session.
    fn close(&self, python: Python<'_>) -> PyResult<()> {
        python.detach(|| {
            let taken = self.locked_state()?.session.take();
            if let Some(session) = taken
                && let Ok(runtime) = transport_stack::transport_runtime()
            {
                runtime.block_on(session.close());
            }
            Ok::<(), WebRtcExtensionError>(())
        })?;
        Ok(())
    }

    #[getter]
    fn is_connected(&self, python: Python<'_>) -> PyResult<bool> {
        Ok(python.detach(|| self.locked_state().map(|state| state.session.is_some()))?)
    }
}

impl WhipSession {
    fn locked_state(&self) -> Result<MutexGuard<'_, PublishingState>, WebRtcExtensionError> {
        self.publishing
            .lock()
            .map_err(|_| WebRtcExtensionError::Transport {
                what: "the WHIP session's state was left poisoned by an earlier panic".to_owned(),
            })
    }
}

fn connected(
    session: Option<&WhipPublishingSession>,
) -> Result<&WhipPublishingSession, WebRtcExtensionError> {
    session.ok_or(WebRtcExtensionError::NotConnected { protocol: "WHIP" })
}

/// Plays encoded media back from a WHEP endpoint.
///
/// The player's reading thread owns the session for its whole life — it
/// connects, drains and closes it — so no two threads reach one session. The
/// lock is taken inside `detach` in every method regardless, because taking it
/// with the GIL held while another thread holds it across a detached network
/// call is a deadlock and not a wait: the detached thread cannot re-attach to
/// release it.
#[pyclass]
struct WhepSession {
    endpoint_url: String,
    bearer_token: Option<String>,
    session: Mutex<Option<WhepPlayingSession>>,
}

#[pymethods]
impl WhepSession {
    #[new]
    #[pyo3(signature = (endpoint_url, bearer_token=None))]
    fn new(endpoint_url: String, bearer_token: Option<String>) -> Self {
        Self {
            endpoint_url,
            bearer_token,
            session: Mutex::new(None),
        }
    }

    /// Connect and begin draining. Called from the processor's own thread, not
    /// from `setup()`, so a relay outage cannot spend the start-up budget.
    fn connect(&self, python: Python<'_>) -> PyResult<()> {
        let endpoint_url = self.endpoint_url.clone();
        let bearer_token = self.bearer_token.clone();

        python.detach(|| {
            let mut session = self.locked_session()?;
            if session.is_some() {
                return Err(WebRtcExtensionError::Refused {
                    what: "this WHEP session is already connected".to_owned(),
                });
            }
            *session = Some(
                transport_stack::transport_runtime()?
                    .block_on(WhepPlayingSession::connect(endpoint_url, bearer_token))?,
            );
            Ok(())
        })?;
        Ok(())
    }

    /// The next assembled access unit or Opus packet, or `None` if none arrived
    /// within `timeout_ms` — which is how the reading thread stays responsive
    /// to a stop it has been asked for.
    fn next_media(&self, python: Python<'_>, timeout_ms: u64) -> PyResult<Option<Py<PyAny>>> {
        let received = python.detach(|| {
            let mut locked = self.locked_session()?;
            let session = locked
                .as_mut()
                .ok_or(WebRtcExtensionError::NotConnected { protocol: "WHEP" })?;
            Ok::<_, WebRtcExtensionError>(
                transport_stack::transport_runtime()?
                    .block_on(session.next_media(Duration::from_millis(timeout_ms))),
            )
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
    fn close(&self, python: Python<'_>) -> PyResult<()> {
        python.detach(|| {
            let taken = self.locked_session()?.take();
            if let Some(session) = taken
                && let Ok(runtime) = transport_stack::transport_runtime()
            {
                runtime.block_on(session.close());
            }
            Ok::<(), WebRtcExtensionError>(())
        })?;
        Ok(())
    }

    #[getter]
    fn is_connected(&self, python: Python<'_>) -> PyResult<bool> {
        Ok(python.detach(|| self.locked_session().map(|session| session.is_some()))?)
    }
}

impl WhepSession {
    fn locked_session(
        &self,
    ) -> Result<MutexGuard<'_, Option<WhepPlayingSession>>, WebRtcExtensionError> {
        self.session
            .lock()
            .map_err(|_| WebRtcExtensionError::Transport {
                what: "the WHEP session's state was left poisoned by an earlier panic".to_owned(),
            })
    }
}

/// One access unit off a WHEP stream, with what its port's catalog entry
/// states taken from the stream itself.
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
    /// The access unit as one media frame, behind the varint of its stamp.
    #[getter]
    fn media_frame<'python>(&self, python: Python<'python>) -> PyResult<Bound<'python, PyBytes>> {
        Ok(PyBytes::new(python, &self.inner.media_frame_bytes()?))
    }
    #[getter]
    fn is_sync_point(&self) -> bool {
        self.inner.is_sync_point
    }
    #[getter]
    fn codec(&self) -> String {
        self.inner.sequence_parameter_set.avc1_codec_string()
    }
    #[getter]
    fn coded_width(&self) -> u32 {
        self.inner.sequence_parameter_set.coded_width
    }
    #[getter]
    fn coded_height(&self) -> u32 {
        self.inner.sequence_parameter_set.coded_height
    }
    #[getter]
    fn timestamp_ns(&self) -> i64 {
        self.inner.timestamp_ns
    }
}

/// One Opus packet off a WHEP stream, with what its port's catalog entry
/// states taken from the stream itself.
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
    /// The packet as one media frame, behind the varint of its stamp.
    #[getter]
    fn media_frame<'python>(&self, python: Python<'python>) -> PyResult<Bound<'python, PyBytes>> {
        Ok(PyBytes::new(python, &self.inner.media_frame_bytes()?))
    }
    #[getter]
    fn channels(&self) -> u8 {
        self.inner.channel_layout.channel_count()
    }
    #[getter]
    fn opus_identification_header_hex(&self) -> String {
        hex_encoded_opus_identification_header_for_rtp_stream(self.inner.channel_layout)
    }
    #[getter]
    fn timestamp_ns(&self) -> i64 {
        self.inner.timestamp_ns
    }
}

/// Nanoseconds on the clock this wheel stamps media frames with, which is the
/// engine's — so a reading compares with `tatolab.stream.monotonic_now_ns()`.
#[pyfunction(name = "monotonic_now_ns")]
fn this_wheels_monotonic_now_ns() -> i64 {
    monotonic_clock::monotonic_now_ns()
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(this_wheels_monotonic_now_ns, module)?)?;
    module.add_class::<WhipSession>()?;
    module.add_class::<WhepSession>()?;
    module.add_class::<PlayedVideoAccessUnit>()?;
    module.add_class::<PlayedOpusPacket>()?;
    Ok(())
}
