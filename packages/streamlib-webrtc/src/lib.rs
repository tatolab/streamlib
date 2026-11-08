// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-webrtc` — the module the wheel's two
//! `@node.process` classes import as `tatolab.webrtc._native`.
//!
//! The engine never calls anything here. A processor extension's per-frame work
//! is its own package's Rust: the publisher's is reached from its own Python,
//! and the player's runs on a thread of its own, writing its node's ports
//! through the module host table.

use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::Duration;
use tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to;
use tatolab_module_sdk::{
    ModuleHost, ModuleHostLogLevel, ModuleHostOutputPort, packets_unit_payload_descriptor,
};

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
pub mod whep_player;
pub mod whep_session;
mod whip_publish_loopback;
pub mod whip_session;

use crate::error::WebRtcExtensionError;
use crate::whep_player::{
    ENCODED_AUDIO_OUTPUT_PORT, ENCODED_VIDEO_OUTPUT_PORT, MessageTheWhepPlayerWrites,
    WhepPlayerCatalogEntry, WhepPlayerConnectedSession, WhepPlayerLogLevel, WhepPlayerOutputPort,
    WhepPlayerOutputPorts, WhepPlayerSessionDialer, WhepPlayerStopSignal, play_until_stopped,
};
use crate::whep_session::WhepPlayingSession;
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
        media_frame: PyBuffer<u8>,
        timestamp_ns: i64,
    ) -> PyResult<()> {
        // Copied while the GIL is held: the slice borrows Python's buffer.
        let media_frame = bytes::Bytes::copy_from_slice(bytes_of_a_contiguous_buffer(
            &media_frame,
            "media_frame",
        )?);

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
    fn write_audio_media_frame(
        &self,
        python: Python<'_>,
        media_frame: PyBuffer<u8>,
    ) -> PyResult<()> {
        // Copied while the GIL is held: the slice borrows Python's buffer.
        let media_frame = bytes::Bytes::copy_from_slice(bytes_of_a_contiguous_buffer(
            &media_frame,
            "media_frame",
        )?);

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

/// `WhepPlayer`'s reading thread, started over the module host table its
/// node lent the two output ports through.
///
/// The thread dials, drains and redials on its own; every frame it receives is
/// written from that thread with no Python attached, and its log records reach
/// the node's log through the table.
#[pyclass]
struct WhepPlayerThread {
    stop_signal: Arc<WhepPlayerStopSignal>,
    reading_thread: Mutex<Option<ReadingThreadEnding>>,
}

struct ReadingThreadEnding {
    join_handle: std::thread::JoinHandle<()>,
    ended: mpsc::Receiver<()>,
}

#[pymethods]
impl WhepPlayerThread {
    /// Start playing `endpoint_url` onto the ports `module_host_table` lends:
    /// `encoded_video` and `encoded_audio`.
    #[new]
    #[pyo3(signature = (module_host_table, endpoint_url, bearer_token=None))]
    fn start(
        module_host_table: &Bound<'_, PyAny>,
        endpoint_url: String,
        bearer_token: Option<String>,
    ) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        let output_ports = WhepPlayerOutputPortsLentByTheModuleHost::open(&module_host)?;
        forward_this_modules_tracing_records_to(&module_host);
        let module_host_for_the_stop_flag = module_host.clone();
        let stop_signal = Arc::new(WhepPlayerStopSignal::also_set_when(Box::new(move || {
            module_host_for_the_stop_flag.stop_requested()
        })));
        let dialer = WhepSessionDialer {
            endpoint_url,
            bearer_token,
        };
        let (thread_ended, ended) = mpsc::channel();
        let stop_signal_of_the_thread = Arc::clone(&stop_signal);
        let join_handle = std::thread::Builder::new()
            .name("whep-player".to_owned())
            .spawn(move || {
                play_until_stopped(
                    &dialer,
                    &output_ports,
                    &stop_signal_of_the_thread,
                    &mut |delay| stop_signal_of_the_thread.wait_unless_stopped(delay),
                );
                let _ = thread_ended.send(());
            })
            .map_err(|spawn_failure| WebRtcExtensionError::Transport {
                what: format!("the WHEP player's reading thread did not start: {spawn_failure}"),
            })?;
        Ok(Self {
            stop_signal,
            reading_thread: Mutex::new(Some(ReadingThreadEnding { join_handle, ended })),
        })
    }

    /// Ask the thread to stop and wait up to `timeout_seconds` for it: `True`
    /// when it ended. A thread still inside a connect outlives the wait and
    /// closes its session itself when the connect returns.
    fn stop_and_join(&self, python: Python<'_>, timeout_seconds: f64) -> bool {
        self.stop_signal.set();
        let timeout = Duration::try_from_secs_f64(timeout_seconds).unwrap_or(Duration::ZERO);
        python.detach(|| {
            let Ok(mut reading_thread) = self.reading_thread.lock() else {
                return false;
            };
            let Some(ending) = reading_thread.take() else {
                return true;
            };
            match ending.ended.recv_timeout(timeout) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = ending.join_handle.join();
                    true
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    *reading_thread = Some(ending);
                    false
                }
            }
        })
    }
}

