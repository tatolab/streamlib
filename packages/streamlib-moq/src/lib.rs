// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `streamlib-moq` — the module the wheel's two
//! `@processor` classes import as `streamlib_moq._native`.
//!
//! The engine never calls anything here. A processor extension's per-frame work
//! is its own package's Rust, reached directly from its own Python.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

mod annex_b_access_unit;
mod cmaf_fragment;
mod cmaf_init_segment;
mod cmaf_init_segment_reader;
mod cmaf_sample_entry;
mod cmaf_track_timeline;
mod delivery_deadline;
mod encoded_media_sample;
mod error;
mod monotonic_clock;
mod moq_broadcast_catalog;
mod moq_broadcast_publisher;
mod moq_broadcast_subscriber;
mod moq_relay_config;
mod moq_session;
mod moq_track_sample;
mod streamlib_bag_object;
mod transport_stack;

use crate::delivery_deadline::{MoqPublisherDeliveryDeadline, UplinkBacklogOnOneTrack};
use crate::encoded_media_sample::{EncodedAudioPacket, EncodedMediaSample, EncodedVideoAccessUnit};
use crate::error::MoqExtensionError;
use crate::monotonic_clock::monotonic_now_ns;
use crate::moq_broadcast_publisher::{
    MoqBroadcastPublisher, MoqContainerFormat, WhatBecameOfOnePublishedBag,
};
use crate::moq_broadcast_subscriber::{MoqBroadcastSubscriber, ReceivedTrackSample};
use crate::moq_relay_config::MoqRelayConfig;
use crate::moq_session::QuicUplinkReadings;
use crate::moq_track_sample::{DataTrackObject, MoqTrackSample};

/// Bring up the tokio runtime and the TLS provider this wheel's sessions share.
#[pyfunction]
fn bring_up_the_transport_stack() -> PyResult<()> {
    transport_stack::bring_up()?;
    Ok(())
}

/// Publishes encoded media to a MoQ broadcast.
///
/// One thread owns a session for its whole life — the helper dispatches
/// `process`, `stop` and `teardown` on one thread — and that, not the lock, is
/// what makes the lifecycle safe. The lock is taken inside `detach` in every
/// method regardless, because taking it with the GIL held while another thread
/// holds it across a detached network call is a deadlock and not a wait: the
/// detached thread cannot re-attach to release it.
#[pyclass]
struct MoqBroadcastPublishingSession {
    publisher: Mutex<MoqBroadcastPublisher>,
}

#[pymethods]
impl MoqBroadcastPublishingSession {
    /// Constructs without connecting: opening the session is what the first
    /// bag does, so a relay round trip never runs inside `setup()`.
    #[new]
    #[pyo3(signature = (relay_url, broadcast, container_format, delivery_deadline_ms=None))]
    fn new(
        relay_url: String,
        broadcast: String,
        container_format: &str,
        delivery_deadline_ms: Option<u64>,
    ) -> PyResult<Self> {
        let container_format = MoqContainerFormat::of_wire_name(container_format)?;
        let config = MoqRelayConfig {
            relay_endpoint_url: relay_url,
            broadcast_path: broadcast,
        };
        Ok(Self {
            publisher: Mutex::new(MoqBroadcastPublisher::new(
                config,
                container_format,
                MoqPublisherDeliveryDeadline::of_optional_milliseconds(delivery_deadline_ms),
            )),
        })
    }

    /// Fix the broadcast's tracks, in the order their links were wired, under
    /// the names the app chose for them or, absent those, their links' own.
    #[pyo3(signature = (inbound_link_names, track_names=None))]
    fn declare_tracks(
        &self,
        python: Python<'_>,
        inbound_link_names: Vec<String>,
        track_names: Option<Vec<String>>,
    ) -> PyResult<()> {
        python.detach(|| {
            self.locked_publisher()?
                .declare_tracks(inbound_link_names, track_names)
        })?;
        Ok(())
    }

