// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The QUIC/WebTransport client and the two session shapes built on it.
//!
//! Three rules the shape here obeys:
//!
//! - The broadcast never appears in the dial URL. Draft-16 puts the relay's
//!   auth token in the path; the namespace travels in the protocol.
//! - The extended CONNECT advertises `moqt-16` as its subprotocol. Draft-16
//!   took version negotiation out of `CLIENT_SETUP`, so this is the only place
//!   the draft is stated, and a relay that will not speak it refuses here.
//! - One processor owns one session. There is no process-global registry for a
//!   second owner to reach through.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::delivery_deadline::UplinkBacklogReading;
use crate::encoded_media_sample::TrackMedium;
use crate::error::{MoqExtensionError, Result};
use crate::moq_relay_config::{MoqRelayConfig, moq_transport_subprotocol};
use crate::moq_track_sample::MoqTrackKind;

/// Cloudflare's relay idles a connection out at roughly 10–15 s, so the QUIC
/// layer speaks up well inside that. `web_transport`'s builder does not expose
/// the transport config, which is why the endpoint is assembled by hand.
const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(4);

/// The most bytes QUIC may hold unacknowledged on a session.
///
/// quinn's default is 10 MiB, which on a congested uplink absorbs seconds of
/// backlog before a write ever blocks — and a write that never blocks is a
/// backlog the forwarder's cursor never shows. Bounded to a few round trips
/// of a 1080p stream at a relay round trip of about 100 ms, so the cursor
/// falls behind within a frame interval of the link falling behind, and a
/// reset has at most this much stale data to discard. The cost is a ceiling
/// on throughput: a path with more than this in flight — about 40 Mbit/s at
/// 100 ms — is throttled to it.
const QUIC_SEND_WINDOW_BYTES: u64 = 512 * 1024;

// Draft-16 §10.4.2 reads a smaller `publisher_priority` as sooner. Video keeps
// `moq-pub`'s media literal of 127 and audio sits one rung ahead of it, so the
// broadcast stays interoperable while still saying which medium to prefer. The
// direction is read from the draft and has not been checked against a relay;
// a shaped-link run that finds the relay reading it the other way flips the
// two. The number reverses meaning once it leaves the header: `moq-transport`
// hands the same `u8` to quinn's `set_priority`, where larger is sooner, so no
// one value can rank audio first at both ends. This one is the statement to
// the relay.

/// The rung the catalog and init tracks are published at.
pub(crate) const DESCRIPTIVE_TRACK_PRIORITY: u8 = 0;
/// The rung an audio track's groups are opened at.
pub(crate) const AUDIO_MEDIA_TRACK_PRIORITY: u8 = 126;
/// The rung a video track's groups are opened at.
pub(crate) const VIDEO_MEDIA_TRACK_PRIORITY: u8 = 127;
/// The rung a data track's groups are opened at.
///
/// Video's rung as a placeholder, not a ranking: the ladder places audio ahead
/// of video and has not placed data, because whether a data object may ever be
/// dropped is undecided — so a data track rides the media literal it would
/// have ridden before the ladder existed.
pub(crate) const DATA_TRACK_PRIORITY: u8 = VIDEO_MEDIA_TRACK_PRIORITY;

/// Draft-16 §10.4.2 reads a smaller `publisher_priority` as sooner, so the
/// ladder is only a ladder while it descends.
const _: () = assert!(
    DESCRIPTIVE_TRACK_PRIORITY < AUDIO_MEDIA_TRACK_PRIORITY
        && AUDIO_MEDIA_TRACK_PRIORITY < VIDEO_MEDIA_TRACK_PRIORITY
);

/// The rung a track kind's groups are opened at.
pub(crate) fn track_priority_of(kind: MoqTrackKind) -> u8 {
    match kind {
        MoqTrackKind::Media(TrackMedium::Audio) => AUDIO_MEDIA_TRACK_PRIORITY,
        MoqTrackKind::Media(TrackMedium::Video) => VIDEO_MEDIA_TRACK_PRIORITY,
        MoqTrackKind::Data => DATA_TRACK_PRIORITY,
    }
}

/// How long a closing session may spend letting already-written objects reach
/// the wire before its control loop is aborted.
///
/// A helper's teardown reply and its exit are each bounded at five seconds, and
/// `teardown` has other work to do, so this takes a small share of the first.
const FINAL_DRAIN_BEFORE_ABORT: Duration = Duration::from_millis(750);

/// The most objects this publisher lets one MoQ group hold before cutting the
/// next one.
///
/// Not the primary cadence — a video sync point is, and it cuts every track at
/// once the way the reference does. This is the backstop for a broadcast with
/// no video in it, where nothing else would ever cut a group. It matters for
/// two reasons beyond tidiness: a subgroup retains every object it has ever
/// carried for as long as its writer lives, so an uncut group grows without
/// bound; and a subscriber joining mid-group gets that group from its first
/// object, so an endless group is an endless replay.
const HIGHEST_OBJECTS_IN_ONE_GROUP: usize = 128;

/// How old the open group of a broadcast with no video may be before the next
/// object cuts a new one.
///
/// The second backstop for the same broadcast, for the same two reasons: a
/// sparse data track — one object a second — would take minutes to reach the
/// object bound, and a joiner mid-group replays all of it. Applied by the
/// planner, which alone knows whether any track has published video, as a
/// stamp comparison on the publisher's own clock at the next write — no timer,
/// so a broadcast that stops writing holds its last group open until it writes
/// again or closes.
pub(crate) const LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS: i64 = 1_000_000_000;

