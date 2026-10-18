// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in relay track source: one track at a MoQ relay, read into a graph.
//!
//! The node is one MoQ client among any. It subscribes to the track
//! `<node>/<port>` of the broadcast `<machine>/<stream>.hang` and writes each
//! object's payload to its one output as received, the first object of each
//! group starting a group. Every stamp in a graph is this machine's, so an
//! object is stamped from the track's own timestamp re-based onto this
//! machine's clock by one offset, never by its arrival.

use std::thread::JoinHandle;
use std::time::Duration;

use hang::catalog::Section;
use serde::{Deserialize, Serialize};
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::descriptors::{CatalogEntry, CatalogSection};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::graph::PortAddress;
use streamlib::sdk::hang_legacy_container_media_frame::read_hang_legacy_container_media_frame;
use streamlib::sdk::iceoryx2::{MessageGroupBoundary, OutputWriter};
use streamlib::sdk::media_clock::MediaClock;
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::schemars::JsonSchema;
use streamlib::sdk::serde_json::Value;

use crate::processor_thread_join::join_within_grace_or_detach;

const TRACK_OUTPUT_PORT: &str = "track";

/// The suffix of a broadcast whose catalog the stack's players read.
const BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ: &str = ".hang";

/// How long after a failed session the relay is first dialed again.
const A_FAILED_SESSION_IS_FIRST_RETRIED_AFTER: Duration = Duration::from_millis(250);

/// The longest a failed session waits to be dialed again, the wait doubling
/// from [`A_FAILED_SESSION_IS_FIRST_RETRIED_AFTER`].
const A_FAILED_SESSION_IS_RETRIED_AT_LEAST_EVERY: Duration = Duration::from_secs(10);

/// How long `stop()` waits for the reading thread before detaching it; inside
/// the runtime's 5 s teardown budget.
const THE_READING_THREAD_ENDS_WITHIN: Duration = Duration::from_secs(4);

/// Configuration for [`RelayTrackSource`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct RelayTrackSourceConfig {
    /// The relay to subscribe at. The default is this machine's local relay on
    /// loopback, which serves every private and public port of the machine.
    #[serde(default = "default_relay_url")]
    pub relay_url: String,
    /// The track to read, `<machine>/<stream>/<node>/<port>`: the exposed port
    /// `<node>/<port>` of the stream `<stream>` on the machine `<machine>`.
    pub track_address: String,
}

fn default_relay_url() -> String {
    "http://127.0.0.1:4443".to_string()
}

impl Default for RelayTrackSourceConfig {
    fn default() -> Self {
        Self {
            relay_url: default_relay_url(),
            track_address: String::new(),
        }
    }
}

#[streamlib::sdk::processor(
    built_in_node,
    description = "Reads one track at a MoQ relay into the graph — how a stream reads another \
                  stream's exposed port, on this machine through the local relay's loopback \
                  listener or on another machine through any relay that serves it, and how it \
                  reads a track any other MoQ publisher wrote.\n\n\
                  `track` carries each object's payload bytes as the relay delivered them, the \
                  first object of each group starting a group, and its catalog entry is the \
                  track's own from the broadcast's catalog. What a payload means is the \
                  publisher's: a `VideoFrame` track names surfaces on the machine that wrote it, \
                  which mean nothing here.\n\n\
                  The relay's object carries the writer's stamp, on the writer's clock. Every \
                  stamp in a graph is this machine's, so `track` is stamped with the object's \
                  own timestamp — the hang container's for a media track, the object's \
                  otherwise — plus one offset taken at the first object, re-anchored when the \
                  track jumps, so the track's own spacing survives jittered arrival. An object \
                  carrying no timestamp is stamped at arrival.\n\n\
                  A relay that refuses, or a track not yet published or withdrawn, is retried \
                  with backoff and never ends the stream.",
    execution = manual,
    config = crate::relay_track_source::RelayTrackSourceConfig,
    output(
        "track",
        description = "Each object of the track, as the relay delivered it"
    ),
)]
pub struct RelayTrackSource {
    relay_track_location: Option<RelayTrackLocation>,
    relay_track_reading_thread: Option<RelayTrackReadingThread>,
}

