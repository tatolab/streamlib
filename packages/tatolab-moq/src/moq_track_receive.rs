// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A receive node's thread: it asks its gateway for a track every second and
//! writes what the gateway reads onto the node's `content` output, each object
//! re-based onto the stream's clock.

use std::time::{Duration, Instant};

use crate::error::{MoqModuleError, Result};
use crate::moq_catalog::the_track_carries_its_timestamp_in_the_hang_legacy_container;
use crate::moq_node_ipc::{
    MoqGatewayControlMessage, MoqNodeIpc, MoqReceivedContentSampleKind,
    MoqTrackContentReceivedFromTheGatewayHeader, publish_slice,
};
use crate::moq_node_thread::MoqNodeThread;

/// How often a running receive node asks its gateway for its track again.
pub const A_RECEIVE_REQUEST_IS_SENT_AGAIN_EVERY: Duration = Duration::from_secs(1);

/// The longest the receive waits for its gateway's wake before it checks for a
/// stop and whether its request is due again.
const THE_RECEIVE_WAKES_AT_LEAST_EVERY: Duration = Duration::from_millis(50);

/// The family a receive node's `content` output carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoqReceivedTrackFamily {
    /// Packets units coded as the codec named; empty for a plain byte stream.
    Packets { codec: String },
    /// Records messages, each one msgpack map.
    Records,
}

/// Where a receive node writes what its gateway reads.
pub trait MoqReceivedTrackOutput: Send {
    /// Write one object stamped `timestamp_ns` on the stream's clock.
    fn write_object(&mut self, payload: &[u8], timestamp_ns: i64, starts_group: bool);
    /// Count `given_up_group_count` groups lost before the next write.
    fn declare_groups_given_up(&mut self, given_up_group_count: u64);
    /// Carry the track's catalog entry, `{"section", "entry"}`.
    fn set_catalog_entry(&mut self, section_and_entry: &serde_json::Value);
    /// The stream's clock now, in nanoseconds.
    fn stream_time_now_ns(&self) -> i64;
}

/// What a receive node asks its gateway for.
#[derive(Debug, Clone)]
pub struct MoqTrackReceiveAsked {
    pub receive_node: String,
    pub gateway_node: String,
    pub broadcast: String,
    pub track: String,
    pub max_age: Duration,
}

/// The receive node's running thread.
pub struct MoqTrackReceiveThread {
    thread: MoqNodeThread,
}

impl MoqTrackReceiveThread {
    /// Ask for `asked` on `ipc` and write what arrives to `output`.
    pub fn spawn(
        ipc: MoqNodeIpc,
        asked: MoqTrackReceiveAsked,
        mut output: Box<dyn MoqReceivedTrackOutput>,
    ) -> Result<Self> {
        let control = ipc.gateway_control_publisher(&asked.gateway_node)?;
        let gateway_wake = ipc.notifier(&ipc.service_names.gateway_wake(&asked.gateway_node))?;
        let content = ipc.receive_content_subscriber(&asked.receive_node)?;
        let wake = ipc.listener(&ipc.service_names.receive_wake(&asked.receive_node))?;
        let request = serde_json::to_vec(&MoqGatewayControlMessage::TrackReceiveRequested {
            receive_node: asked.receive_node.clone(),
            broadcast: asked.broadcast.clone(),
            track: asked.track.clone(),
            max_age_milliseconds: u64::try_from(asked.max_age.as_millis()).unwrap_or(u64::MAX),
        })
        .map_err(|refusal| MoqModuleError::Refused {
            what: format!("the receive request does not serialize: {refusal}"),
        })?;
        let thread = MoqNodeThread::spawn("moq-track-receive", move |stop_signal| {
            let _ipc_held_while_the_thread_runs = ipc;
            let mut writing = MoqReceivedContentWriting::default();
            let mut request_last_sent_at: Option<Instant> = None;
            while !stop_signal.is_requested() {
                let now = Instant::now();
                if request_last_sent_at
                    .is_none_or(|sent_at| now >= sent_at + A_RECEIVE_REQUEST_IS_SENT_AGAIN_EVERY)
                {
                    match publish_slice(&control, (), &request) {
                        Ok(()) => {
                            let _ = gateway_wake.notify();
                        }
                        Err(refusal) => {
                            tracing::debug!("MoqTrackReceive: the request was not sent: {refusal}")
                        }
                    }
                    request_last_sent_at = Some(now);
                }
                loop {
                    match content.receive() {
                        Ok(Some(sample)) => writing.write_the_sample(
                            *sample.user_header(),
                            sample.payload(),
                            output.as_mut(),
                        ),
                        Ok(None) => break,
                        Err(refusal) => {
                            tracing::debug!("MoqTrackReceive: content did not read: {refusal:?}");
                            break;
                        }
                    }
                }
                let _ = wake.timed_wait(|_| {}, THE_RECEIVE_WAKES_AT_LEAST_EVERY);
            }
        })?;
        Ok(Self { thread })
    }