/// How many received objects may wait for the reading processor before the
/// drain stops pulling.
///
/// The transport offers no backpressure at all — a subgroup's object list is an
/// unbounded `Vec` that is never pruned — so this bound is the only one there
/// is. Set where a stall is visible as latency rather than as memory.
const OBJECTS_WAITING_FOR_THE_PROCESSOR: usize = 256;

/// Open the WebTransport session a MoQ session runs on, and hand back a
/// second handle on it that still reaches the QUIC connection.
///
/// TLS 1.3 with the platform's own roots, `h3` as the QUIC ALPN, and `moqt-16`
/// as the WebTransport subprotocol. There is no certificate-verification
/// bypass, because a dial that turns verification off is a dial.
///
/// The generic `web_transport::Session` the MoQ session consumes hides the
/// connection under it; the `quinn`-flavoured handle it is built from does
/// not, and a clone of it kept beside the session is how the path's round
/// trip, congestion window and loss counters are read.
async fn connect_web_transport_session(
    dial_url: url::Url,
) -> Result<(web_transport::Session, web_transport::quinn::Session)> {
    let provider = web_transport::quinn::crypto::default_provider();

    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for certificate in native.certs {
        roots
            .add(certificate)
            .map_err(|failure| MoqExtensionError::Transport {
                what: format!("a system root certificate could not be loaded: {failure}"),
            })?;
    }
    if roots.is_empty() {
        return Err(MoqExtensionError::Transport {
            what: "no system root certificates were found, so no relay's certificate can be \
                   verified"
                .to_owned(),
        });
    }

    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|failure| MoqExtensionError::Transport {
            what: format!("the TLS 1.3 client config could not be built: {failure}"),
        })?
        .with_root_certificates(roots)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![web_transport::quinn::ALPN.as_bytes().to_vec()];

    let quic_crypto =
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).map_err(|failure| {
            MoqExtensionError::Transport {
                what: format!("the QUIC client config could not be built: {failure}"),
            }
        })?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(QUIC_KEEP_ALIVE_INTERVAL));
    transport.send_window(QUIC_SEND_WINDOW_BYTES);
    client_config.transport_config(Arc::new(transport));

    let endpoint = open_a_client_endpoint()?;

    let request = web_transport::quinn::proto::ConnectRequest::new(dial_url)
        .with_protocol(moq_transport_subprotocol()?);
    let session = web_transport::quinn::Client::new(endpoint, client_config)
        .connect(request)
        .await
        .map_err(|failure| MoqExtensionError::Transport {
            what: format!("the relay did not accept the WebTransport session: {failure}"),
        })?;
    Ok((session.clone().into(), session))
}

/// What the QUIC connection under a session reports about its path, read at
/// the moment of asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuicUplinkReadings {
    pub(crate) round_trip_time: Duration,
    pub(crate) congestion_window_bytes: u64,
    pub(crate) lost_packets: u64,
    pub(crate) congestion_events: u64,
}

/// One object as written into an open group: what a backlog reading is made
/// of once the forwarder is behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectWrittenToAnOpenGroup {
    timestamp_ns: i64,
    byte_count: usize,
}

/// A track's open group: its writer, and every object written into it in
/// order — one entry per `write`, which is what lets the writer's unforwarded
/// count be turned back into stamps and bytes.
struct OpenMoqGroup {
    writer: moq_transport::serve::SubgroupWriter,
    objects: Vec<ObjectWrittenToAnOpenGroup>,
}

impl OpenMoqGroup {
    fn uplink_backlog_reading(&self) -> UplinkBacklogReading {
        let Some(unforwarded_objects) = self.writer.unforwarded() else {
            return UplinkBacklogReading::default();
        };
        let first_unforwarded = self.objects.len().saturating_sub(unforwarded_objects);
        let unforwarded = &self.objects[first_unforwarded..];
        UplinkBacklogReading {
            unforwarded_objects: Some(unforwarded.len()),
            unforwarded_bytes: unforwarded.iter().map(|object| object.byte_count).sum(),
            oldest_unforwarded_stamp_ns: unforwarded.first().map(|object| object.timestamp_ns),
        }
    }
}

/// One superseded group a cut abandoned rather than finished, with what its
/// forwarder had not written when it was reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupTheUplinkBacklogAbandoned {
    pub(crate) moq_track_name: String,
    pub(crate) unforwarded_objects: usize,
    pub(crate) unforwarded_bytes: usize,
}

/// Every track's open group, keyed by MoQ track name.
#[derive(Default)]
struct OpenGroupsByTrack {
    open: HashMap<String, OpenMoqGroup>,
}

impl OpenGroupsByTrack {
    /// Append one object to the track's open group, opening one at the given
    /// rung first if none is open or the open one has reached the backstop.
    fn append(
        &mut self,
        track_name: &str,
        subgroups: &mut moq_transport::serve::SubgroupsWriter,
        payload: bytes::Bytes,
        publisher_priority: u8,
        object_stamp_ns: i64,
    ) -> Result<()> {
        let full_enough = self
            .open
            .get(track_name)
            .is_some_and(|open| open.writer.len() >= HIGHEST_OBJECTS_IN_ONE_GROUP);
        if full_enough {
            // Dropped before the next is created, never after: two live
            // subgroups on one track means the older one's objects are already
            // unreachable to a subscriber, which retains only the latest.
            self.open.remove(track_name);
        }

        if !self.open.contains_key(track_name) {
            // `append`, not `create`: the group id is the library's own
            // monotonic counter. Naming it from the producer's `group_index`
            // instead looks appealing and is a silent-loss trap — a lower id
            // than the latest yields a live writer whose objects never reach
            // the wire, and an audio stream, whose every packet is a sync
            // point, would open one group per packet where only the newest
            // survives. The producer's ordering pair rides the object instead.
            let writer = subgroups.append(publisher_priority).map_err(|failure| {
                MoqExtensionError::Transport {
                    what: format!("a MoQ group could not be opened on `{track_name}`: {failure}"),
                }
            })?;
            self.open.insert(
                track_name.to_owned(),
                OpenMoqGroup {
                    writer,
                    objects: Vec::new(),
                },
            );
        }

        let open = self
            .open
            .get_mut(track_name)
            .expect("the group was just opened");
        let written = ObjectWrittenToAnOpenGroup {
            timestamp_ns: object_stamp_ns,
            byte_count: payload.len(),
        };
        if let Err(failure) = open.writer.write(payload) {
            // A failed write leaves the subgroup in a state the next object
            // cannot use, so it goes rather than being written into again.
            self.open.remove(track_name);
            return Err(MoqExtensionError::Transport {
                what: format!("a MoQ object could not be written to `{track_name}`: {failure}"),
            });
        }
        open.objects.push(written);
        Ok(())
    }