    /// Publish one access unit on the track that link owns. `true` when it
    /// reaches the transport; `false` when the delivery deadline shed it.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        inbound_link_name, codec, annex_b_access_unit, is_sync_point, group_index,
        sequence_index, width, height, color, timestamp_ns
    ))]
    fn publish_video_access_unit(
        &self,
        python: Python<'_>,
        inbound_link_name: &str,
        codec: String,
        annex_b_access_unit: &[u8],
        is_sync_point: bool,
        group_index: u64,
        sequence_index: u64,
        width: u32,
        height: u32,
        color: Option<BTreeMap<String, String>>,
        timestamp_ns: i64,
    ) -> PyResult<bool> {
        // Copied while the GIL is held: the slice borrows Python's buffer.
        let sample = EncodedMediaSample::VideoAccessUnit(EncodedVideoAccessUnit {
            codec,
            annex_b_access_unit: bytes::Bytes::copy_from_slice(annex_b_access_unit),
            is_sync_point,
            group_index,
            sequence_index,
            width,
            height,
            color,
            timestamp_ns,
        });
        self.publish_media(python, inbound_link_name, sample)
    }

    /// Publish one Opus packet on the track that link owns. `true` when it
    /// reaches the transport; `false` when the delivery deadline shed it.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        inbound_link_name, opus_packet, is_sync_point, group_index, sequence_index,
        sample_rate, channels, sample_count, pre_skip, timestamp_ns
    ))]
    fn publish_audio_packet(
        &self,
        python: Python<'_>,
        inbound_link_name: &str,
        opus_packet: &[u8],
        is_sync_point: bool,
        group_index: u64,
        sequence_index: u64,
        sample_rate: u32,
        channels: u32,
        sample_count: u32,
        pre_skip: u32,
        timestamp_ns: i64,
    ) -> PyResult<bool> {
        let sample = EncodedMediaSample::AudioPacket(EncodedAudioPacket {
            codec: "opus".to_owned(),
            opus_packet: bytes::Bytes::copy_from_slice(opus_packet),
            is_sync_point,
            group_index,
            sequence_index,
            sample_rate,
            channels,
            sample_count,
            pre_skip,
            timestamp_ns,
        });
        self.publish_media(python, inbound_link_name, sample)
    }

    /// Publish one data object — the envelope Python built and encoded — on
    /// the track that link owns. Written as the object payload whole; nothing
    /// in it is parsed here.
    fn publish_data_object(
        &self,
        python: Python<'_>,
        inbound_link_name: &str,
        object_bytes: &[u8],
    ) -> PyResult<()> {
        let sample = MoqTrackSample::DataObject(DataTrackObject {
            envelope_bytes: bytes::Bytes::copy_from_slice(object_bytes),
        });
        // The deadline never sheds a data object, so the outcome carries
        // nothing the caller does not already know.
        self.publish(python, inbound_link_name, sample)?;
        Ok(())
    }

    /// Finish every open group and drop the connection.
    ///
    /// Returns what a broadcast that never became playable threw away, or
    /// `None` when nothing was held. This crate's `tracing` events reach no
    /// dispatcher inside a helper process, so a loss reported only that way
    /// would be reported to nobody; the caller says it through the log channel
    /// the helper does install.
    fn close(&self, python: Python<'_>) -> PyResult<Option<String>> {
        let discarded = python.detach(|| {
            let mut publisher = self.locked_publisher()?;
            let discarded = match transport_stack::transport_runtime() {
                Ok(runtime) => runtime.block_on(publisher.close()),
                Err(_) => None,
            };
            Ok::<_, MoqExtensionError>(discarded)
        })?;
        Ok(discarded.map(|discarded| {
            format!(
                "{} held samples ({} bytes) were discarded unwritten: the broadcast never \
                 became playable because {}",
                discarded.held_sample_count,
                discarded.held_byte_count,
                discarded.why_the_broadcast_never_opened
            )
        }))
    }

    #[getter]
    fn is_connected(&self, python: Python<'_>) -> PyResult<bool> {
        Ok(python.detach(|| self.locked_publisher().map(|open| open.is_connected()))?)
    }

    /// What the delivery deadline has shed so far: one
    /// `(inbound_link_name, objects, bytes)` per link that shed anything.
    ///
    /// Empty is a broadcast that has dropped nothing, which the caller says
    /// out loud rather than leaving unsaid.
    fn objects_the_delivery_deadline_shed(
        &self,
        python: Python<'_>,
    ) -> PyResult<Vec<(String, u64, u64)>> {
        let shed = python.detach(|| {
            Ok::<_, MoqExtensionError>(
                self.locked_publisher()?
                    .objects_the_delivery_deadline_shed(),
            )
        })?;
        Ok(shed
            .into_iter()
            .map(|track| {
                (
                    track.inbound_link_name,
                    track.objects_shed,
                    track.bytes_shed,
                )
            })
            .collect())
    }

    /// The uplink backlog per inbound link — every declared link, a zero
    /// included: what stands unforwarded now, the sheds the backlog began,
    /// and the superseded groups abandoned for it with what those never
    /// delivered.
    fn uplink_backlog_by_track(
        &self,
        python: Python<'_>,
    ) -> PyResult<Vec<MoqPublisherUplinkBacklogOnOneTrack>> {
        let backlog = python.detach(|| {
            Ok::<_, MoqExtensionError>(self.locked_publisher()?.uplink_backlog_by_track())
        })?;
        Ok(backlog
            .into_iter()
            .map(MoqPublisherUplinkBacklogOnOneTrack::from)
            .collect())
    }

    /// What the QUIC connection under the session reports about the uplink
    /// path right now, or `None` before the session connects.
    fn quic_uplink_readings(
        &self,
        python: Python<'_>,
    ) -> PyResult<Option<MoqPublisherQuicUplinkReadings>> {
        let readings = python.detach(|| {
            Ok::<_, MoqExtensionError>(self.locked_publisher()?.quic_uplink_readings())
        })?;
        Ok(readings.map(MoqPublisherQuicUplinkReadings::from))
    }
}

