// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in relay track source: one track at a MoQ relay, read into a graph.
//!
//! The node is one MoQ client among any. It subscribes to a broadcast's track,
//! each named as its publisher wrote it, and writes each object's payload to
//! its one output as received, the first object of each group starting a
//! group. Every stamp in a graph is this machine's, so an
//! object is stamped from the track's own timestamp re-based onto this
//! machine's clock by one offset, never by its arrival.

use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::descriptors::{
    CatalogEntry, CatalogSection, the_hang_map_a_catalog_section_lists_its_tracks_under,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::hang_legacy_container_media_frame::read_hang_legacy_container_media_frame;
use streamlib::sdk::iceoryx2::{MessageGroupBoundary, OutputWriter};
use streamlib::sdk::media_clock::MediaClock;
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::schemars::JsonSchema;
use streamlib::sdk::serde_json::Value;
use streamlib_relay_track_reader::{
    ReleasedRelayTrackGroup, RelayTrackGroupsReleasedInSequenceOrder,
};

use crate::processor_thread_join::join_within_grace_or_detach;

const TRACK_OUTPUT_PORT: &str = "track";

/// How long after a failed session the relay is first dialed again.
const A_FAILED_SESSION_IS_FIRST_RETRIED_AFTER: Duration = Duration::from_millis(250);

/// The longest a failed session waits to be dialed again, the wait doubling
/// from [`A_FAILED_SESSION_IS_FIRST_RETRIED_AFTER`].
const A_FAILED_SESSION_IS_RETRIED_AT_LEAST_EVERY: Duration = Duration::from_secs(10);

/// How long a session waits for the broadcast's first catalog, which says
/// where the track carries its timestamp, before it reads the track without it.
const THE_FIRST_CATALOG_IS_AWAITED_FOR: Duration = Duration::from_secs(2);

/// How long `stop()` spends telling the relay the session closed, inside
/// [`THE_READING_THREAD_ENDS_WITHIN`].
const THE_RELAY_IS_TOLD_THE_SESSION_CLOSED_WITHIN: Duration = Duration::from_secs(1);

/// The name of the thread a running node reads its track on.
const RELAY_TRACK_READING_THREAD_NAME: &str = "relay-track-source";

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
    /// The broadcast's path at the relay, exactly as its publisher wrote it. A
    /// stream's is `<machine>/<stream>.hang`.
    pub broadcast: String,
    /// The track of `broadcast` to read, exactly as its publisher named it. An
    /// exposed port's is `<node>/<port>`.
    pub track: String,
    /// How far newer content on the track may run past a missing group, in
    /// whole milliseconds of the track's own timestamps, before the group is
    /// given up; the history asked of the relay, clamped to what the track
    /// keeps. A transport budget, never a playout delay.
    #[serde(default = "default_max_age_milliseconds")]
    pub max_age: u64,
}

fn default_relay_url() -> String {
    "http://127.0.0.1:4443".to_string()
}

fn default_max_age_milliseconds() -> u64 {
    100
}

/// A node given no config names no track, and is refused at setup naming
/// `broadcast`.
impl Default for RelayTrackSourceConfig {
    fn default() -> Self {
        Self {
            relay_url: default_relay_url(),
            broadcast: String::new(),
            track: String::new(),
            max_age: default_max_age_milliseconds(),
        }
    }
}