    /// Close every open group so the next object on each track opens a fresh
    /// one: the named tracks' groups are abandoned — their unforwarded objects
    /// never leave, and the forwarder resets their streams with
    /// `DeliveryTimeout` — and every other is finished by dropping its writer.
    fn cut_every_group(
        &mut self,
        abandon_the_group_of: &[String],
    ) -> Vec<GroupTheUplinkBacklogAbandoned> {
        let mut abandoned = Vec::new();
        for (track_name, group) in self.open.drain() {
            if !abandon_the_group_of.contains(&track_name) {
                continue;
            }
            let reading = group.uplink_backlog_reading();
            match group
                .writer
                .abandon(moq_transport::data::DataStreamResetCode::DeliveryTimeout)
            {
                Ok(()) => abandoned.push(GroupTheUplinkBacklogAbandoned {
                    moq_track_name: track_name,
                    unforwarded_objects: reading.unforwarded_objects.unwrap_or(0),
                    unforwarded_bytes: reading.unforwarded_bytes,
                }),
                // Reachable only once the reader side is gone, in which case
                // there was no forwarder for the abandon to pre-empt and nothing
                // is lost — so it is neither counted nor raised above debug.
                Err(failure) => tracing::debug!(
                    track = %track_name,
                    %failure,
                    "the superseded MoQ group could not be abandoned; it finishes instead"
                ),
            }
        }
        abandoned
    }

    fn uplink_backlog_readings(&self) -> HashMap<String, UplinkBacklogReading> {
        self.open
            .iter()
            .map(|(track_name, group)| (track_name.clone(), group.uplink_backlog_reading()))
            .collect()
    }

    fn clear(&mut self) {
        self.open.clear();
    }
}

/// Open the UDP socket a QUIC client dials from.
///
/// The IPv6 unspecified address first, because on a dual-stack host it accepts
/// both families from one socket; a host built without IPv6 refuses to bind it
/// at all, and there the IPv4 unspecified address is the only one there is.
fn open_a_client_endpoint() -> Result<quinn::Endpoint> {
    let unspecified_ipv6 = "[::]:0"
        .parse()
        .expect("the unspecified IPv6 address parses");
    let unspecified_ipv4 = "0.0.0.0:0"
        .parse()
        .expect("the unspecified IPv4 address parses");

    match quinn::Endpoint::client(unspecified_ipv6) {
        Ok(endpoint) => Ok(endpoint),
        Err(ipv6_failure) => quinn::Endpoint::client(unspecified_ipv4).map_err(|ipv4_failure| {
            MoqExtensionError::Transport {
                what: format!(
                    "a QUIC endpoint could not be opened on either family: [::]:0 gave \
                     {ipv6_failure} and 0.0.0.0:0 gave {ipv4_failure}"
                ),
            }
        }),
    }
}

/// Run one MoQ session's control loop, saying what ended it.
///
/// Spawned rather than joined, but never with its `Result` discarded: a session
/// that dies takes every track with it, and the only account of why is here.
fn spawn_the_session_control_loop(
    session: moq_transport::session::Session,
    role: &'static str,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        match session.run().await {
            Ok(()) => tracing::info!(role, "the MoQ session closed"),
            Err(ended) if ended.is_graceful_close() => {
                tracing::info!(role, %ended, "the MoQ session was closed by the relay")
            }
            Err(ended) => tracing::warn!(role, %ended, "the MoQ session ended"),
        }
    })
}

/// One publishing session: one QUIC connection, one broadcast, N named tracks.
pub(crate) struct MoqBroadcastPublishingSession {
    subgroups_by_track: HashMap<String, moq_transport::serve::SubgroupsWriter>,
    open_groups_by_track: OpenGroupsByTrack,
    /// A second handle on the connection the MoQ session runs over, kept for
    /// what the generic session hides: the QUIC path's own readings.
    quic_connection: web_transport::quinn::Session,
    /// Held for the session's life. Dropping it answers every SUBSCRIBE for a
    /// track name that was not pre-created with `NotFound` — harmless while
    /// every track is created up front, and a silent outage the moment one is
    /// not.
    _tracks_request: moq_transport::serve::TracksRequest,
    session_task: tokio::task::JoinHandle<()>,
    publish_namespace_task: tokio::task::JoinHandle<()>,
}