impl ManualProcessor for RelayTrackSource::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.relay_track_location = Some(RelayTrackLocation::of(&self.config)?);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_reading();
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let Some(relay_track_location) = self.relay_track_location.clone() else {
            return Err(Error::Configuration(
                "RelayTrackSource: no track is located. setup() must run first.".into(),
            ));
        };
        self.stop_reading();
        self.relay_track_reading_thread = Some(RelayTrackReadingThread::spawn(
            relay_track_location,
            self.outputs.clone(),
        )?);
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_reading();
        Ok(())
    }
}

impl RelayTrackSource::Processor {
    fn stop_reading(&mut self) {
        if let Some(relay_track_reading_thread) = self.relay_track_reading_thread.take() {
            relay_track_reading_thread.stop();
        }
    }
}

/// Where the node's track is: the relay, the broadcast at it and the track in
/// that broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RelayTrackLocation {
    relay_url: url::Url,
    broadcast_path: String,
    track_name: String,
}

impl RelayTrackLocation {
    /// Locate the track `config` names, refusing by name a relay URL that does
    /// not parse and a track address that is not `<machine>/<stream>/<node>/<port>`.
    fn of(config: &RelayTrackSourceConfig) -> Result<Self> {
        let relay_url = config.relay_url.parse::<url::Url>().map_err(|unparsed| {
            Error::Configuration(format!(
                "RelayTrackSource: `relay_url` `{}` is not a URL ({unparsed})",
                config.relay_url
            ))
        })?;
        let port_address = PortAddress::parse(&config.track_address).map_err(|refusal| {
            Error::Configuration(format!(
                "RelayTrackSource: `track_address` is not a track's address: {refusal}"
            ))
        })?;
        let (Some(machine), Some(stream)) = (port_address.machine(), port_address.stream()) else {
            return Err(Error::Configuration(format!(
                "RelayTrackSource: `track_address` `{}` names no machine or no stream, and a \
                 track's address is `<machine>/<stream>/<node>/<port>`",
                config.track_address
            )));
        };
        Ok(Self {
            relay_url,
            broadcast_path: format!(
                "{machine}/{stream}{BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ}"
            ),
            track_name: format!("{}/{}", port_address.node_name(), port_address.port_name()),
        })
    }
}

impl std::fmt::Display for RelayTrackLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the track `{}` of `{}` at {}",
            self.track_name, self.broadcast_path, self.relay_url
        )
    }
}

/// The thread a running node reads its track on, with its own async runtime.
struct RelayTrackReadingThread {
    stop_requested: tokio::sync::watch::Sender<bool>,
    thread: JoinHandle<()>,
}

impl RelayTrackReadingThread {
    fn spawn(relay_track_location: RelayTrackLocation, outputs: OutputWriter) -> Result<Self> {
        let async_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|refusal| {
                Error::Runtime(format!(
                    "RelayTrackSource: no async runtime builds to read {relay_track_location}: \
                     {refusal}"
                ))
            })?;
        let (stop_requested, stop_requested_receiver) = tokio::sync::watch::channel(false);
        let thread = std::thread::Builder::new()
            .name("relay-track-source".to_string())
            .spawn(move || {
                async_runtime.block_on(read_the_track_until_stopped(
                    relay_track_location,
                    outputs,
                    stop_requested_receiver,
                ));
            })
            .map_err(|refusal| {
                Error::Runtime(format!(
                    "RelayTrackSource: the reading thread did not start: {refusal}"
                ))
            })?;
        Ok(Self {
            stop_requested,
            thread,
        })
    }

    fn stop(self) {
        let _ = self.stop_requested.send(true);
        join_within_grace_or_detach(
            self.thread,
            THE_READING_THREAD_ENDS_WITHIN,
            "relay-track-source",
        );
    }
}