/// One inbound link's uplink backlog, as the publisher reads it: what is
/// behind now and what the backlog has cost so far.
#[pyclass(frozen, get_all)]
struct MoqPublisherUplinkBacklogOnOneTrack {
    inbound_link_name: String,
    unforwarded_objects: Option<u64>,
    sheds_the_backlog_began: u64,
    groups_abandoned: u64,
    objects_abandoned: u64,
    bytes_abandoned: u64,
}

impl From<UplinkBacklogOnOneTrack> for MoqPublisherUplinkBacklogOnOneTrack {
    fn from(backlog: UplinkBacklogOnOneTrack) -> Self {
        Self {
            inbound_link_name: backlog.inbound_link_name,
            unforwarded_objects: backlog.unforwarded_objects,
            sheds_the_backlog_began: backlog.sheds_the_backlog_began,
            groups_abandoned: backlog.groups_abandoned,
            objects_abandoned: backlog.objects_abandoned,
            bytes_abandoned: backlog.bytes_abandoned,
        }
    }
}

/// What the QUIC path under the publishing session reports: its round trip,
/// its congestion window, and what it has lost.
#[pyclass(frozen, get_all)]
struct MoqPublisherQuicUplinkReadings {
    round_trip_time_ms: f64,
    congestion_window_bytes: u64,
    lost_packets: u64,
    congestion_events: u64,
}

impl From<QuicUplinkReadings> for MoqPublisherQuicUplinkReadings {
    fn from(readings: QuicUplinkReadings) -> Self {
        Self {
            round_trip_time_ms: readings.round_trip_time.as_secs_f64() * 1_000.0,
            congestion_window_bytes: readings.congestion_window_bytes,
            lost_packets: readings.lost_packets,
            congestion_events: readings.congestion_events,
        }
    }
}

impl MoqBroadcastPublishingSession {
    /// `true` when the sample reaches the transport; `false` when the delivery
    /// deadline shed it.
    fn publish_media(
        &self,
        python: Python<'_>,
        inbound_link_name: &str,
        sample: EncodedMediaSample,
    ) -> PyResult<bool> {
        let became = self.publish(python, inbound_link_name, sample.into())?;
        Ok(became == WhatBecameOfOnePublishedBag::ReachesTheTransport)
    }