impl MoqBroadcastPublishingSession {
    /// Connect, create every track up front, and announce the namespace.
    ///
    /// Every track is created before `PUBLISH_NAMESPACE` goes out so that a
    /// subscription arriving immediately finds what it asks for.
    pub(crate) async fn connect(config: MoqRelayConfig, track_names: Vec<String>) -> Result<Self> {
        let namespace = config.namespace()?;
        let (session, quic_connection) = connect_web_transport_session(config.dial_url()?).await?;

        let (session, mut publisher, _subscriber) = moq_transport::session::Session::connect(
            session,
            None,
            moq_transport::session::Transport::WebTransport,
        )
        .await
        .map_err(|failure| MoqExtensionError::Transport {
            what: format!("the MoQ publishing session did not open: {failure}"),
        })?;
        let session_task = spawn_the_session_control_loop(session, "publish");

        let (mut tracks_writer, tracks_request, tracks_reader) =
            moq_transport::serve::Tracks::new(namespace).produce();

        let mut subgroups_by_track = HashMap::new();
        for track_name in &track_names {
            // `create` overwrites a track of the same name without saying so,
            // orphaning its subscribers — so a duplicate is refused here.
            if subgroups_by_track.contains_key(track_name) {
                return Err(MoqExtensionError::Refused {
                    what: format!(
                        "two tracks in this broadcast are both named `{track_name}`; one \
                         would silently replace the other"
                    ),
                });
            }
            let track = tracks_writer.create(track_name.as_str()).ok_or_else(|| {
                MoqExtensionError::Transport {
                    what: format!("the MoQ track `{track_name}` could not be created"),
                }
            })?;
            let subgroups = track
                .subgroups()
                .map_err(|failure| MoqExtensionError::Transport {
                    what: format!(
                        "the MoQ track `{track_name}` could not enter subgroups mode: {failure}"
                    ),
                })?;
            subgroups_by_track.insert(track_name.clone(), subgroups);
        }

        // Draft-16 makes this a MUST for a publisher that wants subscriptions
        // routed to it, and it serves them for the session's whole life — so it
        // is spawned. Its `Result` is not discarded: an unanswered
        // PUBLISH_NAMESPACE expires after thirty seconds and the broadcast then
        // exists nowhere, with this line the only account of it.
        let publish_namespace_task = tokio::spawn(async move {
            match publisher.publish_namespace(tracks_reader).await {
                Ok(()) => tracing::info!("the MoQ broadcast stopped being announced"),
                Err(ended) => tracing::warn!(
                    %ended,
                    "the MoQ broadcast is no longer announced; no subscriber can reach it"
                ),
            }
        });

        tracing::info!(
            broadcast = %config.broadcast_path,
            tracks = track_names.len(),
            "the MoQ publishing session connected"
        );

        Ok(Self {
            subgroups_by_track,
            open_groups_by_track: OpenGroupsByTrack::default(),
            quic_connection,
            _tracks_request: tracks_request,
            session_task,
            publish_namespace_task,
        })
    }

    /// Close every open group so the next object on each track opens a fresh
    /// one, abandoning the named tracks' groups and finishing the rest.
    ///
    /// The reference publisher cuts every track together on a video sync point,
    /// which is what makes a group a GOP across audio and video alike.
    /// Finishing is what dropping the writer does: the forwarder drains what
    /// is written and FINs the subgroup's QUIC stream. Abandoning is the
    /// vendored crate's other exit: the forwarder writes nothing more and
    /// resets the stream with `DeliveryTimeout`, so a backlog the uplink is
    /// behind on stops being carried. What each abandoned group still owed is
    /// handed back for the planner to count.
    pub(crate) fn cut_a_new_group_on_every_track(
        &mut self,
        abandon_the_superseded_group_of: &[String],
    ) -> Vec<GroupTheUplinkBacklogAbandoned> {
        // Counted by the planner and said through the Python log, not logged
        // here: this crate's `tracing` events reach no dispatcher in a helper.
        self.open_groups_by_track
            .cut_every_group(abandon_the_superseded_group_of)
    }

    /// Write one object, opening a group first if none is open or the open one
    /// has reached the backstop.
    ///
    /// The priority is the track's own rung and is spent only when a group is
    /// opened: draft-16 carries `publisher_priority` in the subgroup header,
    /// so it is a property of the group and not of the object. The stamp is
    /// kept beside the object so the backlog reading can age it.
    pub(crate) fn write_object(
        &mut self,
        track_name: &str,
        payload: bytes::Bytes,
        publisher_priority: u8,
        object_stamp_ns: i64,
    ) -> Result<()> {
        let subgroups = subgroups_writer_for(&mut self.subgroups_by_track, track_name)?;
        self.open_groups_by_track.append(
            track_name,
            subgroups,
            payload,
            publisher_priority,
            object_stamp_ns,
        )
    }

    /// What every track's open group is behind by on the uplink, keyed by
    /// MoQ track name; a track with no open group is absent.
    pub(crate) fn uplink_backlog_readings(&self) -> HashMap<String, UplinkBacklogReading> {
        self.open_groups_by_track.uplink_backlog_readings()
    }

    /// What the QUIC path under this session reports right now.
    pub(crate) fn quic_uplink_readings(&self) -> QuicUplinkReadings {
        // The `quinn`-flavoured session has a `stats` of its own that hides
        // the congestion window, so the connection it derefs to is asked.
        let connection: &quinn::Connection = &self.quic_connection;
        let stats = connection.stats();
        QuicUplinkReadings {
            round_trip_time: connection.rtt(),
            congestion_window_bytes: stats.path.cwnd,
            lost_packets: stats.path.lost_packets,
            congestion_events: stats.path.congestion_events,
        }
    }