async fn read_the_track_until_stopped(
    relay_track_location: RelayTrackLocation,
    outputs: OutputWriter,
    mut stop_requested: tokio::sync::watch::Receiver<bool>,
) {
    let mut track_reader = RelayTrackReader::new(outputs);
    let mut retried_after = A_FAILED_SESSION_IS_FIRST_RETRIED_AFTER;
    let mut last_reason_the_session_ended: Option<String> = None;
    loop {
        let objects_written_before_the_session = track_reader.objects_written;
        let reason_the_session_ended = tokio::select! {
            _ = stop_requested.wait_for(|stop_requested| *stop_requested) => return,
            reason_the_session_ended = track_reader.read_through_one_session(&relay_track_location) => reason_the_session_ended,
        };
        if track_reader.objects_written > objects_written_before_the_session {
            retried_after = A_FAILED_SESSION_IS_FIRST_RETRIED_AFTER;
        }
        if last_reason_the_session_ended.as_ref() != Some(&reason_the_session_ended) {
            tracing::warn!(
                "RelayTrackSource: reading {relay_track_location} stopped ({reason_the_session_ended}); \
                 dialing again with backoff"
            );
            last_reason_the_session_ended = Some(reason_the_session_ended);
        }
        tokio::select! {
            _ = stop_requested.wait_for(|stop_requested| *stop_requested) => return,
            () = tokio::time::sleep(retried_after) => {}
        }
        retried_after = (retried_after * 2).min(A_FAILED_SESSION_IS_RETRIED_AT_LEAST_EVERY);
    }
}

/// Writes one track's objects onto the node's output, across every session
/// that reads it.
struct RelayTrackReader {
    outputs: OutputWriter,
    track_timestamp_rebasing: TrackTimestampRebasedOntoThisMachinesClock,
    where_the_track_carries_its_timestamp: WhereATrackCarriesItsTimestamp,
    catalog_entry_set_on_the_output: Option<CatalogEntry>,
    objects_written: u64,
}

impl RelayTrackReader {
    fn new(outputs: OutputWriter) -> Self {
        Self {
            outputs,
            track_timestamp_rebasing: TrackTimestampRebasedOntoThisMachinesClock::default(),
            where_the_track_carries_its_timestamp: WhereATrackCarriesItsTimestamp::OnTheObject,
            catalog_entry_set_on_the_output: None,
            objects_written: 0,
        }
    }

    /// Dial the relay once and write the track until the session, the
    /// broadcast or the track ends; the reason it ended.
    async fn read_through_one_session(
        &mut self,
        relay_track_location: &RelayTrackLocation,
    ) -> String {
        let client =
            match moq_tokio::connect::Config::default().init(moq_tokio::quic::Config::default()) {
                Ok(client) => client,
                Err(refusal) => return format!("no MoQ client builds: {refusal}"),
            };
        let subscribed_origin = moq_tokio::origin::spawn();
        let connection = client
            .with_subscriber(subscribed_origin.clone())
            .connect(relay_track_location.relay_url.clone());
        let announced_broadcasts = subscribed_origin.consume();
        let broadcast = tokio::select! {
            ended = connection.closed() => return the_reason_a_session_ended(ended),
            broadcast = announced_broadcasts
                .routed_broadcast(relay_track_location.broadcast_path.as_str()) => match broadcast {
                Ok(broadcast) => broadcast,
                Err(refusal) => return format!("the broadcast was refused: {refusal}"),
            },
        };
        let mut groups_of_the_track = match broadcast
            .track(&relay_track_location.track_name)
            .map(|track| track.subscribe(None))
        {
            Ok(subscribing) => match subscribing.await {
                Ok(subscriber) => subscriber.ordered(),
                Err(refusal) => return format!("the track was refused: {refusal}"),
            },
            Err(refusal) => return format!("the track was refused: {refusal}"),
        };
        let mut catalog_of_the_broadcast = match broadcast.track(hang::Catalog::DEFAULT_NAME) {
            Ok(catalog_track) => match catalog_track.subscribe(None).await {
                Ok(catalog_subscriber) => Some(moq_json::snapshot::Consumer::<Value>::new(
                    catalog_subscriber,
                    moq_json::snapshot::consumer::Config::default(),
                )),
                Err(refusal) => {
                    tracing::debug!("RelayTrackSource: the broadcast serves no catalog: {refusal}");
                    None
                }
            },
            Err(refusal) => {
                tracing::debug!("RelayTrackSource: the broadcast serves no catalog: {refusal}");
                None
            }
        };
        tracing::info!("RelayTrackSource: reading {relay_track_location}");

        let mut group_being_read: Option<moq_net::group::Consumer> = None;
        let mut the_next_object_starts_a_group = false;
        loop {
            tokio::select! {
                ended = connection.closed() => return the_reason_a_session_ended(ended),
                catalog = the_next_catalog_of(&mut catalog_of_the_broadcast) => match catalog {
                    Some(catalog) => self.take_the_tracks_entry_from(&catalog, &relay_track_location.track_name),
                    None => catalog_of_the_broadcast = None,
                },
                group = groups_of_the_track.next_group(), if group_being_read.is_none() => match group {
                    Ok(Some(group)) => {
                        group_being_read = Some(group);
                        the_next_object_starts_a_group = true;
                    }
                    Ok(None) => return "the track ended".to_string(),
                    Err(refusal) => return format!("the track ended: {refusal}"),
                },
                object = the_next_object_of(&mut group_being_read) => match object {
                    Ok(Some(object)) => {
                        let group_boundary = if std::mem::take(&mut the_next_object_starts_a_group) {
                            MessageGroupBoundary::StartsGroup
                        } else {
                            MessageGroupBoundary::ContinuesGroup
                        };
                        self.write_the_object(&object, group_boundary);
                    }
                    Ok(None) => group_being_read = None,
                    Err(refusal) => {
                        tracing::debug!("RelayTrackSource: a group ended early: {refusal}");
                        group_being_read = None;
                    }
                },
            }
        }
    }