    /// Stop the thread and wait up to `timeout` for it; whether it ended.
    pub fn stop_and_join(&mut self, timeout: Duration) -> bool {
        self.thread.stop_and_join(timeout)
    }
}

/// The writing of what a gateway sent onto the output, carried across samples.
#[derive(Debug, Default)]
pub struct MoqReceivedContentWriting {
    next_sequence_number: Option<u64>,
    timestamps_ride_the_hang_legacy_container: bool,
    track_timestamp_rebasing: TrackTimestampRebasedOntoTheStreamClock,
    waiting_for_a_group_start: bool,
}

impl MoqReceivedContentWriting {
    /// Write one sample the gateway sent.
    pub fn write_the_sample(
        &mut self,
        header: MoqTrackContentReceivedFromTheGatewayHeader,
        payload: &[u8],
        output: &mut dyn MoqReceivedTrackOutput,
    ) {
        let samples_overwritten = self.next_sequence_number.map_or(0, |expected| {
            header.sequence_number.saturating_sub(expected)
        });
        self.next_sequence_number = Some(header.sequence_number + 1);
        if samples_overwritten > 0 {
            output.declare_groups_given_up(1);
            self.waiting_for_a_group_start = true;
        }
        match header.kind {
            kind if kind == MoqReceivedContentSampleKind::CatalogEntry as u32 => {
                let section_and_entry = serde_json::from_slice::<serde_json::Value>(payload).ok();
                self.timestamps_ride_the_hang_legacy_container =
                    the_track_carries_its_timestamp_in_the_hang_legacy_container(
                        section_and_entry.as_ref(),
                    );
                if let Some(section_and_entry) = &section_and_entry {
                    output.set_catalog_entry(section_and_entry);
                }
            }
            kind if kind == MoqReceivedContentSampleKind::GroupsGivenUp as u32 => {
                output.declare_groups_given_up(header.groups_given_up);
            }
            kind if kind == MoqReceivedContentSampleKind::Object as u32 => {
                let starts_group = header.starts_group != 0;
                if self.waiting_for_a_group_start && !starts_group {
                    return;
                }
                self.waiting_for_a_group_start = false;
                let track_timestamp_ns = if self.timestamps_ride_the_hang_legacy_container {
                    the_hang_legacy_container_timestamp_ns_of(payload)
                } else {
                    (header.object_timestamp_ns != 0)
                        .then(|| i128::from(header.object_timestamp_ns))
                };
                let timestamp_ns = self
                    .track_timestamp_rebasing
                    .stamp_on_the_stream_clock(track_timestamp_ns, output.stream_time_now_ns());
                output.write_object(payload, timestamp_ns, starts_group);
            }
            _ => {}
        }
    }
}

/// The microsecond QUIC varint opening a payload in hang's legacy container,
/// in nanoseconds.
fn the_hang_legacy_container_timestamp_ns_of(payload: &[u8]) -> Option<i128> {
    let first = *payload.first()?;
    let length = 1usize << (first >> 6);
    let bytes = payload.get(..length)?;
    let mut microseconds = u64::from(first & 0x3f);
    for byte in &bytes[1..] {
        microseconds = (microseconds << 8) | u64::from(*byte);
    }
    Some(i128::from(microseconds) * 1_000)
}

/// How far a re-based stamp may fall behind the object's arrival before the
/// track is taken to have jumped and the offset is taken again.
const A_REBASED_STAMP_FALLS_BEHIND_ARRIVAL_BY_AT_MOST: Duration = Duration::from_secs(1);

