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

use crate::encoded_media_sample::TrackMedium;
use crate::error::{MoqExtensionError, Result};
use crate::moq_relay_config::{MoqRelayConfig, moq_transport_subprotocol};
use crate::moq_track_sample::MoqTrackKind;

/// Cloudflare's relay idles a connection out at roughly 10–15 s, so the QUIC
/// layer speaks up well inside that. `web_transport`'s builder does not expose
/// the transport config, which is why the endpoint is assembled by hand.
const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(4);

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
/// The second backstop for the same broadcast, for the same two reasons, and
/// applied by the planner rather than here because only the planner knows
/// whether any track has published video. A sparse data track — one object a
/// second — would take minutes to reach the object bound, and a joiner
/// mid-group replays all of it; this bounds the replay to about a second. No
/// timer: a stamp comparison on the publisher's own monotonic clock at the
/// next write, so a broadcast that stops writing holds its last group open
/// until it writes again or closes. A broadcast with video never reaches it —
/// the video sync point cuts first.
pub(crate) const LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS: i64 = 1_000_000_000;

/// How many received objects may wait for the reading processor before the
/// drain stops pulling.
///
/// The transport offers no backpressure at all — a subgroup's object list is an
/// unbounded `Vec` that is never pruned — so this bound is the only one there
/// is. Set where a stall is visible as latency rather than as memory.
const OBJECTS_WAITING_FOR_THE_PROCESSOR: usize = 256;

/// Open the WebTransport session a MoQ session runs on.
///
/// TLS 1.3 with the platform's own roots, `h3` as the QUIC ALPN, and `moqt-16`
/// as the WebTransport subprotocol. There is no certificate-verification
/// bypass, because a dial that turns verification off is a dial.
async fn connect_web_transport_session(dial_url: url::Url) -> Result<web_transport::Session> {
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
    Ok(session.into())
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
    open_subgroup_by_track: HashMap<String, moq_transport::serve::SubgroupWriter>,
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
        let session = connect_web_transport_session(config.dial_url()?).await?;

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
            open_subgroup_by_track: HashMap::new(),
            _tracks_request: tracks_request,
            session_task,
            publish_namespace_task,
        })
    }

    /// Close every open group so the next object on each track opens a fresh
    /// one.
    ///
    /// The reference publisher cuts every track together on a video sync point,
    /// which is what makes a group a GOP across audio and video alike. Closing
    /// is what dropping the writer does: it finishes the subgroup's QUIC
    /// stream.
    pub(crate) fn open_a_new_group_on_every_track(&mut self) {
        self.open_subgroup_by_track.clear();
    }

    /// Write one object, opening a group first if none is open or the open one
    /// has reached the backstop.
    ///
    /// The priority is the track's own rung and is spent only when a group is
    /// opened: draft-16 carries `publisher_priority` in the subgroup header,
    /// so it is a property of the group and not of the object.
    pub(crate) fn write_object(
        &mut self,
        track_name: &str,
        payload: bytes::Bytes,
        publisher_priority: u8,
    ) -> Result<()> {
        let full_enough = self
            .open_subgroup_by_track
            .get(track_name)
            .is_some_and(|open| open.len() >= HIGHEST_OBJECTS_IN_ONE_GROUP);
        if full_enough {
            // Dropped before the next is created, never after: two live
            // subgroups on one track means the older one's objects are already
            // unreachable to a subscriber, which retains only the latest.
            self.open_subgroup_by_track.remove(track_name);
        }

        if !self.open_subgroup_by_track.contains_key(track_name) {
            let subgroups = subgroups_writer_for(&mut self.subgroups_by_track, track_name)?;
            // `append`, not `create`: the group id is the library's own
            // monotonic counter. Naming it from the producer's `group_index`
            // instead looks appealing and is a silent-loss trap — a lower id
            // than the latest yields a live writer whose objects never reach
            // the wire, and an audio stream, whose every packet is a sync
            // point, would open one group per packet where only the newest
            // survives. The producer's ordering pair rides the object instead.
            let opened = subgroups.append(publisher_priority).map_err(|failure| {
                MoqExtensionError::Transport {
                    what: format!("a MoQ group could not be opened on `{track_name}`: {failure}"),
                }
            })?;
            self.open_subgroup_by_track
                .insert(track_name.to_owned(), opened);
        }

        let open = self
            .open_subgroup_by_track
            .get_mut(track_name)
            .expect("the group was just opened");
        let written = open.write(payload);
        if let Err(failure) = written {
            // A failed write leaves the subgroup in a state the next object
            // cannot use, so it goes rather than being written into again.
            self.open_subgroup_by_track.remove(track_name);
            return Err(MoqExtensionError::Transport {
                what: format!("a MoQ object could not be written to `{track_name}`: {failure}"),
            });
        }
        Ok(())
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
        self.open_subgroup_by_track.clear();
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
        let session = connect_web_transport_session(config.dial_url()?).await?;

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
/// on, so pinned here rather than re-derived.
#[cfg(test)]
mod tests {
    use super::VIDEO_MEDIA_TRACK_PRIORITY;
    use moq_transport::serve::{ServeError, Track, TrackReaderMode};

    const A_NAMESPACE: &str = "streamlib/a-broadcast";
    const A_TRACK: &str = "1.m4s";

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