    /// Write one object as the whole of its own track — one group, one object,
    /// never rewritten.
    ///
    /// What the catalog and the init segment are. A subscriber joining at any
    /// later moment still receives it, because the transport retains a track's
    /// most recent group and for these there is only ever one.
    pub(crate) fn write_the_only_object_of(
        &mut self,
        track_name: &str,
        payload: bytes::Bytes,
    ) -> Result<()> {
        let subgroups = subgroups_writer_for(&mut self.subgroups_by_track, track_name)?;
        let mut group = subgroups
            .append(DESCRIPTIVE_TRACK_PRIORITY)
            .map_err(|failure| MoqExtensionError::Transport {
                what: format!("the `{track_name}` group could not be opened: {failure}"),
            })?;
        group
            .write(payload)
            .map_err(|failure| MoqExtensionError::Transport {
                what: format!("the `{track_name}` object could not be written: {failure}"),
            })
    }

    /// Finish every subgroup and end the session's tasks.
    /// Finish every open group, let what is already written reach the wire, and
    /// end the session's tasks.
    ///
    /// Dropping the writers finishes their subgroup streams, but it is the
    /// session's own control loop that forwards the bytes — so aborting it in
    /// the same breath discards whatever had not left yet. The wait is bounded
    /// well inside the helper's five-second teardown budget, and a session that
    /// ends on its own before the budget expires costs nothing.
    pub(crate) async fn close(mut self) {
        self.open_groups_by_track.clear();
        self.subgroups_by_track.clear();
        self.publish_namespace_task.abort();

        if tokio::time::timeout(FINAL_DRAIN_BEFORE_ABORT, &mut self.session_task)
            .await
            .is_err()
        {
            self.session_task.abort();
        }
    }
}

/// The subgroups writer for a track, or a refusal naming every track there is.
///
/// A free function taking the map rather than a method: the refusal reads the
/// map's keys, and building it inside an `ok_or_else` on `self` borrows `self`
/// twice.
fn subgroups_writer_for<'writers>(
    subgroups_by_track: &'writers mut HashMap<String, moq_transport::serve::SubgroupsWriter>,
    track_name: &str,
) -> Result<&'writers mut moq_transport::serve::SubgroupsWriter> {
    if !subgroups_by_track.contains_key(track_name) {
        return Err(MoqExtensionError::Refused {
            what: format!(
                "`{track_name}` is not a track of this broadcast; it names {}",
                describe_track_names(subgroups_by_track.keys())
            ),
        });
    }
    Ok(subgroups_by_track
        .get_mut(track_name)
        .expect("the key was just found"))
}

fn describe_track_names<'names>(names: impl Iterator<Item = &'names String>) -> String {
    let mut named: Vec<&str> = names.map(String::as_str).collect();
    named.sort_unstable();
    if named.is_empty() {
        "no tracks at all".to_owned()
    } else {
        named.join(", ")
    }
}

/// One object as it arrived, with the track that carried it.
pub(crate) struct ReceivedMoqObject {
    pub(crate) track_name: String,
    pub(crate) payload: bytes::Bytes,
}

/// What a drain task hands back: an object, or the reason there will be no
/// more.
type DrainedObject = std::result::Result<ReceivedMoqObject, String>;

/// One subscribing session: one QUIC connection, one broadcast, N tracks
/// draining into one queue.
pub(crate) struct MoqBroadcastSubscribingSession {
    received: tokio::sync::mpsc::Receiver<DrainedObject>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// One step of a track's drain loop, produced by the `select!` so that neither
/// arm has to mutate the state the other borrows.
enum DrainStep {
    ANewGroupOpened(moq_transport::serve::SubgroupReader),
    AnObjectArrived(bytes::Bytes),
    TheTrackEnded,
    ItFailed(String),
}

impl MoqBroadcastSubscribingSession {
    /// Connect and start draining every named track.
    pub(crate) async fn connect(config: MoqRelayConfig, track_names: Vec<String>) -> Result<Self> {
        let namespace = config.namespace()?;
        let (session, _quic_connection) = connect_web_transport_session(config.dial_url()?).await?;

        let (session, _publisher, subscriber) = moq_transport::session::Session::connect(
            session,
            None,
            moq_transport::session::Transport::WebTransport,
        )
        .await
        .map_err(|failure| MoqExtensionError::Transport {
            what: format!("the MoQ subscribing session did not open: {failure}"),
        })?;

        let (sender, received) = tokio::sync::mpsc::channel(OBJECTS_WAITING_FOR_THE_PROCESSOR);
        let mut tasks = Vec::with_capacity(track_names.len() * 2 + 1);

        // Losing the connection unblocks nothing on its own: no reader is
        // closed, and both the drain and the subscribe future wait forever.
        // The session's own end is therefore the terminal event, and this is
        // what turns it into one the reading processor can see.
        let session_ended = sender.clone();
        tasks.push(tokio::spawn(async move {
            let ended = match session.run().await {
                Ok(()) => "the relay closed the MoQ session".to_owned(),
                Err(failure) if failure.is_graceful_close() => {
                    format!("the relay closed the MoQ session: {failure}")
                }
                Err(failure) => format!("the MoQ session ended: {failure}"),
            };
            tracing::info!(%ended, "the MoQ subscribing session is over");
            let _ = session_ended.send(Err(ended)).await;
        }));

        for track_name in track_names {
            let (writer, reader) =
                moq_transport::serve::Track::new(namespace.clone(), track_name.as_str()).produce();

            // `subscribe` does not start the subscription loop — it returns
            // only once the subscription is over — so awaiting it here would
            // deadlock before a single object was read.
            let mut subscribing = subscriber.clone();
            let subscribed_track = track_name.clone();
            tasks.push(tokio::spawn(async move {
                match subscribing.subscribe(writer).await {
                    Ok(()) => tracing::info!(track = %subscribed_track, "the subscription ended"),
                    Err(failure) => tracing::warn!(
                        track = %subscribed_track,
                        %failure,
                        "the subscription ended in failure"
                    ),
                }
            }));

            let draining = sender.clone();
            tasks.push(tokio::spawn(async move {
                drain_one_track(track_name, reader, draining).await
            }));
        }

        tracing::info!(
            broadcast = %config.broadcast_path,
            "the MoQ subscribing session connected"
        );

        Ok(Self { received, tasks })
    }