/// How far a re-based stamp may run ahead of the object's arrival; the group a
/// joining reader is handed in one burst is written at its arrival.
const A_REBASED_STAMP_RUNS_AHEAD_OF_ARRIVAL_BY_AT_MOST: Duration = Duration::from_millis(100);

/// How far a track's timestamp may step back, a reordered frame, before the
/// track is taken to have restarted.
const A_TRACKS_TIMESTAMP_STEPS_BACK_BY_AT_MOST: Duration = Duration::from_millis(250);

/// A track's own timestamps restated on the stream's clock: each plus one
/// offset taken at the first object, lowered when an object arrives earlier
/// than it allows, so the track's spacing survives arrival jitter.
#[derive(Debug, Default)]
pub struct TrackTimestampRebasedOntoTheStreamClock {
    offset_ns: Option<i128>,
    latest_track_timestamp_ns: Option<i128>,
}

impl TrackTimestampRebasedOntoTheStreamClock {
    /// The stamp of an object stamped `track_timestamp_ns` by its writer and
    /// arriving at `arrival_ns`; an object carrying no timestamp is stamped at
    /// arrival.
    pub fn stamp_on_the_stream_clock(
        &mut self,
        track_timestamp_ns: Option<i128>,
        arrival_ns: i64,
    ) -> i64 {
        let Some(track_timestamp_ns) = track_timestamp_ns else {
            return arrival_ns;
        };
        let arrival_ns_wide = i128::from(arrival_ns);
        let the_track_stepped_back = self.latest_track_timestamp_ns.is_some_and(|latest| {
            latest - track_timestamp_ns
                > A_TRACKS_TIMESTAMP_STEPS_BACK_BY_AT_MOST.as_nanos() as i128
        });
        let largest_lead_over_arrival_ns =
            A_REBASED_STAMP_RUNS_AHEAD_OF_ARRIVAL_BY_AT_MOST.as_nanos() as i128;
        let the_rebased_stamp_falls_too_far_behind = |offset_ns: i128| {
            arrival_ns_wide - (track_timestamp_ns + offset_ns)
                > A_REBASED_STAMP_FALLS_BEHIND_ARRIVAL_BY_AT_MOST.as_nanos() as i128
        };
        let offset_ns = match self.offset_ns {
            Some(offset_ns)
                if !the_track_stepped_back
                    && !the_rebased_stamp_falls_too_far_behind(offset_ns) =>
            {
                offset_ns.min(arrival_ns_wide + largest_lead_over_arrival_ns - track_timestamp_ns)
            }
            _ => arrival_ns_wide - track_timestamp_ns,
        };
        self.offset_ns = Some(offset_ns);
        self.latest_track_timestamp_ns = Some(
            self.latest_track_timestamp_ns
                .filter(|_| !the_track_stepped_back)
                .map_or(track_timestamp_ns, |latest| latest.max(track_timestamp_ns)),
        );
        i64::try_from(track_timestamp_ns + offset_ns).unwrap_or(arrival_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_containers_varint_reads_as_its_timestamp() {
        assert_eq!(
            the_hang_legacy_container_timestamp_ns_of(&[0x25, 0xff]),
            Some(37_000)
        );
        assert_eq!(
            the_hang_legacy_container_timestamp_ns_of(&[0x7b, 0xbd, 0x00]),
            Some(15_293_000)
        );
        assert_eq!(
            the_hang_legacy_container_timestamp_ns_of(&[0x80, 0x01]),
            None
        );
    }

    #[test]
    fn the_track_keeps_its_spacing_and_a_restart_re_anchors_it() {
        let mut rebasing = TrackTimestampRebasedOntoTheStreamClock::default();
        assert_eq!(
            rebasing.stamp_on_the_stream_clock(Some(1_000_000_000), 5_000_000_000),
            5_000_000_000
        );
        assert_eq!(
            rebasing.stamp_on_the_stream_clock(Some(1_033_000_000), 5_045_000_000),
            5_033_000_000
        );
        assert_eq!(
            rebasing.stamp_on_the_stream_clock(Some(0), 6_000_000_000),
            6_000_000_000
        );
        assert_eq!(rebasing.stamp_on_the_stream_clock(None, 7), 7);
    }
}