    fn publish(
        &self,
        python: Python<'_>,
        inbound_link_name: &str,
        sample: MoqTrackSample,
    ) -> PyResult<WhatBecameOfOnePublishedBag> {
        let became = python.detach(|| {
            let mut publisher = self.locked_publisher()?;
            // Read here rather than inside the planner: one reading covers the
            // whole of one bag's decision, and a test can plan against a stated
            // instant. Read after the lock rather than before `detach`, or the
            // first bag's age misses the relay connect `publish` opens with.
            let now_ns = monotonic_now_ns();
            transport_stack::transport_runtime()?.block_on(publisher.publish(
                inbound_link_name,
                sample,
                now_ns,
            ))
        })?;
        Ok(became)
    }

    fn locked_publisher(&self) -> Result<MutexGuard<'_, MoqBroadcastPublisher>, MoqExtensionError> {
        lock_or_refuse(&self.publisher, "publishing")
    }
}

/// Subscribes to a MoQ broadcast.
///
/// The subscriber's reading thread owns the session for its whole life — it
/// connects, drains and closes it — so no two threads reach one session. The
/// lock is taken inside `detach` in every method regardless, for the same
/// deadlock reason the publisher's is.
#[pyclass]
struct MoqBroadcastSubscribingSession {
    subscriber: Mutex<MoqBroadcastSubscriber>,
}

#[pymethods]
impl MoqBroadcastSubscribingSession {
    #[new]
    #[pyo3(signature = (
        relay_url, broadcast, container_format, video_track=None, audio_track=None,
        data_track=None
    ))]
    fn new(
        relay_url: String,
        broadcast: String,
        container_format: &str,
        video_track: Option<String>,
        audio_track: Option<String>,
        data_track: Option<String>,
    ) -> PyResult<Self> {
        let container_format = MoqContainerFormat::of_wire_name(container_format)?;
        let config = MoqRelayConfig {
            relay_endpoint_url: relay_url,
            broadcast_path: broadcast,
        };
        Ok(Self {
            subscriber: Mutex::new(MoqBroadcastSubscriber::new(
                config,
                container_format,
                video_track,
                audio_track,
                data_track,
            )?),
        })
    }

    /// Connect and begin draining. Called from the processor's own thread, not
    /// from `setup()`, so a relay outage cannot spend the start-up budget.
    fn connect(&self, python: Python<'_>) -> PyResult<()> {
        python.detach(|| {
            let mut subscriber = self.locked_subscriber()?;
            transport_stack::transport_runtime()?.block_on(subscriber.connect())
        })?;
        Ok(())
    }

    /// The next sample — an access unit, an Opus packet or a data object — or
    /// `None` if none arrived within `timeout_ms`.
    fn next_media(&self, python: Python<'_>, timeout_ms: u64) -> PyResult<Option<Py<PyAny>>> {
        let received = python.detach(|| {
            let mut subscriber = self.locked_subscriber()?;
            transport_stack::transport_runtime()?
                .block_on(subscriber.next_sample(Duration::from_millis(timeout_ms)))
        })?;

        Ok(match received {
            Some(ReceivedTrackSample {
                sample: MoqTrackSample::EncodedMedia(EncodedMediaSample::VideoAccessUnit(unit)),
                ..
            }) => Some(Py::new(python, ReceivedVideoAccessUnit::from(unit))?.into_any()),
            Some(ReceivedTrackSample {
                sample: MoqTrackSample::EncodedMedia(EncodedMediaSample::AudioPacket(packet)),
                ..
            }) => Some(Py::new(python, ReceivedOpusPacket::from(packet))?.into_any()),
            Some(ReceivedTrackSample {
                track_name,
                sample: MoqTrackSample::DataObject(object),
            }) => Some(
                Py::new(
                    python,
                    ReceivedDataObject {
                        track_name,
                        payload: object.envelope_bytes,
                    },
                )?
                .into_any(),
            ),
            None => None,
        })
    }

    /// Stop draining and drop the connection.
    fn close(&self, python: Python<'_>) -> PyResult<()> {
        python.detach(|| {
            self.locked_subscriber()?.close();
            Ok::<(), MoqExtensionError>(())
        })?;
        Ok(())
    }

    #[getter]
    fn is_connected(&self, python: Python<'_>) -> PyResult<bool> {
        Ok(python.detach(|| self.locked_subscriber().map(|open| open.is_connected()))?)
    }
}