/// The two output ports and the log the node host lent this wheel.
struct WhepPlayerOutputPortsLentByTheModuleHost {
    module_host: ModuleHost,
    encoded_video: ModuleHostOutputPort,
    encoded_audio: ModuleHostOutputPort,
}

impl WhepPlayerOutputPortsLentByTheModuleHost {
    fn open(module_host: &ModuleHost) -> Result<Self, tatolab_module_sdk::ModuleHostError> {
        Ok(Self {
            module_host: module_host.clone(),
            encoded_video: module_host.open_output_port(ENCODED_VIDEO_OUTPUT_PORT)?,
            encoded_audio: module_host.open_output_port(ENCODED_AUDIO_OUTPUT_PORT)?,
        })
    }

    fn output_port(&self, port: WhepPlayerOutputPort) -> &ModuleHostOutputPort {
        match port {
            WhepPlayerOutputPort::EncodedVideo => &self.encoded_video,
            WhepPlayerOutputPort::EncodedAudio => &self.encoded_audio,
        }
    }
}

impl WhepPlayerOutputPorts for WhepPlayerOutputPortsLentByTheModuleHost {
    fn write(
        &self,
        port: WhepPlayerOutputPort,
        media_frame: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
    ) -> Result<(), String> {
        let packets_unit = packets_unit_payload_descriptor(port.packets_unit_codec_name())
            .map_err(|refusal| refusal.to_string())?;
        self.output_port(port)
            .write_described(media_frame, Some(timestamp_ns), starts_group, &packets_unit)
            .map_err(|refusal| refusal.to_string())
    }

    fn set_catalog_entry(
        &self,
        port: WhepPlayerOutputPort,
        catalog_entry: &WhepPlayerCatalogEntry,
    ) -> Result<(), String> {
        self.output_port(port)
            .set_catalog_entry(catalog_entry.section(), &catalog_entry.entry_json())
            .map_err(|refusal| refusal.to_string())
    }

    fn log(&self, level: WhepPlayerLogLevel, message: &str) {
        let level = match level {
            WhepPlayerLogLevel::Error => ModuleHostLogLevel::Error,
            WhepPlayerLogLevel::Warn => ModuleHostLogLevel::Warn,
            WhepPlayerLogLevel::Info => ModuleHostLogLevel::Info,
        };
        self.module_host
            .log(level, "tatolab_webrtc::whep_player", message);
    }
}

/// Dials a fresh WHEP session per attempt.
struct WhepSessionDialer {
    endpoint_url: String,
    bearer_token: Option<String>,
}

impl WhepPlayerSessionDialer for WhepSessionDialer {
    fn dial(&self) -> Result<Box<dyn WhepPlayerConnectedSession>, WebRtcExtensionError> {
        let session = transport_stack::transport_runtime()?.block_on(
            WhepPlayingSession::connect(self.endpoint_url.clone(), self.bearer_token.clone()),
        )?;
        Ok(Box::new(ConnectedWhepPlayingSession { session }))
    }
}

struct ConnectedWhepPlayingSession {
    session: WhepPlayingSession,
}

impl WhepPlayerConnectedSession for ConnectedWhepPlayingSession {
    fn next_message(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<MessageTheWhepPlayerWrites>, WebRtcExtensionError> {
        transport_stack::transport_runtime()?
            .block_on(self.session.next_media_unless_the_session_ended(timeout))?
            .map(MessageTheWhepPlayerWrites::of_received_media)
            .transpose()
    }

    fn close(&mut self) {
        if let Ok(runtime) = transport_stack::transport_runtime() {
            runtime.block_on(self.session.close());
        }
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
    module.add_class::<WhepPlayerThread>()?;
    Ok(())
}

/// The bytes of a C-contiguous buffer — `bytes`, a read's `PortPayloadBorrow`,
/// a `memoryview` — borrowed while `buffer` lives.
fn bytes_of_a_contiguous_buffer<'buffer>(
    buffer: &'buffer PyBuffer<u8>,
    argument_name: &str,
) -> PyResult<&'buffer [u8]> {
    if !buffer.is_c_contiguous() {
        return Err(PyValueError::new_err(format!(
            "{argument_name} must be a C-contiguous buffer"
        )));
    }
    // SAFETY: a C-contiguous buffer spans `len_bytes` bytes from `buf_ptr`,
    // and its exporter keeps them alive and unmoved while `buffer` lives.
    Ok(unsafe { std::slice::from_raw_parts(buffer.buf_ptr() as *const u8, buffer.len_bytes()) })
}