    fn take_the_tracks_entry_from(&mut self, catalog: &Value, track_name: &str) {
        let catalog_entry = the_catalog_entry_of_the_track_named(catalog, track_name);
        if catalog_entry == self.catalog_entry_set_on_the_output {
            return;
        }
        self.where_the_track_carries_its_timestamp =
            WhereATrackCarriesItsTimestamp::of_a_track_with_the_entry(catalog_entry.as_ref());
        if let Some(catalog_entry) = &catalog_entry
            && let Err(refusal) = self
                .outputs
                .set_catalog_entry(TRACK_OUTPUT_PORT, catalog_entry.clone())
        {
            tracing::warn!(
                "RelayTrackSource: the track's catalog entry is not carried on `track`: {refusal}"
            );
        }
        self.catalog_entry_set_on_the_output = catalog_entry;
    }

    fn write_the_object(
        &mut self,
        object: &moq_net::frame::Frame,
        group_boundary: MessageGroupBoundary,
    ) {
        let arrival_ns = MediaClock::now().as_nanos() as i64;
        let object_timestamp_ns = self
            .where_the_track_carries_its_timestamp
            .the_timestamp_ns_of(object);
        let timestamp_ns = self
            .track_timestamp_rebasing
            .stamp_on_this_machines_clock(object_timestamp_ns, arrival_ns);
        match self.outputs.write_bytes_with_timestamp(
            TRACK_OUTPUT_PORT,
            &object.payload,
            timestamp_ns,
            group_boundary,
        ) {
            Ok(()) => self.objects_written += 1,
            Err(refusal) => {
                tracing::debug!("RelayTrackSource: an object was not written: {refusal}")
            }
        }
    }
}

fn the_reason_a_session_ended(ended: moq_tokio::Result<()>) -> String {
    match ended {
        Ok(()) => "the relay closed the session".to_string(),
        Err(refusal) => format!("the session failed: {refusal}"),
    }
}

async fn the_next_catalog_of(
    catalog_of_the_broadcast: &mut Option<moq_json::snapshot::Consumer<Value>>,
) -> Option<Value> {
    let Some(catalog) = catalog_of_the_broadcast else {
        return std::future::pending().await;
    };
    match catalog.next().await {
        Ok(catalog) => catalog,
        Err(refusal) => {
            tracing::debug!("RelayTrackSource: the catalog no longer reads: {refusal}");
            None
        }
    }
}

async fn the_next_object_of(
    group_being_read: &mut Option<moq_net::group::Consumer>,
) -> moq_net::Result<Option<moq_net::frame::Frame>> {
    match group_being_read {
        Some(group) => group.read_frame().await,
        None => std::future::pending().await,
    }
}

/// The track named `track_name`'s entry in a broadcast's catalog, in hang's
/// layout: the same fields under the section that lists it.
fn the_catalog_entry_of_the_track_named(catalog: &Value, track_name: &str) -> Option<CatalogEntry> {
    CatalogSection::ALL.into_iter().find_map(|section| {
        let map_of_the_section = match section {
            CatalogSection::Video | CatalogSection::Audio => "renditions",
            CatalogSection::Text => hang::catalog::Text::MAP,
            CatalogSection::Json => hang::catalog::Json::MAP,
            CatalogSection::Binary => hang::catalog::Binary::MAP,
        };
        let entry = catalog
            .get(section.as_catalog_key())?
            .get(map_of_the_section)?
            .get(track_name)?;
        Some(CatalogEntry {
            section,
            entry: entry.clone(),
        })
    })
}