#[streamlib::sdk::processor(
    built_in_node,
    description = "Reads one track at a MoQ relay into the graph — how a stream reads another \
                  stream's exposed port, on this machine through the local relay's loopback \
                  listener or on another machine through any relay that serves it, and how it \
                  reads a track any other MoQ publisher wrote. The track is named by `broadcast` \
                  and `track`, each exactly as its publisher wrote it: a stream's exposed port \
                  is the track `<node>/<port>` of the broadcast `<machine>/<stream>.hang`.\n\n\
                  `track` carries each object's payload bytes as the relay delivered them, the \
                  first object of each group starting a group, and its catalog entry is the \
                  track's own from the broadcast's catalog. What a payload means is the \
                  publisher's: a `VideoFrame` track names surfaces on the machine that wrote it, \
                  which mean nothing here.\n\n\
                  Groups reach `track` in sequence order, whatever order the relay delivers \
                  them in. A missing group is waited for until newer content on the track runs \
                  `max_age` milliseconds past it, measured in the track's own timestamps and \
                  never on a clock, until its stream is reset, or until the track ends; then it \
                  is given up and the next group read. The read joins at the live edge.\n\n\
                  The relay's object carries the writer's stamp, on the writer's clock. Every \
                  stamp in a graph is this machine's, so `track` is stamped with the object's \
                  own timestamp — the hang container's for a media track, the object's \
                  otherwise — plus one offset taken at the first object and lowered for an \
                  object that would run more than 100 ms ahead of its arrival, as the stack's \
                  own player lowers its reference past its buffer, such as the group in progress \
                  a joining reader is handed in one burst; it is taken again when the track \
                  steps back or falls a second behind, so the track's own spacing survives \
                  jittered arrival. An object \
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
            Duration::from_millis(self.config.max_age),
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
    /// not parse and a broadcast or track left empty.
    fn of(config: &RelayTrackSourceConfig) -> Result<Self> {
        // A relay's credential may ride its URL, so a refusal never echoes it.
        let relay_url = config.relay_url.parse::<url::Url>().map_err(|unparsed| {
            Error::Configuration(format!(
                "RelayTrackSource: `relay_url` is not a URL ({unparsed})"
            ))
        })?;
        for (key, name_as_written) in [("broadcast", &config.broadcast), ("track", &config.track)] {
            if name_as_written.is_empty() {
                return Err(Error::Configuration(format!(
                    "RelayTrackSource: `{key}` is empty or not given; name it as its publisher wrote it"
                )));
            }
        }
        Ok(Self {
            relay_url,
            broadcast_path: config.broadcast.clone(),
            track_name: config.track.clone(),
        })
    }
}

impl std::fmt::Display for RelayTrackLocation {
    /// Names the relay by its origin alone: a credential rides a relay URL's
    /// path, query or user info, and none of them reaches a log line.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the track `{}` of `{}` at {}",
            self.track_name,
            self.broadcast_path,
            self.relay_url.origin().ascii_serialization()
        )
    }
}

/// The thread a running node reads its track on, with its own async runtime.
struct RelayTrackReadingThread {
    stop_requested: tokio::sync::watch::Sender<bool>,
    thread: JoinHandle<()>,
}