    /// The next object, or `None` if none arrived inside `timeout`.
    ///
    /// The timeout is what lets a reading thread notice it has been asked to
    /// stop without waiting on a broadcast that may never send again.
    pub(crate) async fn next_object(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ReceivedMoqObject>> {
        match tokio::time::timeout(timeout, self.received.recv()).await {
            Err(_elapsed) => Ok(None),
            Ok(None) => Err(MoqExtensionError::BroadcastEnded {
                what: "every track's drain ended".to_owned(),
            }),
            Ok(Some(Ok(object))) => Ok(Some(object)),
            Ok(Some(Err(ended))) => Err(MoqExtensionError::BroadcastEnded { what: ended }),
        }
    }

    /// Stop draining and drop the connection.
    pub(crate) fn close(self) {
        for task in self.tasks {
            task.abort();
        }
    }
}

/// Read one track's objects in order, for as long as it lasts.
///
/// The shape is load-bearing. A subscriber holds only the newest subgroup a
/// track has produced, so a loop that takes a subgroup, drains it to its end
/// and only then asks for the next stops asking for the whole time it is
/// draining — and every group opened meanwhile is gone with no error anywhere.
/// Asking for the next group and reading the current one are raced instead,
/// and the objects that have already arrived in the old group are
/// drained before the new one takes its place, so the order a producer wrote in
/// is the order that leaves here.
async fn drain_one_track(
    track_name: String,
    reader: moq_transport::serve::TrackReader,
    sender: tokio::sync::mpsc::Sender<DrainedObject>,
) {
    let mut subgroups = match reader.mode().await {
        Ok(moq_transport::serve::TrackReaderMode::Subgroups(subgroups)) => subgroups,
        Ok(_other_mode) => {
            let _ = sender
                .send(Err(format!(
                    "`{track_name}` is not published as subgroups, and subgroups is what a MoQ \
                     media track carries"
                )))
                .await;
            return;
        }
        Err(failure) => {
            let _ = sender
                .send(Err(describe_track_end(&track_name, &failure)))
                .await;
            return;
        }
    };

    let mut open_group: Option<moq_transport::serve::SubgroupReader> = None;
    loop {
        let step = match open_group.as_mut() {
            None => match subgroups.next().await {
                Ok(Some(opened)) => DrainStep::ANewGroupOpened(opened),
                Ok(None) => DrainStep::TheTrackEnded,
                Err(failure) => DrainStep::ItFailed(describe_track_end(&track_name, &failure)),
            },
            Some(group) => tokio::select! {
                biased;
                opened = subgroups.next() => match opened {
                    Ok(Some(opened)) => DrainStep::ANewGroupOpened(opened),
                    Ok(None) => DrainStep::TheTrackEnded,
                    Err(failure) => DrainStep::ItFailed(describe_track_end(&track_name, &failure)),
                },
                object = group.read_next() => match object {
                    Ok(Some(payload)) => DrainStep::AnObjectArrived(payload),
                    // The group is finished, not the track: wait for the next.
                    Ok(None) => DrainStep::ANewGroupOpened(match subgroups.next().await {
                        Ok(Some(opened)) => opened,
                        Ok(None) => return end_the_drain(&track_name, &sender, None).await,
                        Err(failure) => {
                            return end_the_drain(&track_name, &sender, Some(failure)).await;
                        }
                    }),
                    Err(failure) => DrainStep::ItFailed(describe_track_end(&track_name, &failure)),
                },
            },
        };

        match step {
            DrainStep::AnObjectArrived(payload) => {
                if send_one_object(&track_name, &sender, payload)
                    .await
                    .is_err()
                {
                    return;
                }
            }
            DrainStep::ANewGroupOpened(opened) => {
                if let Some(mut superseded) = open_group.take() {
                    // Whatever of the old group has already arrived is still in
                    // publication order and still ahead of the new group's
                    // first object, so it goes out before the swap.
                    while superseded.pos() < superseded.len() {
                        match superseded.read_next().await {
                            Ok(Some(payload)) => {
                                if send_one_object(&track_name, &sender, payload)
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Ok(None) => break,
                            Err(failure) => {
                                tracing::debug!(
                                    track = %track_name,
                                    %failure,
                                    "the superseded MoQ group could not be finished"
                                );
                                break;
                            }
                        }
                    }
                }
                open_group = Some(opened);
            }
            DrainStep::TheTrackEnded => return end_the_drain(&track_name, &sender, None).await,
            DrainStep::ItFailed(ended) => {
                let _ = sender.send(Err(ended)).await;
                return;
            }
        }
    }
}

/// Hand one object to the reading processor, or give up because it is gone.
async fn send_one_object(
    track_name: &str,
    sender: &tokio::sync::mpsc::Sender<DrainedObject>,
    payload: bytes::Bytes,
) -> std::result::Result<(), ()> {
    sender
        .send(Ok(ReceivedMoqObject {
            track_name: track_name.to_owned(),
            payload,
        }))
        .await
        .map_err(|_| ())
}

async fn end_the_drain(
    track_name: &str,
    sender: &tokio::sync::mpsc::Sender<DrainedObject>,
    failure: Option<moq_transport::serve::ServeError>,
) {
    let ended = match failure {
        Some(failure) => describe_track_end(track_name, &failure),
        None => format!("`{track_name}` ended"),
    };
    let _ = sender.send(Err(ended)).await;
}

/// Why a track stopped, in words that separate a clean end from a fault.
///
/// A clean end arrives as `ServeError::Done` rather than as an absent object,
/// so the two have to be told apart here or a finished broadcast reads as a
/// failure and a failure reads as a finish.
fn describe_track_end(track_name: &str, failure: &moq_transport::serve::ServeError) -> String {
    match failure {
        moq_transport::serve::ServeError::Done => format!("`{track_name}` ended"),
        other => format!("`{track_name}` stopped: {other}"),
    }
}

/// What the transport does with an object the publisher has already written —
/// undocumented in `moq-transport`, and what [`crate::delivery_deadline`] rests
/// on, so pinned here rather than re-derived — and what this wheel's own
/// open-group bookkeeping makes of the vendored crate's forwarder cursor.
#[cfg(test)]
mod tests {
    use super::{
        AUDIO_MEDIA_TRACK_PRIORITY, GroupTheUplinkBacklogAbandoned, OpenGroupsByTrack,
        VIDEO_MEDIA_TRACK_PRIORITY,
    };
    use crate::delivery_deadline::UplinkBacklogReading;
    use moq_transport::data::DataStreamResetCode;
    use moq_transport::serve::{
        ServeError, SubgroupsReader, SubgroupsWriter, Track, TrackReaderMode,
    };

    const A_NAMESPACE: &str = "streamlib/a-broadcast";
    const A_TRACK: &str = "1.m4s";

    /// A track's subgroups writer and the reader on the far side of it.
    async fn a_tracks_subgroups() -> (SubgroupsWriter, SubgroupsReader) {
        let namespace = moq_transport::coding::TrackNamespace::try_from(A_NAMESPACE)
            .expect("the namespace parses");
        let (track_writer, track_reader) = Track::new(namespace, A_TRACK).produce();
        let subgroups_writer = track_writer
            .subgroups()
            .expect("a fresh track enters subgroups mode");
        let TrackReaderMode::Subgroups(subgroups_reader) =
            track_reader.mode().await.expect("the track reads back")
        else {
            panic!("a track published as subgroups reads back as subgroups");
        };
        (subgroups_writer, subgroups_reader)
    }

    fn a_payload_of(byte_count: usize) -> bytes::Bytes {
        bytes::Bytes::from(vec![0xAB_u8; byte_count])
    }

    #[tokio::test]
    async fn the_backlog_reading_is_nobody_forwarding_until_a_forwarder_starts() {
        let (mut subgroups, _reader) = a_tracks_subgroups().await;
        let mut open = OpenGroupsByTrack::default();

        open.append(
            A_TRACK,
            &mut subgroups,
            a_payload_of(100),
            VIDEO_MEDIA_TRACK_PRIORITY,
            1_000,
        )
        .expect("the object is written");
        open.append(
            A_TRACK,
            &mut subgroups,
            a_payload_of(200),
            VIDEO_MEDIA_TRACK_PRIORITY,
            2_000,
        )
        .expect("the object is written");

        assert_eq!(
            open.uplink_backlog_readings(),
            std::collections::HashMap::from([(
                A_TRACK.to_owned(),
                UplinkBacklogReading {
                    unforwarded_objects: None,
                    unforwarded_bytes: 0,
                    oldest_unforwarded_stamp_ns: None,
                },
            )])
        );
    }

    #[tokio::test]
    async fn the_backlog_reading_turns_the_forwarder_cursor_into_stamps_and_bytes() {
        let (mut subgroups, mut reader) = a_tracks_subgroups().await;
        let mut open = OpenGroupsByTrack::default();
        for (stamp_ns, byte_count) in [(1_000, 100), (2_000, 200), (3_000, 300)] {
            open.append(
                A_TRACK,
                &mut subgroups,
                a_payload_of(byte_count),
                VIDEO_MEDIA_TRACK_PRIORITY,
                stamp_ns,
            )
            .expect("the object is written");
        }

        // A forwarder that has written the first object and is on the second.
        let mut forwarding = reader
            .next()
            .await
            .expect("the opened group reaches the reader")
            .expect("there is an opened group");
        forwarding.mark_forwarding_started();
        forwarding
            .read_next()
            .await
            .expect("the first object reads");
        forwarding.mark_forwarded();

        assert_eq!(
            open.uplink_backlog_readings()[A_TRACK],
            UplinkBacklogReading {
                unforwarded_objects: Some(2),
                unforwarded_bytes: 500,
                oldest_unforwarded_stamp_ns: Some(2_000),
            }
        );
    }

    #[tokio::test]
    async fn a_cut_abandons_the_named_groups_with_delivery_timeout_and_finishes_the_rest() {
        let (mut video_subgroups, mut video_reader) = a_tracks_subgroups().await;
        let (mut audio_subgroups, mut audio_reader) = a_tracks_subgroups().await;
        let mut open = OpenGroupsByTrack::default();
        for stamp_ns in [1_000, 2_000, 3_000] {
            open.append(
                "video",
                &mut video_subgroups,
                a_payload_of(1_000),
                VIDEO_MEDIA_TRACK_PRIORITY,
                stamp_ns,
            )
            .expect("the object is written");
        }
        open.append(
            "audio",
            &mut audio_subgroups,
            a_payload_of(50),
            AUDIO_MEDIA_TRACK_PRIORITY,
            1_000,
        )
        .expect("the object is written");
        let mut video_forwarding = video_reader.next().await.unwrap().unwrap();
        video_forwarding.mark_forwarding_started();
        video_forwarding
            .read_next()
            .await
            .expect("the first object reads");
        video_forwarding.mark_forwarded();
        let mut audio_forwarding = audio_reader.next().await.unwrap().unwrap();
        audio_forwarding.mark_forwarding_started();

        let abandoned = open.cut_every_group(&["video".to_owned()]);

        assert_eq!(
            abandoned,
            vec![GroupTheUplinkBacklogAbandoned {
                moq_track_name: "video".to_owned(),
                unforwarded_objects: 2,
                unforwarded_bytes: 2_000,
            }]
        );
        assert!(
            open.uplink_backlog_readings().is_empty(),
            "nothing is open after a cut"
        );
        // The abandoned group's forwarder is told so ahead of the two objects
        // it had not written; the finished group's forwarder drains and ends.
        assert!(matches!(
            video_forwarding.read_next().await,
            Err(ServeError::Abandoned(DataStreamResetCode::DeliveryTimeout))
        ));
        assert_eq!(
            audio_forwarding
                .read_next()
                .await
                .expect("the object reads"),
            Some(a_payload_of(50))
        );
        assert_eq!(
            audio_forwarding.read_next().await.expect("the group ends"),
            None
        );
    }

    /// What the forwarder races each chunk write against: resolves on an
    /// abandon with its code, and never for a group that finishes instead.
    #[tokio::test]
    async fn until_abandoned_resolves_with_the_abandons_code_and_never_on_a_finish() {
        let (mut subgroups, mut reader) = a_tracks_subgroups().await;
        let mut open = OpenGroupsByTrack::default();
        open.append(
            A_TRACK,
            &mut subgroups,
            a_payload_of(10),
            VIDEO_MEDIA_TRACK_PRIORITY,
            1,
        )
        .expect("the object is written");
        let abandoned_group = reader.next().await.unwrap().unwrap();
        let waiting = tokio::spawn(async move { abandoned_group.until_abandoned().await });
        tokio::task::yield_now().await;

        open.cut_every_group(&[A_TRACK.to_owned()]);

        assert_eq!(
            waiting.await.expect("the wait finishes"),
            DataStreamResetCode::DeliveryTimeout
        );

        open.append(
            A_TRACK,
            &mut subgroups,
            a_payload_of(10),
            VIDEO_MEDIA_TRACK_PRIORITY,
            2,
        )
        .expect("the next group opens");
        let finished_group = reader.next().await.unwrap().unwrap();
        open.cut_every_group(&[]);
        let never = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            finished_group.until_abandoned(),
        )
        .await;
        assert!(never.is_err(), "a finished group was never abandoned");
    }

    #[tokio::test]
    async fn a_cut_that_abandons_nothing_finishes_every_group_as_before() {
        let (mut subgroups, mut reader) = a_tracks_subgroups().await;
        let mut open = OpenGroupsByTrack::default();
        open.append(
            A_TRACK,
            &mut subgroups,
            a_payload_of(10),
            VIDEO_MEDIA_TRACK_PRIORITY,
            1,
        )
        .expect("the object is written");
        let mut forwarding = reader.next().await.unwrap().unwrap();

        assert!(open.cut_every_group(&[]).is_empty());

        assert_eq!(
            forwarding.read_next().await.unwrap(),
            Some(a_payload_of(10))
        );
        assert_eq!(forwarding.read_next().await.unwrap(), None);
    }

    /// The writer, and the reader positioned on the group it opened.
    async fn one_open_group() -> (
        moq_transport::serve::SubgroupsWriter,
        moq_transport::serve::SubgroupWriter,
        moq_transport::serve::SubgroupReader,
    ) {
        let namespace = moq_transport::coding::TrackNamespace::try_from(A_NAMESPACE)
            .expect("the namespace parses");
        let (track_writer, track_reader) = Track::new(namespace, A_TRACK).produce();
        let mut subgroups_writer = track_writer
            .subgroups()
            .expect("a fresh track enters subgroups mode");
        let open = subgroups_writer
            .append(VIDEO_MEDIA_TRACK_PRIORITY)
            .expect("a group opens");
        let TrackReaderMode::Subgroups(mut subgroups_reader) =
            track_reader.mode().await.expect("the track reads back")
        else {
            panic!("a track published as subgroups reads back as subgroups");
        };
        let open_reader = subgroups_reader
            .next()
            .await
            .expect("the opened group reaches the reader")
            .expect("there is an opened group");
        (subgroups_writer, open, open_reader)
    }

    #[tokio::test]
    async fn closing_a_group_delivers_every_object_already_written_to_it_before_the_close() {
        let (_subgroups, mut open, mut reader) = one_open_group().await;
        open.write(bytes::Bytes::from_static(b"first"))
            .expect("the first object is written");
        open.write(bytes::Bytes::from_static(b"second"))
            .expect("the second object is written");

        open.close(ServeError::Cancel)
            .expect("a group closes with an error");

        // This is why the drop policy decides before the write: `close` cannot
        // pre-empt a backlog, so the forwarder puts every stale object on the
        // wire and only then resets the stream.
        assert_eq!(
            reader.read_next().await.expect("the first object reads"),
            Some(bytes::Bytes::from_static(b"first"))
        );
        assert_eq!(
            reader.read_next().await.expect("the second object reads"),
            Some(bytes::Bytes::from_static(b"second"))
        );
        assert_eq!(reader.read_next().await, Err(ServeError::Cancel));
    }

    #[tokio::test]
    async fn a_group_whose_writer_is_dropped_ends_cleanly_once_its_objects_are_read() {
        let (_subgroups, mut open, mut reader) = one_open_group().await;
        open.write(bytes::Bytes::from_static(b"only"))
            .expect("the object is written");

        drop(open);

        assert_eq!(
            reader.read_next().await.expect("the object reads"),
            Some(bytes::Bytes::from_static(b"only"))
        );
        // No error: the subgroup finished, which is what FINs its QUIC stream.
        assert_eq!(reader.read_next().await.expect("the group ends"), None);
    }
}