/// Where a track's objects carry the timestamp its writer gave them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhereATrackCarriesItsTimestamp {
    /// A media track in hang's legacy container: a varint in microseconds
    /// opening every payload.
    InTheHangLegacyContainersVarint,
    /// The object's own transport timestamp; zero carries none.
    OnTheObject,
}

impl WhereATrackCarriesItsTimestamp {
    fn of_a_track_with_the_entry(catalog_entry: Option<&CatalogEntry>) -> Self {
        let container = catalog_entry.and_then(|catalog_entry| match catalog_entry.section {
            CatalogSection::Video => hang::catalog::VideoConfig::deserialize(&catalog_entry.entry)
                .ok()
                .map(|video| video.container),
            CatalogSection::Audio => hang::catalog::AudioConfig::deserialize(&catalog_entry.entry)
                .ok()
                .map(|audio| audio.container),
            _ => None,
        });
        match container {
            Some(hang::catalog::Container::Legacy) => Self::InTheHangLegacyContainersVarint,
            _ => Self::OnTheObject,
        }
    }

    /// The writer's timestamp of `object`, in nanoseconds; `None` when it
    /// carries none.
    fn the_timestamp_ns_of(self, object: &moq_net::frame::Frame) -> Option<i128> {
        if self == Self::InTheHangLegacyContainersVarint
            && let Ok(media_frame) = read_hang_legacy_container_media_frame(&object.payload)
        {
            return Some(i128::from(media_frame.timestamp_microseconds) * 1_000);
        }
        if object.timestamp.is_zero() {
            return None;
        }
        i128::try_from(object.timestamp.as_nanos()).ok()
    }
}

/// How far a re-based stamp may sit from the object's arrival before the
/// track is taken to have jumped and the offset is taken again.
const A_REBASED_STAMP_STRAYS_FROM_ARRIVAL_BY_AT_MOST: Duration = Duration::from_secs(1);

/// How far a track's timestamp may step back — a reordered frame — before the
/// track is taken to have restarted and the offset is taken again.
const A_TRACKS_TIMESTAMP_STEPS_BACK_BY_AT_MOST: Duration = Duration::from_millis(250);

/// A track's own timestamps restated on this machine's clock: each plus one
/// offset taken at the first object, so the track's spacing is kept whatever
/// the network did to the objects' arrival.
#[derive(Debug, Default)]
struct TrackTimestampRebasedOntoThisMachinesClock {
    offset_ns: Option<i128>,
    latest_track_timestamp_ns: Option<i128>,
}