impl RelayTrackReadingThread {
    fn spawn(
        relay_track_location: RelayTrackLocation,
        max_age: Duration,
        outputs: OutputWriter,
    ) -> Result<Self> {
        let async_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|refusal| {
                Error::Runtime(format!(
                    "RelayTrackSource: no async runtime builds to read {relay_track_location}: \
                     {refusal}"
                ))
            })?;
        // One dial per session: the node's own backoff is the only redial.
        let moq_client = {
            let _inside_the_async_runtime = async_runtime.enter();
            moq_tokio::connect::Config::default()
                .init(moq_tokio::quic::Config::default())
                .map_err(|refusal| {
                    Error::Runtime(format!(
                        "RelayTrackSource: no MoQ client builds to read {relay_track_location}: \
                         {refusal}"
                    ))
                })?
                .with_reconnect(false)
        };
        let (stop_requested, stop_requested_receiver) = tokio::sync::watch::channel(false);
        let thread = std::thread::Builder::new()
            .name(RELAY_TRACK_READING_THREAD_NAME.to_string())
            .spawn(move || {
                async_runtime.block_on(read_the_track_until_stopped(
                    relay_track_location,
                    moq_client,
                    RelayTrackSourceSession::new(outputs, max_age),
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
            RELAY_TRACK_READING_THREAD_NAME,
        );
    }
}

async fn read_the_track_until_stopped(
    relay_track_location: RelayTrackLocation,
    moq_client: moq_tokio::Client,
    mut track_reader: RelayTrackSourceSession,
    mut stop_requested: tokio::sync::watch::Receiver<bool>,
) {
    let mut retried_after = A_FAILED_SESSION_IS_FIRST_RETRIED_AFTER;
    let mut last_reason_the_session_ended: Option<WhyARelayTrackSessionEnded> = None;
    loop {
        let objects_written_before_the_session = track_reader.objects_written;
        let reason_the_session_ended = tokio::select! {
            _ = stop_requested.wait_for(|stop_requested| *stop_requested) => break,
            reason_the_session_ended = track_reader
                .read_through_one_session(&moq_client, &relay_track_location) => {
                reason_the_session_ended
            }
        };
        if track_reader.objects_written > objects_written_before_the_session {
            retried_after = A_FAILED_SESSION_IS_FIRST_RETRIED_AFTER;
        }
        if last_reason_the_session_ended.as_ref() != Some(&reason_the_session_ended) {
            tracing::warn!(
                "RelayTrackSource: reading {relay_track_location} stopped \
                 ({reason_the_session_ended}); dialing again with backoff"
            );
            last_reason_the_session_ended = Some(reason_the_session_ended);
        }
        tokio::select! {
            _ = stop_requested.wait_for(|stop_requested| *stop_requested) => break,
            () = tokio::time::sleep(retried_after) => {}
        }
        retried_after = (retried_after * 2).min(A_FAILED_SESSION_IS_RETRIED_AT_LEAST_EVERY);
    }
    // Dropping a session only queues its close, which nothing sends once this
    // thread's runtime stops; closing the client sends it, so the relay lets
    // the subscriber go rather than waiting out its idle timeout.
    let _ = tokio::time::timeout(
        THE_RELAY_IS_TOLD_THE_SESSION_CLOSED_WITHIN,
        moq_client.close(),
    )
    .await;
}

/// Writes one track's objects onto the node's output, across every session
/// that reads it.
struct RelayTrackSourceSession {
    outputs: OutputWriter,
    max_age: Duration,
    track_timestamp_rebasing: TrackTimestampRebasedOntoThisMachinesClock,
    where_the_track_carries_its_timestamp: WhereATrackCarriesItsTimestamp,
    catalog_entry_set_on_the_output: Option<CatalogEntry>,
    objects_written: u64,
}

impl RelayTrackSourceSession {
    fn new(outputs: OutputWriter, max_age: Duration) -> Self {
        Self {
            outputs,
            max_age,
            track_timestamp_rebasing: TrackTimestampRebasedOntoThisMachinesClock::default(),
            where_the_track_carries_its_timestamp: WhereATrackCarriesItsTimestamp::OnTheObject,
            catalog_entry_set_on_the_output: None,
            objects_written: 0,
        }
    }

    /// Dial the relay once and write the track until the session, the
    /// broadcast or the track ends; why it ended.
    async fn read_through_one_session(
        &mut self,
        moq_client: &moq_tokio::Client,
        relay_track_location: &RelayTrackLocation,
    ) -> WhyARelayTrackSessionEnded {
        let subscribed_origin = moq_tokio::origin::spawn();
        let connection = moq_client
            .clone()
            .with_subscriber(subscribed_origin.clone())
            .connect(relay_track_location.relay_url.clone());
        let announced_broadcasts = subscribed_origin.consume();
        let broadcast = tokio::select! {
            ended = connection.closed() => {
                return WhyARelayTrackSessionEnded::of_a_closed_session(ended);
            }
            broadcast = announced_broadcasts
                .routed_broadcast(relay_track_location.broadcast_path.as_str()) => match broadcast {
                Ok(broadcast) => broadcast,
                Err(refusal) => {
                    return WhyARelayTrackSessionEnded::BroadcastWasRefused(
                        refusal.to_string(),
                    );
                }
            },
        };
        let (mut groups_of_the_track, mut catalog_of_the_broadcast) =
            match subscribe_to_the_track_and_its_catalog(
                &broadcast,
                relay_track_location,
                self.max_age,
            )
            .await
            {
                Ok(subscribed) => subscribed,
                Err(refusal) => {
                    return WhyARelayTrackSessionEnded::TrackWasRefused(refusal.to_string());
                }
            };
        if catalog_of_the_broadcast.is_some()
            && let Ok(Some(catalog)) = tokio::time::timeout(
                THE_FIRST_CATALOG_IS_AWAITED_FOR,
                the_next_catalog_of(&mut catalog_of_the_broadcast),
            )
            .await
        {
            self.take_the_tracks_entry_from(&catalog, &relay_track_location.track_name);
        }
        tracing::info!("RelayTrackSource: reading {relay_track_location}");

        let mut group_being_read: Option<moq_net::group::Consumer> = None;
        let mut the_next_object_starts_a_group = false;
        loop {
            // The catalog says where the track carries its timestamp, so an
            // update is taken before any object waiting beside it.
            tokio::select! {
                biased;
                catalog = the_next_catalog_of(&mut catalog_of_the_broadcast) => match catalog {
                    Some(catalog) => self
                        .take_the_tracks_entry_from(&catalog, &relay_track_location.track_name),
                    None => catalog_of_the_broadcast = None,
                },
                ended = connection.closed() => {
                    return WhyARelayTrackSessionEnded::of_a_closed_session(ended);
                }
                released = groups_of_the_track.next_released_group(), if group_being_read.is_none() => {
                    match released {
                        Ok(Some(ReleasedRelayTrackGroup::InSequence(group))) => {
                            group_being_read = Some(*group);
                            the_next_object_starts_a_group = true;
                        }
                        Ok(Some(ReleasedRelayTrackGroup::GivenUp { groups })) => {
                            tracing::debug!(
                                "RelayTrackSource: {groups} groups of {relay_track_location} \
                                 were given up"
                            );
                        }
                        Ok(None) => return WhyARelayTrackSessionEnded::TrackEnded(None),
                        Err(refusal) => {
                            return WhyARelayTrackSessionEnded::TrackEnded(Some(
                                refusal.to_string(),
                            ));
                        }
                    }
                }
                object = the_next_object_of(&mut group_being_read) => match object {
                    Ok(Some(object)) => {
                        let group_boundary =
                            if std::mem::take(&mut the_next_object_starts_a_group) {
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

/// Why one session reading a relay's track ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WhyARelayTrackSessionEnded {
    RelayClosedTheSession,
    SessionFailed(String),
    BroadcastWasRefused(String),
    TrackWasRefused(String),
    TrackEnded(Option<String>),
}

impl WhyARelayTrackSessionEnded {
    fn of_a_closed_session(ended: moq_tokio::Result<()>) -> Self {
        match ended {
            Ok(()) => Self::RelayClosedTheSession,
            Err(refusal) => Self::SessionFailed(refusal.to_string()),
        }
    }
}

impl std::fmt::Display for WhyARelayTrackSessionEnded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RelayClosedTheSession => formatter.write_str("the relay closed the session"),
            Self::SessionFailed(refusal) => write!(formatter, "the session failed: {refusal}"),
            Self::BroadcastWasRefused(refusal) => {
                write!(formatter, "the broadcast was refused: {refusal}")
            }
            Self::TrackWasRefused(refusal) => {
                write!(formatter, "the track was refused: {refusal}")
            }
            Self::TrackEnded(None) => formatter.write_str("the track ended"),
            Self::TrackEnded(Some(refusal)) => write!(formatter, "the track ended: {refusal}"),
        }
    }
}

/// The track's groups, and the broadcast's catalog when it serves one.
async fn subscribe_to_the_track_and_its_catalog(
    broadcast: &moq_net::broadcast::Consumer,
    relay_track_location: &RelayTrackLocation,
    max_age: Duration,
) -> moq_net::Result<(
    RelayTrackGroupsReleasedInSequenceOrder,
    Option<moq_json::snapshot::Consumer<Value>>,
)> {
    let groups_of_the_track = RelayTrackGroupsReleasedInSequenceOrder::subscribe(
        &broadcast.track(&relay_track_location.track_name)?,
        max_age,
    )
    .await?;
    let catalog_subscribed: moq_net::Result<_> = async {
        broadcast
            .track(hang::Catalog::DEFAULT_NAME)?
            .subscribe(None)
            .await
    }
    .await;
    let catalog_of_the_broadcast = match catalog_subscribed {
        Ok(catalog_subscriber) => Some(moq_json::snapshot::Consumer::new(
            catalog_subscriber,
            moq_json::snapshot::consumer::Config::default(),
        )),
        Err(refusal) => {
            tracing::debug!("RelayTrackSource: the broadcast serves no catalog: {refusal}");
            None
        }
    };
    Ok((groups_of_the_track, catalog_of_the_broadcast))
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
        let entry = catalog
            .get(section.as_catalog_key())?
            .get(the_hang_map_a_catalog_section_lists_its_tracks_under(
                section,
            ))?
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

/// How far a re-based stamp may fall behind the object's arrival — a track
/// whose time slowed or stalled — before the track is taken to have jumped and
/// the offset is taken again.
const A_REBASED_STAMP_FALLS_BEHIND_ARRIVAL_BY_AT_MOST: Duration = Duration::from_secs(1);

/// How far a re-based stamp may run ahead of the object's arrival. The stack's
/// own player lowers its reference for any frame that would lead by more than
/// its `buffer`, to lead by exactly that; the node does the same with the
/// allowance at 100 ms where the player's `buffer` defaults to zero, so arrival
/// jitter under it never shortens a step. The group a joining reader is handed
/// in one burst is written at its arrival, and a track that leaps ahead never
/// carries its stamps into this machine's future.
const A_REBASED_STAMP_RUNS_AHEAD_OF_ARRIVAL_BY_AT_MOST: Duration = Duration::from_millis(100);

/// How far a track's timestamp may step back — a reordered frame — before the
/// track is taken to have restarted and the offset is taken again.
const A_TRACKS_TIMESTAMP_STEPS_BACK_BY_AT_MOST: Duration = Duration::from_millis(250);

/// A track's own timestamps restated on this machine's clock: each plus one
/// offset taken at the first object, lowered when an object arrives earlier
/// than it allows, so the track's spacing is kept whatever jitter did to the
/// objects' arrival.
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

    /// A reader joining a track is handed the group in progress from its
    /// keyframe in one burst, here two seconds of it, then the live objects:
    /// the burst is written at its arrival, never stepping back, and the live
    /// objects keep the track's spacing within the jitter allowance of arrival.
    #[test]
    fn a_group_handed_over_in_a_burst_on_joining_is_written_at_arrival_and_the_live_edge_keeps_its_spacing()
     {
        let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
        let joined_at_ns = 50_000_000_000_i64;
        let cached_objects = 60_i64;
        let largest_lead_ns = A_REBASED_STAMP_RUNS_AHEAD_OF_ARRIVAL_BY_AT_MOST.as_nanos() as i64;
        let stamps_and_arrivals: Vec<(i64, i64)> = (0..cached_objects + 30)
            .map(|object_index| {
                let track_timestamp_ns = i128::from(object_index) * FRAME_SPACING_NS;
                let arrival_ns = if object_index < cached_objects {
                    joined_at_ns + object_index * 1_000_000
                } else {
                    let live_index = object_index - cached_objects;
                    joined_at_ns
                        + cached_objects * 1_000_000
                        + (i128::from(live_index) * FRAME_SPACING_NS) as i64
                        + (live_index * 7_919) % 16 * 1_000_000
                };
                (
                    rebasing.stamp_on_this_machines_clock(Some(track_timestamp_ns), arrival_ns),
                    arrival_ns,
                )
            })
            .collect();
        for pair in stamps_and_arrivals.windows(2) {
            assert!(
                pair[1].0 > pair[0].0,
                "the stamps never step back: {stamps_and_arrivals:?}"
            );
        }
        for (stamp, arrival) in &stamps_and_arrivals {
            assert!(
                stamp - arrival <= largest_lead_ns,
                "no stamp runs ahead of its arrival by more than the allowance: \
                 {stamps_and_arrivals:?}"
            );
        }
        let live_edge = &stamps_and_arrivals[cached_objects as usize..];
        for pair in live_edge.windows(2) {
            assert_eq!(
                i128::from(pair[1].0 - pair[0].0),
                FRAME_SPACING_NS,
                "the live edge keeps the track's spacing: {live_edge:?}"
            );
        }
    }

    #[test]
    fn a_track_that_falls_behind_its_arrival_by_more_than_a_second_is_re_anchored() {
        let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
        rebasing.stamp_on_this_machines_clock(Some(0), 1_000_000_000);
        let after_the_stall =
            rebasing.stamp_on_this_machines_clock(Some(FRAME_SPACING_NS), 2_500_000_000);
        assert_eq!(after_the_stall, 2_500_000_000);
    }

    #[test]
    fn a_track_that_leaps_ahead_of_its_arrival_is_stamped_within_the_allowance_of_arrival() {
        let largest_lead_ns = A_REBASED_STAMP_RUNS_AHEAD_OF_ARRIVAL_BY_AT_MOST.as_nanos() as i64;
        for leap_ns in [5_000_000_000_i128, 60_000_000_000] {
            let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
            rebasing.stamp_on_this_machines_clock(Some(0), 1_000_000_000);
            rebasing.stamp_on_this_machines_clock(Some(FRAME_SPACING_NS), 1_033_333_333);
            let after_the_leap =
                rebasing.stamp_on_this_machines_clock(Some(leap_ns), 1_066_666_666);
            assert_eq!(after_the_leap, 1_066_666_666 + largest_lead_ns);
            let next = rebasing
                .stamp_on_this_machines_clock(Some(leap_ns + FRAME_SPACING_NS), 1_099_999_999);
            assert_eq!(i128::from(next - after_the_leap), FRAME_SPACING_NS);
        }
    }

    #[test]
    fn an_object_arriving_earlier_than_the_offset_allows_lowers_it() {
        let mut rebasing = TrackTimestampRebasedOntoThisMachinesClock::default();
        let largest_lead_ns = A_REBASED_STAMP_RUNS_AHEAD_OF_ARRIVAL_BY_AT_MOST.as_nanos() as i64;
        rebasing.stamp_on_this_machines_clock(Some(0), 1_400_000_000);
        let early = rebasing.stamp_on_this_machines_clock(Some(FRAME_SPACING_NS), 1_033_333_333);
        assert_eq!(early, 1_033_333_333 + largest_lead_ns);
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
            "binary": {"tracks": {"camera/video": {
                "mode": "snapshot", "mime": "application/msgpack"
            }}},
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

    fn config_read_from(config: serde_json::Value) -> serde_json::Result<RelayTrackSourceConfig> {
        serde_json::from_value(config)
    }

    fn config_naming(broadcast: &str, track: &str) -> RelayTrackSourceConfig {
        config_read_from(json!({"broadcast": broadcast, "track": track}))
            .expect("a broadcast with a track is a config")
    }

    #[test]
    fn a_broadcast_and_a_track_are_read_exactly_as_written_at_the_local_relay_by_default() {
        for (broadcast, track) in [
            ("desk/camera-rig.hang", "encoder/encoded_video"),
            ("Desk/Main Room.hang", "Front Camera/video"),
            ("live/Rooms/42/east wing", "video0"),
            ("bbb", "audio/data/opus"),
        ] {
            let location = RelayTrackLocation::of(&config_naming(broadcast, track))
                .expect("a broadcast with a track locates a track");
            assert_eq!(location.relay_url.as_str(), "http://127.0.0.1:4443/");
            assert_eq!(location.broadcast_path, broadcast);
            assert_eq!(location.track_name, track);
        }
    }

    #[test]
    fn a_config_without_max_age_reads_with_one_hundred_milliseconds() {
        assert_eq!(
            config_naming("desk/main.hang", "microphone/audio").max_age,
            100
        );
        assert_eq!(
            config_read_from(json!({
                "broadcast": "desk/main.hang",
                "track": "microphone/audio",
                "max_age": 40,
            }))
            .expect("a whole number of milliseconds is a max_age")
            .max_age,
            40
        );
    }

    #[test]
    fn a_config_naming_a_key_the_node_does_not_take_is_refused_naming_it() {
        let refusal = config_read_from(json!({
            "broadcast": "desk/camera-rig.hang",
            "track": "encoder/encoded_video",
            "catalog": "catalog.json",
        }))
        .expect_err("`catalog` is no key of the config")
        .to_string();
        assert!(refusal.contains("unknown field `catalog`"), "{refusal}");
    }

    #[test]
    fn a_config_missing_the_broadcast_or_the_track_is_refused_naming_it() {
        for (config, missing_key) in [
            (json!({"track": "encoder/encoded_video"}), "broadcast"),
            (json!({"broadcast": "desk/camera-rig.hang"}), "track"),
        ] {
            let refusal = config_read_from(config)
                .expect_err("both keys are required")
                .to_string();
            assert!(
                refusal.contains(&format!("missing field `{missing_key}`")),
                "{refusal}"
            );
        }
    }

    #[test]
    fn an_empty_broadcast_or_track_or_no_config_at_all_is_refused_naming_it() {
        for (config, empty_key) in [
            (config_naming("", "video0"), "broadcast"),
            (config_naming("live/42", ""), "track"),
            (RelayTrackSourceConfig::default(), "broadcast"),
        ] {
            let refusal = RelayTrackLocation::of(&config)
                .expect_err("an empty name locates no track")
                .to_string();
            assert!(
                refusal.contains(&format!("`{empty_key}` is empty")),
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_relay_url_that_does_not_parse_is_refused_without_echoing_it() {
        let refusal = RelayTrackLocation::of(&RelayTrackSourceConfig {
            relay_url: "not a url?jwt=secret".into(),
            ..config_naming("desk/main.hang", "camera/video")
        })
        .expect_err("a relay URL that does not parse")
        .to_string();
        assert!(refusal.contains("`relay_url` is not a URL"), "{refusal}");
        assert!(!refusal.contains("secret"), "{refusal}");
    }

    #[test]
    fn a_located_track_names_its_relay_by_origin_alone() {
        let location = RelayTrackLocation::of(&RelayTrackSourceConfig {
            relay_url: "https://user:password@relay.example:4443/token-in-the-path?jwt=secret"
                .into(),
            ..config_naming("desk/main.hang", "camera/video")
        })
        .expect("the track is located");
        let shown = location.to_string();
        assert!(shown.ends_with("at https://relay.example:4443"), "{shown}");
        for credential in ["user", "password", "token-in-the-path", "secret"] {
            assert!(!shown.contains(credential), "{shown}");
        }
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
        assert_eq!(config_schema["properties"]["max_age"]["default"], 100);
        assert_eq!(config_schema["required"], json!(["broadcast", "track"]));
        assert_eq!(
            config_schema["properties"]
                .as_object()
                .expect("the config's properties")
                .keys()
                .collect::<Vec<_>>(),
            ["broadcast", "max_age", "relay_url", "track"]
        );
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