impl MoqBroadcastSubscribingSession {
    fn locked_subscriber(
        &self,
    ) -> Result<MutexGuard<'_, MoqBroadcastSubscriber>, MoqExtensionError> {
        lock_or_refuse(&self.subscriber, "subscribing")
    }
}

/// Take a session's lock, or say which session an earlier panic poisoned.
fn lock_or_refuse<'session, Session>(
    session: &'session Mutex<Session>,
    role: &'static str,
) -> Result<MutexGuard<'session, Session>, MoqExtensionError> {
    session.lock().map_err(|_| MoqExtensionError::Transport {
        what: format!("the MoQ {role} session was left poisoned by an earlier panic"),
    })
}

/// One access unit off a MoQ broadcast, with every key the video wire contract
/// requires taken from the broadcast itself.
#[pyclass]
struct ReceivedVideoAccessUnit {
    inner: EncodedVideoAccessUnit,
}

impl From<EncodedVideoAccessUnit> for ReceivedVideoAccessUnit {
    fn from(inner: EncodedVideoAccessUnit) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl ReceivedVideoAccessUnit {
    #[getter]
    fn codec(&self) -> &str {
        &self.inner.codec
    }
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
    /// Built from a borrow rather than a clone of the map: this is read once
    /// per frame on the subscribe path.
    #[getter]
    fn color<'python>(&self, python: Python<'python>) -> PyResult<Option<Bound<'python, PyDict>>> {
        let Some(axes) = self.inner.color.as_ref() else {
            return Ok(None);
        };
        let color = PyDict::new(python);
        for (axis, spelling) in axes {
            color.set_item(axis.as_str(), spelling.as_str())?;
        }
        Ok(Some(color))
    }
}

/// One Opus packet off a MoQ broadcast.
#[pyclass]
struct ReceivedOpusPacket {
    inner: EncodedAudioPacket,
}

impl From<EncodedAudioPacket> for ReceivedOpusPacket {
    fn from(inner: EncodedAudioPacket) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl ReceivedOpusPacket {
    #[getter]
    fn bitstream<'python>(&self, python: Python<'python>) -> Bound<'python, PyBytes> {
        PyBytes::new(python, &self.inner.opus_packet)
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

/// One object off a MoQ data track, whole: the envelope the publisher's
/// Python built around the user's bag, for this wheel's Python to decode. No
/// stamp accessor, because the stamp is inside the envelope.
#[pyclass]
struct ReceivedDataObject {
    track_name: String,
    payload: bytes::Bytes,
}

#[pymethods]
impl ReceivedDataObject {
    /// Built by `next_media` on receive; constructible so the drain loop's
    /// dispatch on this type can be driven without a session.
    #[new]
    fn new(track_name: String, payload: &[u8]) -> Self {
        Self {
            track_name,
            payload: bytes::Bytes::copy_from_slice(payload),
        }
    }
    #[getter]
    fn track_name(&self) -> &str {
        &self.track_name
    }
    #[getter]
    fn payload<'python>(&self, python: Python<'python>) -> Bound<'python, PyBytes> {
        PyBytes::new(python, &self.payload)
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(bring_up_the_transport_stack, module)?)?;
    module.add_class::<MoqBroadcastPublishingSession>()?;
    module.add_class::<MoqBroadcastSubscribingSession>()?;
    module.add_class::<ReceivedVideoAccessUnit>()?;
    module.add_class::<ReceivedOpusPacket>()?;
    module.add_class::<ReceivedDataObject>()?;
    module.add_class::<MoqPublisherUplinkBacklogOnOneTrack>()?;
    module.add_class::<MoqPublisherQuicUplinkReadings>()?;
    Ok(())
}