impl TrackTimestampRebasedOntoThisMachinesClock {
    /// The stamp of an object stamped `track_timestamp_ns` by its writer and
    /// arriving at `arrival_ns` on this machine's clock; an object carrying no
    /// timestamp is stamped at arrival.
    fn stamp_on_this_machines_clock(
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
        let offset_ns = match self.offset_ns {
            Some(offset_ns)
                if !the_track_stepped_back
                    && (track_timestamp_ns + offset_ns - arrival_ns_wide).unsigned_abs()
                        <= A_REBASED_STAMP_STRAYS_FROM_ARRIVAL_BY_AT_MOST.as_nanos() =>
            {
                offset_ns
            }
            previous_offset_ns => {
                if previous_offset_ns.is_some() {
                    tracing::debug!(
                        "RelayTrackSource: the track jumped, so its stamps are re-anchored at \
                         this object's arrival"
                    );
                }
                arrival_ns_wide - track_timestamp_ns
            }
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
    use streamlib::sdk::processors::GeneratedProcessor;
    use streamlib::sdk::serde_json::json;

    const FRAME_SPACING_NS: i128 = 33_333_333;
    const NETWORK_LATENCY_NS: i64 = 40_000_000;

    /// Arrival jitter of up to ±15 ms, the same every run.
    fn arrival_jitter_ns(object_index: i64) -> i64 {
        ((object_index * 7_919) % 31 - 15) * 1_000_000
    }

    fn arrival_of(object_index: i64, track_timestamp_ns: i128) -> i64 {
        5_000_000_000
            + track_timestamp_ns as i64
            + NETWORK_LATENCY_NS
            + arrival_jitter_ns(object_index)
    }

    #[test]
    fn the_output_keeps_the_tracks_spacing_under_jittered_arrival() {
        let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
        let stamps: Vec<i64> = (0..120)
            .map(|object_index| {
                let track_timestamp_ns =
                    1_000_000_000_000 + i128::from(object_index) * FRAME_SPACING_NS;
                rebasing.stamp_on_this_machines_clock(
                    Some(track_timestamp_ns),
                    arrival_of(object_index, track_timestamp_ns - 1_000_000_000_000),
                )
            })
            .collect();
        for pair in stamps.windows(2) {
            assert_eq!(
                i128::from(pair[1] - pair[0]),
                FRAME_SPACING_NS,
                "the track's spacing is kept: {stamps:?}"
            );
        }
        assert_eq!(
            stamps[0],
            arrival_of(0, 0),
            "the offset is taken at the first object's arrival"
        );
    }

    #[test]
    fn a_track_that_restarts_its_timestamps_is_re_anchored_and_keeps_its_spacing_after() {
        let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
        let mut arrival_ns = 9_000_000_000_i64;
        for object_index in 0..30 {
            rebasing.stamp_on_this_machines_clock(
                Some(600_000_000 + i128::from(object_index) * FRAME_SPACING_NS),
                arrival_ns,
            );
            arrival_ns += FRAME_SPACING_NS as i64;
        }
        let restarted_arrival_ns = arrival_ns + 2_000_000;
        let first_after_the_restart =
            rebasing.stamp_on_this_machines_clock(Some(0), restarted_arrival_ns);
        assert_eq!(first_after_the_restart, restarted_arrival_ns);
        let second_after_the_restart = rebasing.stamp_on_this_machines_clock(
            Some(FRAME_SPACING_NS),
            restarted_arrival_ns + FRAME_SPACING_NS as i64 + 9_000_000,
        );
        assert_eq!(
            i128::from(second_after_the_restart - first_after_the_restart),
            FRAME_SPACING_NS
        );
    }

    #[test]
    fn a_track_that_leaps_ahead_of_its_arrival_is_re_anchored() {
        let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
        rebasing.stamp_on_this_machines_clock(Some(0), 1_000_000_000);
        rebasing.stamp_on_this_machines_clock(Some(FRAME_SPACING_NS), 1_033_333_333);
        let after_the_leap =
            rebasing.stamp_on_this_machines_clock(Some(60_000_000_000), 1_066_666_666);
        assert_eq!(after_the_leap, 1_066_666_666);
    }

    #[test]
    fn a_frame_reordered_a_little_back_keeps_the_offset() {
        let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
        let first = rebasing.stamp_on_this_machines_clock(Some(100_000_000), 2_000_000_000);
        let reordered = rebasing.stamp_on_this_machines_clock(Some(33_333_333), 2_010_000_000);
        assert_eq!(i128::from(first - reordered), 66_666_667);
    }

    #[test]
    fn an_object_carrying_no_timestamp_is_stamped_at_arrival() {
        let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
        assert_eq!(
            rebasing.stamp_on_this_machines_clock(None, 7_000_000_000),
            7_000_000_000
        );
        assert_eq!(
            rebasing.stamp_on_this_machines_clock(None, 7_123_000_000),
            7_123_000_000
        );
    }

    fn an_object(transport_timestamp_ns: u64, payload: Vec<u8>) -> moq_net::frame::Frame {
        moq_net::frame::Frame {
            timestamp: moq_net::Timestamp::from_nanos(transport_timestamp_ns).unwrap(),
            payload: payload.into(),
        }
    }

    #[test]
    fn a_legacy_media_tracks_timestamp_is_its_varint_and_any_other_tracks_the_objects() {
        let mut media_frame = Vec::new();
        streamlib::sdk::hang_legacy_container_media_frame::write_hang_legacy_container_media_frame(
            &mut media_frame,
            1_234_567_000,
            &[0, 0, 0, 1, 0x65],
        )
        .unwrap();
        let video_entry = CatalogEntry {
            section: CatalogSection::Video,
            entry: json!({"codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720}),
        };
        let binary_entry = CatalogEntry {
            section: CatalogSection::Binary,
            entry: json!({"mode": "snapshot", "mime": "application/msgpack"}),
        };

        let in_the_varint =
            WhereATrackCarriesItsTimestamp::of_a_track_with_the_entry(Some(&video_entry));
        assert_eq!(
            in_the_varint,
            WhereATrackCarriesItsTimestamp::InTheHangLegacyContainersVarint
        );
        assert_eq!(
            in_the_varint.the_timestamp_ns_of(&an_object(0, media_frame.clone())),
            Some(1_234_567_000)
        );

        for on_the_object in [
            WhereATrackCarriesItsTimestamp::of_a_track_with_the_entry(Some(&binary_entry)),
            WhereATrackCarriesItsTimestamp::of_a_track_with_the_entry(None),
        ] {
            assert_eq!(on_the_object, WhereATrackCarriesItsTimestamp::OnTheObject);
            assert_eq!(
                on_the_object.the_timestamp_ns_of(&an_object(77_000, media_frame.clone())),
                Some(77_000)
            );
            assert_eq!(
                on_the_object.the_timestamp_ns_of(&an_object(0, b"x".to_vec())),
                None
            );
        }
    }

    #[test]
    fn a_tracks_entry_is_read_from_the_section_that_lists_it_as_written() {
        let catalog = json!({
            "video": {"renditions": {"encoder/encoded_video": {
                "codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720, "framerate": 30
            }}},
            "binary": {"tracks": {"camera/video": {"mode": "snapshot", "mime": "application/msgpack"}}},
        });
        assert_eq!(
            the_catalog_entry_of_the_track_named(&catalog, "encoder/encoded_video"),
            Some(CatalogEntry {
                section: CatalogSection::Video,
                entry: json!({
                    "codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720, "framerate": 30
                }),
            })
        );
        assert_eq!(
            the_catalog_entry_of_the_track_named(&catalog, "camera/video")
                .map(|entry| entry.section),
            Some(CatalogSection::Binary)
        );
        assert_eq!(
            the_catalog_entry_of_the_track_named(&catalog, "nobody/here"),
            None
        );
    }

    fn config_naming(track_address: &str) -> RelayTrackSourceConfig {
        RelayTrackSourceConfig {
            relay_url: default_relay_url(),
            track_address: track_address.to_string(),
        }
    }

    #[test]
    fn a_track_address_locates_the_streams_broadcast_and_the_ports_track_cast() {
        let location = RelayTrackLocation::of(&config_naming("Desk/Main/Front Camera/video"))
            .expect("a four-part address locates a track");
        assert_eq!(location.relay_url.as_str(), "http://127.0.0.1:4443/");
        assert_eq!(location.broadcast_path, "desk/main.hang");
        assert_eq!(location.track_name, "front-camera/video");
    }

    #[test]
    fn a_track_address_naming_no_machine_and_stream_is_refused_naming_the_shape() {
        for address in ["main/camera/video", "camera/video"] {
            let refusal = RelayTrackLocation::of(&config_naming(address))
                .expect_err("an address short of a machine and a stream locates no track")
                .to_string();
            assert!(
                refusal.contains("`<machine>/<stream>/<node>/<port>`") && refusal.contains(address),
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_relay_url_that_does_not_parse_is_refused_naming_it() {
        let refusal = RelayTrackLocation::of(&RelayTrackSourceConfig {
            relay_url: "not a url".into(),
            track_address: "desk/main/camera/video".into(),
        })
        .expect_err("a relay URL that does not parse")
        .to_string();
        assert!(refusal.contains("`relay_url` `not a url`"), "{refusal}");
    }

    #[test]
    fn the_descriptor_publishes_the_config_and_the_one_output() {
        let descriptor = <RelayTrackSource::Processor as GeneratedProcessor>::descriptor()
            .expect("a descriptor");
        let config_schema = descriptor.config_schema.expect("a config schema");
        assert_eq!(
            config_schema["properties"]["relay_url"]["default"],
            "http://127.0.0.1:4443"
        );
        assert_eq!(config_schema["required"], json!(["track_address"]));
        assert_eq!(
            descriptor
                .outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect::<Vec<_>>(),
            ["track"]
        );
    }
}
