// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Received MoQ objects turned back into the samples a bag is spelled from.
//!
//! The two containers lose different things, and this is where the difference
//! shows. `streamlib_bag` hands back the producer's own keys — the ordering
//! pair and the stamp included — because they rode the object. `cmaf` carries
//! neither: an ISOBMFF fragment states a decode time and a sync flag and
//! nothing else, so the pair is minted here on the subscriber's own counter and
//! the stamp is anchored to the subscriber's own clock. Those two bags are
//! equally decodable and only one of them is the producer's.

use std::collections::{BTreeSet, VecDeque};
use std::num::NonZeroU32;
use std::time::Duration;

use crate::annex_b_access_unit::annex_b_access_unit_from_length_prefixed_sample;
use crate::cmaf_fragment::{CmafFragmentSample, read_cmaf_fragment};
use crate::cmaf_init_segment_reader::read_cmaf_init_segment;
use crate::cmaf_track_timeline::OPUS_TRACK_TIMESCALE_HZ;
use crate::encoded_media_sample::{
    EncodedAudioPacket, EncodedMediaSample, EncodedVideoAccessUnit, TrackMedium,
};
use crate::error::{MoqExtensionError, Result};
use crate::monotonic_clock::monotonic_now_ns;
use crate::moq_broadcast_catalog::{CMAF_PACKAGING, INIT_TRACK_NAME};
use crate::moq_broadcast_publisher::MoqContainerFormat;
use crate::moq_relay_config::MoqRelayConfig;
use crate::moq_session::MoqBroadcastSubscribingSession;
use crate::moq_track_sample::{DataTrackObject, MoqTrackKind, MoqTrackSample};
use crate::streamlib_bag_object::decode_object;

/// What a refusal from this path calls the session it was reading.
const SUBSCRIBING_SESSION_ROLE: &str = "subscribing";

/// One subscribed broadcast, read as encoded bags and data objects.
pub(crate) struct MoqBroadcastSubscriber {
    relay_config: MoqRelayConfig,
    /// Every track the session subscribes to, in the order it opens them: the
    /// init track first on `cmaf`, because nothing else can be decoded until it
    /// has arrived.
    subscribed_track_names: Vec<String>,
    received_object_router: ReceivedMoqObjectToEncodedSampleRouter,
    subscribing_session: Option<MoqBroadcastSubscribingSession>,
    /// One CMAF object may carry more than one sample, and `next_sample` hands
    /// back one. The rest wait here rather than being dropped.
    samples_awaiting_the_reader: VecDeque<MoqTrackSample>,
}

impl MoqBroadcastSubscriber {
    /// Describe a subscription: where to connect, which container to read, and
    /// which track carries each kind — video, audio, data.
    pub(crate) fn new(
        relay_config: MoqRelayConfig,
        container_format: MoqContainerFormat,
        video_track_name: Option<String>,
        audio_track_name: Option<String>,
        data_track_name: Option<String>,
    ) -> Result<Self> {
        if video_track_name.is_none() && audio_track_name.is_none() && data_track_name.is_none() {
            return Err(MoqExtensionError::Refused {
                what: "a subscriber naming no track subscribes to nothing and publishes nothing, \
                       which reads from outside as a hang — name at least one of `video_track`, \
                       `audio_track` and `data_track`"
                    .to_owned(),
            });
        }
        let configured_track_names = [
            (MoqTrackKind::Media(TrackMedium::Video), &video_track_name),
            (MoqTrackKind::Media(TrackMedium::Audio), &audio_track_name),
            (MoqTrackKind::Data, &data_track_name),
        ];
        for (kind, configured_track_name) in configured_track_names {
            if configured_track_name.as_deref().is_some_and(str::is_empty) {
                return Err(MoqExtensionError::Refused {
                    what: format!(
                        "`{kind}_track` is the empty string, which names no track on the relay; \
                         leave it unset to subscribe to no {kind} at all",
                        kind = kind.as_str()
                    ),
                });
            }
        }
        for (index, (kind, configured_track_name)) in configured_track_names.iter().enumerate() {
            let Some(track_name) = configured_track_name.as_deref() else {
                continue;
            };
            if let Some((other_kind, _)) = configured_track_names[index + 1..]
                .iter()
                .find(|(_, other_track_name)| other_track_name.as_deref() == Some(track_name))
            {
                return Err(MoqExtensionError::Refused {
                    what: format!(
                        "`{}_track` and `{}_track` are both `{track_name}`, and one track is one \
                         kind, so every object on it would be read twice under two different \
                         contracts",
                        kind.as_str(),
                        other_kind.as_str()
                    ),
                });
            }
        }
        if container_format == MoqContainerFormat::Cmaf && data_track_name.is_some() {
            return Err(MoqExtensionError::Refused {
                what: "`data_track` names a data track, and the `cmaf` container has no packaging \
                       for one; a data track rides `streamlib_bag` only"
                    .to_owned(),
            });
        }

        let mut subscribed_track_names = Vec::with_capacity(4);
        if container_format == MoqContainerFormat::Cmaf {
            // First, and subscribed to even when it is the only track that ever
            // sends: a fragment is undecodable without the parameter sets, the
            // coded extent and the Opus parameters this object carries.
            subscribed_track_names.push(INIT_TRACK_NAME.to_owned());
        }
        subscribed_track_names.extend(video_track_name.iter().cloned());
        subscribed_track_names.extend(audio_track_name.iter().cloned());
        subscribed_track_names.extend(data_track_name.iter().cloned());

        Ok(Self {
            relay_config,
            subscribed_track_names,
            received_object_router: ReceivedMoqObjectToEncodedSampleRouter::of(
                container_format,
                video_track_name,
                audio_track_name,
                data_track_name,
            ),
            subscribing_session: None,
            samples_awaiting_the_reader: VecDeque::new(),
        })
    }

    /// Open the QUIC connection and start draining every subscribed track.
    pub(crate) async fn connect(&mut self) -> Result<()> {
        if self.subscribing_session.is_some() {
            return Err(MoqExtensionError::Refused {
                what: "this subscriber is already connected; a second connection would leave the \
                       first draining a broadcast nothing reads"
                    .to_owned(),
            });
        }
        self.subscribing_session = Some(
            MoqBroadcastSubscribingSession::connect(
                self.relay_config.clone(),
                self.subscribed_track_names.clone(),
            )
            .await?,
        );
        Ok(())
    }

    /// The next sample — an encoded bag, or a data track's object — or `None`
    /// if none arrived inside `timeout`.
    ///
    /// `None` is also what an object that is not itself a sample returns — the
    /// init segment, an object on a track this subscriber did not name — so a
    /// caller polls rather than treating one `None` as end of stream.
    pub(crate) async fn next_sample(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<MoqTrackSample>> {
        if let Some(sample) = self.samples_awaiting_the_reader.pop_front() {
            return Ok(Some(sample));
        }
        let subscribing_session =
            self.subscribing_session
                .as_mut()
                .ok_or(MoqExtensionError::NotConnected {
                    role: SUBSCRIBING_SESSION_ROLE,
                })?;
        let Some(received_object) = subscribing_session.next_object(timeout).await? else {
            return Ok(None);
        };

        let mut reconstituted = self
            .received_object_router
            .route_received_object(&received_object.track_name, &received_object.payload)?
            .into_iter();
        let first = reconstituted.next();
        self.samples_awaiting_the_reader.extend(reconstituted);
        Ok(first)
    }

    /// Whether the QUIC connection is open.
    pub(crate) fn is_connected(&self) -> bool {
        self.subscribing_session.is_some()
    }

    /// Drop the connection. Whatever arrived and was never read goes with it.
    ///
    /// The subscriber stays reusable: a later `connect` reads a whole new
    /// broadcast, whose init segment and whose track epochs are its own.
    pub(crate) fn close(&mut self) {
        if let Some(subscribing_session) = self.subscribing_session.take() {
            subscribing_session.close();
        }
        let unread_decoded_bags = self.samples_awaiting_the_reader.len();
        if unread_decoded_bags > 0 {
            tracing::info!(
                unread_decoded_bags,
                "the subscriber closed with bags it had decoded and nothing had read yet"
            );
        }
        self.samples_awaiting_the_reader.clear();
        self.received_object_router
            .forget_what_the_closed_broadcast_described();
    }
}

/// Everything that turns one arriving object into bags, and nothing that
/// touches a socket — so the object source is a seam a test feeds by hand.
struct ReceivedMoqObjectToEncodedSampleRouter {
    container_format: MoqContainerFormat,
    video_track_name: Option<String>,
    audio_track_name: Option<String>,
    data_track_name: Option<String>,
    the_init_segment_has_arrived: bool,
    video_track_reconstitution: Option<CmafVideoTrackReconstitution>,
    audio_track_reconstitution: Option<CmafAudioTrackReconstitution>,
    /// A CMAF media object that arrived before the init segment is dropped, not
    /// held: replaying it would need an unbounded buffer for a loss the relay
    /// bounds anyway — the init track's latest group is retained forever, so it
    /// arrives at the join and never again. Counted so the loss is accounted.
    media_objects_dropped_before_the_init_segment_arrived: u64,
    the_pre_init_drop_has_been_reported: bool,
    track_names_reported_as_unnamed: BTreeSet<String>,
    the_stray_track_name_cap_has_been_reported: bool,
}

/// How many distinct stray track names one subscriber holds to keep its
/// reporting once-per-track. Bounded because the relay chooses the names: a
/// far end misrouting objects under freshly minted names would otherwise grow
/// this set for as long as the subscription lives.
const MOST_STRAY_TRACK_NAMES_HELD_TO_REPORT_EACH_ONCE: usize = 64;

impl ReceivedMoqObjectToEncodedSampleRouter {
    fn of(
        container_format: MoqContainerFormat,
        video_track_name: Option<String>,
        audio_track_name: Option<String>,
        data_track_name: Option<String>,
    ) -> Self {
        Self {
            container_format,
            video_track_name,
            audio_track_name,
            data_track_name,
            the_init_segment_has_arrived: false,
            video_track_reconstitution: None,
            audio_track_reconstitution: None,
            media_objects_dropped_before_the_init_segment_arrived: 0,
            the_pre_init_drop_has_been_reported: false,
            track_names_reported_as_unnamed: BTreeSet::new(),
            the_stray_track_name_cap_has_been_reported: false,
        }
    }

    /// Everything reconstitution carries across objects — the descriptions, the
    /// ordering counters, the per-track stamp anchors — belongs to one
    /// broadcast, so a reconnection starts from nothing.
    fn forget_what_the_closed_broadcast_described(&mut self) {
        *self = Self::of(
            self.container_format,
            self.video_track_name.clone(),
            self.audio_track_name.clone(),
            self.data_track_name.clone(),
        );
    }

    /// The samples one object carries: none for an object that is not one.
    fn route_received_object(
        &mut self,
        track_name: &str,
        payload: &[u8],
    ) -> Result<Vec<MoqTrackSample>> {
        match self.container_format {
            MoqContainerFormat::StreamlibBag => match self.kind_of_track(track_name) {
                Some(MoqTrackKind::Media(medium)) => {
                    Ok(vec![decode_object(payload, medium)?.into()])
                }
                // Whole and unread: the envelope's keys are the Python's to
                // decode, and nothing of a data object is parsed on this side.
                Some(MoqTrackKind::Data) => Ok(vec![MoqTrackSample::DataObject(DataTrackObject {
                    envelope_bytes: bytes::Bytes::copy_from_slice(payload),
                })]),
                None => {
                    self.report_an_object_on_a_track_this_subscriber_did_not_name(track_name);
                    Ok(Vec::new())
                }
            },
            MoqContainerFormat::Cmaf => Ok(self
                .route_a_cmaf_object(track_name, payload)?
                .into_iter()
                .map(MoqTrackSample::from)
                .collect()),
        }
    }

    fn route_a_cmaf_object(
        &mut self,
        track_name: &str,
        payload: &[u8],
    ) -> Result<Vec<EncodedMediaSample>> {
        if track_name == INIT_TRACK_NAME {
            self.absorb_the_init_segment(payload)?;
            return Ok(Vec::new());
        }
        let Some(MoqTrackKind::Media(medium)) = self.kind_of_track(track_name) else {
            self.report_an_object_on_a_track_this_subscriber_did_not_name(track_name);
            return Ok(Vec::new());
        };
        if !self.the_init_segment_has_arrived {
            self.media_objects_dropped_before_the_init_segment_arrived += 1;
            if !self.the_pre_init_drop_has_been_reported {
                self.the_pre_init_drop_has_been_reported = true;
                tracing::warn!(
                    track = %track_name,
                    "media arrived before the init segment and cannot be decoded without it; \
                     dropping until it lands, and reporting the count when it does"
                );
            }
            return Ok(Vec::new());
        }

        let fragment_samples = read_cmaf_fragment(payload)?;
        match medium {
            TrackMedium::Video => self.video_bags_of(fragment_samples),
            TrackMedium::Audio => self.audio_bags_of(fragment_samples),
        }
    }

    /// The one place this module reads the init-segment reader's types.
    fn absorb_the_init_segment(&mut self, payload: &[u8]) -> Result<()> {
        for description in read_cmaf_init_segment(payload)? {
            let media_timescale_hz = NonZeroU32::new(description.media_timescale_hz).ok_or_else(
                || MoqExtensionError::MalformedObject {
                    container: CMAF_PACKAGING,
                    what: format!(
                        "track {} of the init segment declares a timescale of zero, which places \
                         every sample of it at the same instant",
                        description.track_id
                    ),
                },
            )?;
            match description.track_medium {
                TrackMedium::Video => {
                    let (Some(wire_codec), Some((coded_width, coded_height))) =
                        (description.wire_codec.clone(), description.coded_extent)
                    else {
                        tracing::warn!(
                            track_id = description.track_id,
                            "the init segment's video track states no codec or no coded extent, \
                             so no bag can be spelled from its fragments"
                        );
                        continue;
                    };
                    if self.video_track_reconstitution.is_some() {
                        tracing::warn!(
                            track_id = description.track_id,
                            "the init segment describes a second video track; this subscriber has \
                             one video port, so the later description is ignored"
                        );
                        continue;
                    }
                    self.video_track_reconstitution = Some(CmafVideoTrackReconstitution {
                        codec: wire_codec,
                        media_timescale_hz,
                        parameter_set_nal_units: description.parameter_set_nal_units,
                        coded_width,
                        coded_height,
                        ordering_pair_counter: SubscriberMintedOrderingPairCounter::default(),
                        stamp_anchor: None,
                    });
                }
                TrackMedium::Audio => {
                    let (Some(channels), Some(sample_rate), Some(pre_skip)) = (
                        description.channels,
                        description.sample_rate,
                        description.pre_skip,
                    ) else {
                        tracing::warn!(
                            track_id = description.track_id,
                            "the init segment's audio track states no channel count, rate or \
                             pre-skip, so no bag can be spelled from its fragments"
                        );
                        continue;
                    };
                    if self.audio_track_reconstitution.is_some() {
                        tracing::warn!(
                            track_id = description.track_id,
                            "the init segment describes a second audio track; this subscriber has \
                             one audio port, so the later description is ignored"
                        );
                        continue;
                    }
                    self.audio_track_reconstitution = Some(CmafAudioTrackReconstitution {
                        codec: description.wire_codec.unwrap_or_else(|| "opus".to_owned()),
                        media_timescale_hz,
                        channels,
                        sample_rate,
                        pre_skip,
                        ordering_pair_counter: SubscriberMintedOrderingPairCounter::default(),
                        stamp_anchor: None,
                    });
                }
            }
        }
        self.the_init_segment_has_arrived = true;
        if self.media_objects_dropped_before_the_init_segment_arrived > 0 {
            tracing::info!(
                dropped = self.media_objects_dropped_before_the_init_segment_arrived,
                "the init segment arrived; that many media objects reached this subscriber \
                 before it and were dropped undecoded"
            );
        }
        Ok(())
    }

    fn video_bags_of(
        &mut self,
        fragment_samples: Vec<CmafFragmentSample>,
    ) -> Result<Vec<EncodedMediaSample>> {
        let video = self.video_track_reconstitution.as_mut().ok_or_else(|| {
            MoqExtensionError::MalformedObject {
                container: CMAF_PACKAGING,
                what: "a fragment arrived on the video track and the init segment describes no \
                       video track, so nothing states its parameter sets or its coded extent"
                    .to_owned(),
            }
        })?;

        let mut bags = Vec::with_capacity(fragment_samples.len());
        for fragment_sample in fragment_samples {
            // `avc1` and `hvc1` keep the parameter sets in the sample entry, so
            // they are prepended here — but only at a sync point: a decoder
            // does not need them on a delta frame, and a bag that repeats them
            // spends link on bytes nothing reads.
            let parameter_set_nal_units: &[Vec<u8>] = if fragment_sample.is_sync_point {
                &video.parameter_set_nal_units
            } else {
                &[]
            };
            let annex_b_access_unit = annex_b_access_unit_from_length_prefixed_sample(
                &fragment_sample.sample_bytes,
                parameter_set_nal_units,
            )
            .map_err(|failure| MoqExtensionError::MalformedObject {
                container: CMAF_PACKAGING,
                what: failure.to_string(),
            })?;

            let ordering_pair = video
                .ordering_pair_counter
                .account_reconstituted_bag(fragment_sample.is_sync_point);
            let timestamp_ns = video
                .stamp_anchor
                .get_or_insert_with(|| {
                    CmafTrackStampAnchorOnTheSubscribersClock::at(fragment_sample.decode_time)
                })
                .stamp_of(fragment_sample.decode_time, video.media_timescale_hz);

            bags.push(EncodedMediaSample::VideoAccessUnit(
                EncodedVideoAccessUnit {
                    codec: video.codec.clone(),
                    annex_b_access_unit: bytes::Bytes::from(annex_b_access_unit),
                    is_sync_point: fragment_sample.is_sync_point,
                    group_index: ordering_pair.group_index,
                    sequence_index: ordering_pair.sequence_index,
                    width: video.coded_width,
                    height: video.coded_height,
                    // ISO/IEC 23001-8 axes live in the bitstream's VUI on this
                    // path, not beside it: the container states no colour and
                    // inventing one here would out-rank the stream's own.
                    color: None,
                    timestamp_ns,
                },
            ));
        }
        Ok(bags)
    }

    fn audio_bags_of(
        &mut self,
        fragment_samples: Vec<CmafFragmentSample>,
    ) -> Result<Vec<EncodedMediaSample>> {
        let audio = self.audio_track_reconstitution.as_mut().ok_or_else(|| {
            MoqExtensionError::MalformedObject {
                container: CMAF_PACKAGING,
                what: "a fragment arrived on the audio track and the init segment describes no \
                       audio track, so nothing states its channel count, its rate or its \
                       `pre_skip`"
                    .to_owned(),
            }
        })?;

        let mut bags = Vec::with_capacity(fragment_samples.len());
        for fragment_sample in fragment_samples {
            let sample_count =
                opus_sample_count_of_the_packets_table_of_contents(&fragment_sample.sample_bytes)?;
            let ordering_pair = audio.ordering_pair_counter.account_reconstituted_bag(true);
            let timestamp_ns = audio
                .stamp_anchor
                .get_or_insert_with(|| {
                    CmafTrackStampAnchorOnTheSubscribersClock::at(fragment_sample.decode_time)
                })
                .stamp_of(fragment_sample.decode_time, audio.media_timescale_hz);

            bags.push(EncodedMediaSample::AudioPacket(EncodedAudioPacket {
                codec: audio.codec.clone(),
                opus_packet: bytes::Bytes::from(fragment_sample.sample_bytes),
                // RFC 6716 §3.1: every Opus packet is a decode entry point.
                is_sync_point: true,
                group_index: ordering_pair.group_index,
                sequence_index: ordering_pair.sequence_index,
                sample_rate: audio.sample_rate,
                channels: audio.channels,
                sample_count,
                pre_skip: audio.pre_skip,
                timestamp_ns,
            }));
        }
        Ok(bags)
    }

    fn kind_of_track(&self, track_name: &str) -> Option<MoqTrackKind> {
        if self.video_track_name.as_deref() == Some(track_name) {
            Some(MoqTrackKind::Media(TrackMedium::Video))
        } else if self.audio_track_name.as_deref() == Some(track_name) {
            Some(MoqTrackKind::Media(TrackMedium::Audio))
        } else if self.data_track_name.as_deref() == Some(track_name) {
            Some(MoqTrackKind::Data)
        } else {
            None
        }
    }

    /// Ignore the object, and say so once per track rather than once per
    /// object: a relay that misroutes one object misroutes all of them.
    fn report_an_object_on_a_track_this_subscriber_did_not_name(&mut self, track_name: &str) {
        if self.track_names_reported_as_unnamed.len()
            >= MOST_STRAY_TRACK_NAMES_HELD_TO_REPORT_EACH_ONCE
            && !self.track_names_reported_as_unnamed.contains(track_name)
        {
            if !self.the_stray_track_name_cap_has_been_reported {
                self.the_stray_track_name_cap_has_been_reported = true;
                tracing::warn!(
                    track = %track_name,
                    named_so_far = MOST_STRAY_TRACK_NAMES_HELD_TO_REPORT_EACH_ONCE,
                    "that many distinct track names this subscriber did not name have arrived; \
                     later ones are still ignored, but no longer named or held"
                );
            }
            return;
        }
        if self
            .track_names_reported_as_unnamed
            .insert(track_name.to_owned())
        {
            tracing::warn!(
                track = %track_name,
                "an object arrived on a track this subscriber did not name; ignoring it, \
                 because no output port has a contract for it"
            );
        }
    }
}

/// One video track as the init segment described it, plus what minting its
/// bags' ordering and stamps takes.
struct CmafVideoTrackReconstitution {
    codec: String,
    media_timescale_hz: NonZeroU32,
    parameter_set_nal_units: Vec<Vec<u8>>,
    coded_width: u32,
    coded_height: u32,
    ordering_pair_counter: SubscriberMintedOrderingPairCounter,
    stamp_anchor: Option<CmafTrackStampAnchorOnTheSubscribersClock>,
}

/// One Opus track as the init segment described it, plus the same two.
struct CmafAudioTrackReconstitution {
    codec: String,
    media_timescale_hz: NonZeroU32,
    channels: u32,
    sample_rate: u32,
    pre_skip: u32,
    ordering_pair_counter: SubscriberMintedOrderingPairCounter,
    stamp_anchor: Option<CmafTrackStampAnchorOnTheSubscribersClock>,
}

/// The ordering pair a bag carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EncodedStreamOrderingPair {
    group_index: u64,
    sequence_index: u64,
}

/// The pair `cmaf` does not carry, minted on the same rule the engine's
/// producer-side counter uses: a sync point after the first bag opens the next
/// group, and `sequence_index` never resets — which is the property a
/// consumer's gap detection rests on.
#[derive(Debug, Default)]
struct SubscriberMintedOrderingPairCounter {
    bags_reconstituted: u64,
    current_group_index: u64,
}

impl SubscriberMintedOrderingPairCounter {
    fn account_reconstituted_bag(&mut self, is_sync_point: bool) -> EncodedStreamOrderingPair {
        if is_sync_point && self.bags_reconstituted > 0 {
            self.current_group_index += 1;
        }
        let pair = EncodedStreamOrderingPair {
            group_index: self.current_group_index,
            sequence_index: self.bags_reconstituted,
        };
        self.bags_reconstituted += 1;
        pair
    }
}

/// Where one track's decode times are pinned on `CLOCK_MONOTONIC`.
///
/// Per track, not per broadcast: a CMAF track's `tfdt` epoch is that track's
/// own first stamp, so two tracks share no origin and one shared anchor would
/// place whichever arrived second by the other's offset. Cross-track alignment
/// is not recoverable from `cmaf` at all — it is what the `streamlib_bag`
/// container carries the producer's own stamps for.
#[derive(Debug, Clone, Copy)]
struct CmafTrackStampAnchorOnTheSubscribersClock {
    first_decode_time: u64,
    monotonic_at_the_first_sample_ns: i64,
}

impl CmafTrackStampAnchorOnTheSubscribersClock {
    fn at(first_decode_time: u64) -> Self {
        Self {
            first_decode_time,
            monotonic_at_the_first_sample_ns: monotonic_now_ns(),
        }
    }

    fn stamp_of(self, decode_time: u64, media_timescale_hz: NonZeroU32) -> i64 {
        self.monotonic_at_the_first_sample_ns
            .saturating_add(nanoseconds_of_track_ticks(
                decode_time.saturating_sub(self.first_decode_time),
                media_timescale_hz,
            ))
    }
}

/// A track's own ticks into nanoseconds, rounding to nearest — the inverse of
/// [`crate::cmaf_track_timeline::rescale_nanoseconds`].
fn nanoseconds_of_track_ticks(ticks: u64, media_timescale_hz: NonZeroU32) -> i64 {
    let timescale = u128::from(media_timescale_hz.get());
    let nanoseconds = (u128::from(ticks) * 1_000_000_000u128 + timescale / 2) / timescale;
    i64::try_from(nanoseconds).unwrap_or(i64::MAX)
}

/// One millisecond of Opus's own clock, the rate RFC 6716 §2 states every
/// frame duration against whatever bandwidth the frame was coded at.
const OPUS_SAMPLES_PER_MILLISECOND: u32 = OPUS_TRACK_TIMESCALE_HZ / 1_000;

/// RFC 6716 §3.2.5: a code-3 packet states its frame count in the low six bits
/// of the byte after the TOC.
const OPUS_CODE_THREE_FRAME_COUNT_MASK: u8 = 0b0011_1111;

/// The samples one Opus packet decodes to, read from the packet itself.
///
/// RFC 6716 §3.1 puts a packet's frame duration and frame count in its own TOC
/// byte, so a subscriber never has to trust a far end's arithmetic for a field
/// the bitstream already states.
fn opus_sample_count_of_the_packets_table_of_contents(opus_packet: &[u8]) -> Result<u32> {
    let Some(&table_of_contents_byte) = opus_packet.first() else {
        return Err(MoqExtensionError::MalformedObject {
            container: CMAF_PACKAGING,
            what: "an Opus packet of zero bytes arrived, and RFC 6716 §3.1 gives every packet a \
                   TOC byte stating its frame duration and its frame count"
                .to_owned(),
        });
    };
    // RFC 6716 §3.1: `config` in the top five bits, `s` in the next, `c` in the
    // low two.
    let frame_count = match table_of_contents_byte & 0b11 {
        0 => 1,
        1 | 2 => 2,
        _ => {
            let Some(&frame_count_byte) = opus_packet.get(1) else {
                return Err(MoqExtensionError::MalformedObject {
                    container: CMAF_PACKAGING,
                    what: "an Opus packet states frame-count code 3 and then ends, so the byte \
                           RFC 6716 §3.2.5 puts its frame count in is not there"
                        .to_owned(),
                });
            };
            match u32::from(frame_count_byte & OPUS_CODE_THREE_FRAME_COUNT_MASK) {
                0 => {
                    return Err(MoqExtensionError::MalformedObject {
                        container: CMAF_PACKAGING,
                        what: "an Opus packet states frame-count code 3 and then a frame count of \
                               zero, which decodes to no audio at all"
                            .to_owned(),
                    });
                }
                stated_frame_count => stated_frame_count,
            }
        }
    };
    let tenths_of_a_millisecond =
        opus_frame_duration_in_tenths_of_a_millisecond_of_a_toc_config(table_of_contents_byte >> 3);
    // RFC 6716 §3.4 caps a packet at 120 ms. A code-3 packet states its own
    // frame count in a following byte, so an out-of-range count would
    // otherwise turn into a sample count no encoder could have produced —
    // which a decoder downstream would read as a gap.
    let total_tenths_of_a_millisecond = frame_count * tenths_of_a_millisecond;
    if total_tenths_of_a_millisecond > LONGEST_OPUS_PACKET_IN_TENTHS_OF_A_MILLISECOND {
        return Err(MoqExtensionError::MalformedObject {
            container: CMAF_PACKAGING,
            what: format!(
                "the opus packet states {frame_count} frames of {}.{} ms, which is longer than \
                 the 120 ms RFC 6716 §3.4 allows one packet to carry",
                tenths_of_a_millisecond / 10,
                tenths_of_a_millisecond % 10
            ),
        });
    }
    Ok(total_tenths_of_a_millisecond * OPUS_SAMPLES_PER_MILLISECOND / 10)
}

/// RFC 6716 §3.4: no Opus packet carries more than 120 ms of audio.
const LONGEST_OPUS_PACKET_IN_TENTHS_OF_A_MILLISECOND: u32 = 1_200;

/// What a TOC byte's `config` says one of the packet's frames lasts.
///
/// RFC 6716 §3.1: `config` 0..=11 is SILK at 10, 20, 40 or 60 ms, 12..=15 is
/// hybrid at 10 or 20 ms, and 16..=31 is CELT at 2.5, 5, 10 or 20 ms. Tenths of
/// a millisecond, because the shortest CELT frame is not a whole one.
fn opus_frame_duration_in_tenths_of_a_millisecond_of_a_toc_config(
    table_of_contents_config: u8,
) -> u32 {
    const SILK_FRAME_DURATIONS: [u32; 4] = [100, 200, 400, 600];
    const HYBRID_FRAME_DURATIONS: [u32; 2] = [100, 200];
    const CELT_FRAME_DURATIONS: [u32; 4] = [25, 50, 100, 200];
    match table_of_contents_config {
        0..=11 => SILK_FRAME_DURATIONS[usize::from(table_of_contents_config % 4)],
        12..=15 => HYBRID_FRAME_DURATIONS[usize::from(table_of_contents_config % 2)],
        _ => CELT_FRAME_DURATIONS[usize::from(table_of_contents_config % 4)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annex_b_access_unit::{
        ANNEX_B_START_CODE, AnnexBNalHeaderGrammar, length_prefix_annex_b_access_unit,
    };
    use crate::cmaf_fragment::build_cmaf_fragment;

    /// The `tkhd.track_id` these fixtures write into a fragment.
    const CMAF_FRAGMENT_TRACK_ID: u32 = 1;
    use crate::cmaf_init_segment::{
        CmafTrackDescriptionForTheInitSegment, build_cmaf_init_segment,
    };
    use crate::cmaf_sample_entry::{build_opus_sample_entry, build_video_sample_entry};
    use crate::cmaf_track_timeline::{CmafTrackTimeline, VIDEO_TRACK_TIMESCALE_HZ};
    use crate::moq_broadcast_catalog::media_track_name;
    use crate::streamlib_bag_object::encode_object;

    const VIDEO_TRACK_ID: u32 = CMAF_FRAGMENT_TRACK_ID;
    const AUDIO_TRACK_ID: u32 = 2;
    /// What an `OpusEncoder` bag reports as its encoder lookahead. The number
    /// is the publisher's, and recovering it rather than inventing one is the
    /// difference between trimming 6.5 ms and trimming 80 ms.
    const PUBLISHED_OPUS_PRE_SKIP: u32 = 312;

    fn a_relay_config() -> MoqRelayConfig {
        MoqRelayConfig {
            relay_endpoint_url: "https://relay.example/anon".to_owned(),
            broadcast_path: "a-broadcast".to_owned(),
        }
    }

    fn a_subscriber_of(
        container_format: MoqContainerFormat,
        video_track_name: Option<String>,
        audio_track_name: Option<String>,
    ) -> MoqBroadcastSubscriber {
        MoqBroadcastSubscriber::new(
            a_relay_config(),
            container_format,
            video_track_name,
            audio_track_name,
            None,
        )
        .expect("the fixture names at least one track, which is the only refusal `new` makes")
    }

    fn a_data_track_subscriber_of(data_track_name: &str) -> MoqBroadcastSubscriber {
        MoqBroadcastSubscriber::new(
            a_relay_config(),
            MoqContainerFormat::StreamlibBag,
            None,
            None,
            Some(data_track_name.to_owned()),
        )
        .expect("a data track alone is a subscription")
    }

    /// A real H.264 SPS and PPS: profile 0x42 (baseline), level 0x1f.
    fn h264_sequence_parameter_set() -> Vec<u8> {
        vec![0x67, 0x42, 0xC0, 0x1F, 0xDA, 0x02, 0xD0, 0x49]
    }

    fn h264_picture_parameter_set() -> Vec<u8> {
        vec![0x68, 0xCE, 0x3C, 0x80]
    }

    /// One coded slice NAL: `nal_unit_type` 5 for an IDR, 1 for a delta.
    fn a_coded_slice_nal_unit(is_sync_point: bool, filler: u8) -> Vec<u8> {
        let header = if is_sync_point { 0x65 } else { 0x41 };
        vec![header, filler, filler, filler, filler]
    }

    fn annex_b_access_unit_of(nal_units: &[Vec<u8>]) -> Vec<u8> {
        let mut annex_b = Vec::new();
        for nal_unit in nal_units {
            annex_b.extend_from_slice(&ANNEX_B_START_CODE);
            annex_b.extend_from_slice(nal_unit);
        }
        annex_b
    }

    /// The coded extent every video fixture but the reconnection one uses.
    const FIXTURE_CODED_EXTENT: (u32, u32) = (320, 180);

    fn a_video_init_segment_description() -> CmafTrackDescriptionForTheInitSegment {
        a_video_init_segment_description_of_coded_extent(FIXTURE_CODED_EXTENT)
    }

    fn a_video_init_segment_description_of_coded_extent(
        coded_extent: (u32, u32),
    ) -> CmafTrackDescriptionForTheInitSegment {
        let (coded_width, coded_height) = coded_extent;
        let sample_entry = build_video_sample_entry(
            "h264",
            &length_prefix_annex_b_access_unit(
                &annex_b_access_unit_of(&[
                    h264_sequence_parameter_set(),
                    h264_picture_parameter_set(),
                ]),
                AnnexBNalHeaderGrammar::H264,
            )
            .parameter_sets,
            coded_width,
            coded_height,
        )
        .expect("the fixture parameter sets describe an H.264 track");
        CmafTrackDescriptionForTheInitSegment {
            track_id: VIDEO_TRACK_ID,
            inbound_link_name: "encoder/encoded_video".to_owned(),
            cmaf_track_sample_entry: sample_entry.cmaf_track_sample_entry,
            media_timescale_hz: VIDEO_TRACK_TIMESCALE_HZ,
            coded_extent: Some(coded_extent),
        }
    }

    fn an_audio_init_segment_description() -> CmafTrackDescriptionForTheInitSegment {
        let sample_entry =
            build_opus_sample_entry(2, OPUS_TRACK_TIMESCALE_HZ, PUBLISHED_OPUS_PRE_SKIP)
                .expect("stereo Opus at 48 kHz is what this container path places");
        CmafTrackDescriptionForTheInitSegment {
            track_id: AUDIO_TRACK_ID,
            inbound_link_name: "encoder/encoded_audio".to_owned(),
            cmaf_track_sample_entry: sample_entry.cmaf_track_sample_entry,
            media_timescale_hz: OPUS_TRACK_TIMESCALE_HZ,
            coded_extent: None,
        }
    }

    fn an_init_object(tracks: &[CmafTrackDescriptionForTheInitSegment]) -> bytes::Bytes {
        build_cmaf_init_segment(tracks).expect("the fixture describes at least one track")
    }

    fn a_video_fragment(
        sequence_number: u32,
        decode_time: u64,
        is_sync_point: bool,
        filler: u8,
    ) -> bytes::Bytes {
        let mut nal_units = Vec::new();
        if is_sync_point {
            nal_units.push(h264_sequence_parameter_set());
            nal_units.push(h264_picture_parameter_set());
        }
        nal_units.push(a_coded_slice_nal_unit(is_sync_point, filler));
        let length_prefixed = length_prefix_annex_b_access_unit(
            &annex_b_access_unit_of(&nal_units),
            AnnexBNalHeaderGrammar::H264,
        );
        build_cmaf_fragment(
            VIDEO_TRACK_ID,
            sequence_number,
            decode_time,
            33_000_000,
            is_sync_point,
            &length_prefixed.length_prefixed_sample_bytes,
        )
        .expect("the fixture sample is small enough for an mdat")
    }

    /// An audio fragment whose decode time and duration are the ones this
    /// wheel's own publisher would write for a bag stamped `stamp_ns`.
    fn an_audio_fragment_placed_as_the_publisher_places_one(
        sequence_number: u32,
        audio_track_timeline: &mut CmafTrackTimeline,
        stamp_ns: i64,
        opus_packet: &[u8],
        sample_count_the_packet_decodes_to: u32,
    ) -> bytes::Bytes {
        let placement = audio_track_timeline
            .place_sample_of_stated_duration(stamp_ns, sample_count_the_packet_decodes_to);
        build_cmaf_fragment(
            AUDIO_TRACK_ID,
            sequence_number,
            placement.decode_time,
            placement.duration,
            true,
            opus_packet,
        )
        .expect("the fixture packet is small enough for an mdat")
    }

    /// One Opus packet whose TOC states `config`, mono, and frame-count code
    /// `c` — with filler where the frames' own bytes would be, which nothing
    /// on this path reads.
    fn an_opus_packet_stating_config_and_frame_count_code(
        table_of_contents_config: u8,
        frame_count_code: u8,
    ) -> Vec<u8> {
        vec![
            (table_of_contents_config << 3) | frame_count_code,
            0x00,
            0x00,
        ]
    }

    /// One code-3 Opus packet stating an arbitrary frame count.
    fn an_opus_packet_stating_config_and_an_arbitrary_frame_count(
        table_of_contents_config: u8,
        frame_count: u8,
    ) -> Vec<u8> {
        vec![
            (table_of_contents_config << 3) | 0b11,
            frame_count & OPUS_CODE_THREE_FRAME_COUNT_MASK,
            0x00,
        ]
    }

    /// The 20 ms stereo CELT packet an `OpusEncoder` in this tree mints:
    /// config 31, `s` set, frame-count code 0.
    fn a_twenty_millisecond_stereo_opus_packet() -> Vec<u8> {
        vec![0xFC, 0x01, 0x02, 0x03, 0x04]
    }

    /// What the packet above decodes to, and so the duration the publisher
    /// writes beside it: 20 ms of Opus's own 48 kHz clock.
    const SAMPLES_A_TWENTY_MILLISECOND_OPUS_PACKET_DECODES_TO: u32 = 960;

    fn the_only_video_access_unit(bags: Vec<MoqTrackSample>) -> EncodedVideoAccessUnit {
        match bags.as_slice() {
            [MoqTrackSample::EncodedMedia(EncodedMediaSample::VideoAccessUnit(unit))] => {
                unit.clone()
            }
            other => panic!("expected exactly one video access unit, got {other:?}"),
        }
    }

    fn the_only_audio_packet(bags: Vec<MoqTrackSample>) -> EncodedAudioPacket {
        match bags.as_slice() {
            [MoqTrackSample::EncodedMedia(EncodedMediaSample::AudioPacket(packet))] => {
                packet.clone()
            }
            other => panic!("expected exactly one Opus packet, got {other:?}"),
        }
    }

    /// A `tracing` subscriber that keeps the level of every event a test's own
    /// call emitted, so a test can state how loudly a loss is reported.
    #[derive(Clone, Default)]
    struct TracingEventLevelsCapturedWhileASubscriberRuns(
        std::sync::Arc<std::sync::Mutex<Vec<tracing::Level>>>,
    );

    impl TracingEventLevelsCapturedWhileASubscriberRuns {
        fn levels(&self) -> Vec<tracing::Level> {
            self.0
                .lock()
                .expect("no test panics while holding this lock")
                .clone()
        }
    }

    impl tracing::Subscriber for TracingEventLevelsCapturedWhileASubscriberRuns {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            self.0
                .lock()
                .expect("no test panics while holding this lock")
                .push(*event.metadata().level());
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn contains_the_byte_run(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    #[should_panic(expected = "expected exactly one video access unit")]
    fn a_second_video_bag_beside_the_only_one_a_test_named_fails_that_test() {
        let a_bag = EncodedMediaSample::VideoAccessUnit(EncodedVideoAccessUnit {
            codec: "h264".to_owned(),
            annex_b_access_unit: bytes::Bytes::from(annex_b_access_unit_of(&[
                a_coded_slice_nal_unit(true, 0xAB),
            ])),
            is_sync_point: true,
            group_index: 0,
            sequence_index: 0,
            width: FIXTURE_CODED_EXTENT.0,
            height: FIXTURE_CODED_EXTENT.1,
            color: None,
            timestamp_ns: 1,
        });
        the_only_video_access_unit(vec![a_bag.clone().into(), a_bag.into()]);
    }

    #[test]
    #[should_panic(expected = "expected exactly one Opus packet")]
    fn a_second_opus_bag_beside_the_only_one_a_test_named_fails_that_test() {
        let a_bag = EncodedMediaSample::AudioPacket(EncodedAudioPacket {
            codec: "opus".to_owned(),
            opus_packet: bytes::Bytes::from(a_twenty_millisecond_stereo_opus_packet()),
            is_sync_point: true,
            group_index: 0,
            sequence_index: 0,
            sample_rate: 48_000,
            channels: 2,
            sample_count: 960,
            pre_skip: PUBLISHED_OPUS_PRE_SKIP,
            timestamp_ns: 1,
        });
        the_only_audio_packet(vec![a_bag.clone().into(), a_bag.into()]);
    }

    #[test]
    fn a_subscriber_naming_no_track_at_all_is_refused_rather_than_left_to_produce_nothing() {
        let refusal = MoqBroadcastSubscriber::new(
            a_relay_config(),
            MoqContainerFormat::Cmaf,
            None,
            None,
            None,
        )
        .map(drop)
        .expect_err("naming no track subscribes to nothing");
        assert!(
            matches!(refusal, MoqExtensionError::Refused { .. }),
            "a caller that named no track passed something wrong, so this is a ValueError on the \
             Python side; got {refusal:?}"
        );
        assert!(
            refusal.to_string().contains("`data_track`"),
            "the refusal names every track a subscriber can name; got {refusal}"
        );
    }

    #[test]
    fn a_subscriber_naming_only_a_data_track_subscribes_to_it_and_nothing_else() {
        let subscriber = a_data_track_subscriber_of("telemetry");
        assert_eq!(
            subscriber.subscribed_track_names,
            vec!["telemetry".to_owned()]
        );
    }

    #[test]
    fn a_streamlib_bag_subscriber_naming_a_data_track_subscribes_to_it_beside_its_media() {
        let subscriber = MoqBroadcastSubscriber::new(
            a_relay_config(),
            MoqContainerFormat::StreamlibBag,
            Some("video".to_owned()),
            Some("audio".to_owned()),
            Some("telemetry".to_owned()),
        )
        .expect("three tracks of three kinds");
        assert_eq!(
            subscriber.subscribed_track_names,
            vec![
                "video".to_owned(),
                "audio".to_owned(),
                "telemetry".to_owned()
            ],
            "the data track joins the media tracks; nothing about them changes"
        );
    }

    #[test]
    fn a_data_track_under_cmaf_is_refused_by_name() {
        let refusal = MoqBroadcastSubscriber::new(
            a_relay_config(),
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
            Some("telemetry".to_owned()),
        )
        .map(drop)
        .expect_err("CMAF has no packaging for a data track");
        assert!(
            matches!(refusal, MoqExtensionError::Refused { .. }),
            "the caller asked for a container that cannot carry what they named, which is a \
             bad call; got {refusal:?}"
        );
        let said = refusal.to_string();
        assert!(
            said.contains("`data_track`") && said.contains("`cmaf`"),
            "the refusal names the config and the container; got {said}"
        );
    }

    #[test]
    fn a_data_track_object_is_handed_back_byte_for_byte_and_never_parsed() {
        // Not msgpack, not anything: the Rust hands a data object through
        // whole, and what its bytes mean is the Python's to decide.
        let published: &[u8] = b"\x83\xaesequence_index\x07 whatever the envelope holds";
        let mut subscriber = a_data_track_subscriber_of("telemetry");

        let samples = subscriber
            .received_object_router
            .route_received_object("telemetry", published)
            .expect("a data object is never malformed on this side; nothing here reads it");

        assert_eq!(
            samples,
            vec![MoqTrackSample::DataObject(DataTrackObject {
                envelope_bytes: bytes::Bytes::copy_from_slice(published),
            })],
            "the same bytes out, untouched"
        );
    }

    #[test]
    fn a_data_object_on_a_subscriber_that_named_no_data_track_is_ignored_as_a_stray() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::StreamlibBag,
            Some("video".to_owned()),
            None,
        );

        let samples = subscriber
            .received_object_router
            .route_received_object("telemetry", b"whatever this is")
            .expect("an unnamed track is ignored, not refused");

        assert!(samples.is_empty());
        assert!(
            subscriber
                .received_object_router
                .track_names_reported_as_unnamed
                .contains("telemetry"),
            "no output port has a contract for it, and the drop is reported once per track"
        );
    }

    #[test]
    fn a_media_object_on_the_data_track_is_still_handed_through_as_bytes() {
        // The kind is the config's, not the bytes': an encoded-media object
        // that a publisher misrouted onto the data track is a data object here,
        // and the Python's envelope refusal is what names it.
        let published = encode_object(
            &EncodedMediaSample::AudioPacket(EncodedAudioPacket {
                codec: "opus".to_owned(),
                opus_packet: bytes::Bytes::from(a_twenty_millisecond_stereo_opus_packet()),
                is_sync_point: true,
                group_index: 0,
                sequence_index: 0,
                sample_rate: 48_000,
                channels: 2,
                sample_count: 960,
                pre_skip: PUBLISHED_OPUS_PRE_SKIP,
                timestamp_ns: 1,
            })
            .into(),
        )
        .expect("the fixture bag encodes");
        let mut subscriber = a_data_track_subscriber_of("telemetry");

        let samples = subscriber
            .received_object_router
            .route_received_object("telemetry", &published)
            .expect("nothing on this side reads a data object");

        assert!(matches!(
            samples.as_slice(),
            [MoqTrackSample::DataObject(DataTrackObject { envelope_bytes })] if *envelope_bytes == published
        ));
    }

    #[test]
    fn one_track_name_cannot_carry_media_and_data_because_one_track_is_one_kind() {
        for (video_track_name, audio_track_name) in [
            (Some("both".to_owned()), None),
            (None, Some("both".to_owned())),
        ] {
            let refusal = MoqBroadcastSubscriber::new(
                a_relay_config(),
                MoqContainerFormat::StreamlibBag,
                video_track_name,
                audio_track_name,
                Some("both".to_owned()),
            )
            .map(drop)
            .expect_err("one track is one kind");
            assert!(matches!(refusal, MoqExtensionError::Refused { .. }));
            assert!(
                refusal.to_string().contains("`data_track`"),
                "the refusal names both configs that collide; got {refusal}"
            );
        }
    }

    #[test]
    fn a_data_track_named_as_the_empty_string_is_refused_rather_than_subscribed_to() {
        let refusal = MoqBroadcastSubscriber::new(
            a_relay_config(),
            MoqContainerFormat::StreamlibBag,
            None,
            None,
            Some(String::new()),
        )
        .map(drop)
        .expect_err("the empty string names no track on the relay");
        assert!(matches!(refusal, MoqExtensionError::Refused { .. }));
        assert!(refusal.to_string().contains("`data_track`"), "{refusal}");
    }

    #[test]
    fn a_cmaf_subscriber_subscribes_to_the_init_track_as_well_as_its_media_tracks() {
        let subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            Some(media_track_name(AUDIO_TRACK_ID)),
        );
        assert_eq!(
            subscriber.subscribed_track_names,
            vec![
                INIT_TRACK_NAME.to_owned(),
                media_track_name(VIDEO_TRACK_ID),
                media_track_name(AUDIO_TRACK_ID),
            ]
        );
    }

    #[test]
    fn a_streamlib_bag_subscriber_subscribes_to_its_media_tracks_and_nothing_else() {
        let subscriber = a_subscriber_of(
            MoqContainerFormat::StreamlibBag,
            Some("video".to_owned()),
            None,
        );
        assert_eq!(subscriber.subscribed_track_names, vec!["video".to_owned()]);
    }

    #[test]
    fn a_streamlib_bag_object_hands_back_every_key_the_producer_wrote() {
        let published = EncodedMediaSample::VideoAccessUnit(EncodedVideoAccessUnit {
            codec: "h264".to_owned(),
            annex_b_access_unit: bytes::Bytes::from(annex_b_access_unit_of(&[
                h264_sequence_parameter_set(),
                h264_picture_parameter_set(),
                a_coded_slice_nal_unit(true, 0xAB),
            ])),
            is_sync_point: true,
            group_index: 41,
            sequence_index: 987,
            width: 1920,
            height: 1080,
            color: Some(
                [
                    ("primaries".to_owned(), "bt709".to_owned()),
                    ("transfer".to_owned(), "bt709".to_owned()),
                ]
                .into_iter()
                .collect(),
            ),
            timestamp_ns: 1_234_567_891_011,
        });
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::StreamlibBag,
            Some("video".to_owned()),
            None,
        );

        let bags = subscriber
            .received_object_router
            .route_received_object(
                "video",
                &encode_object(&published.clone().into()).expect("the fixture bag encodes"),
            )
            .expect("the object is this subscriber's own container");

        assert_eq!(
            bags,
            vec![published.into()],
            "every key including the producer's ordering pair and its stamp comes back unchanged"
        );
    }

    #[test]
    fn a_cmaf_video_fragment_carries_the_parameter_sets_only_where_a_decoder_needs_them() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
        );
        let object_router = &mut subscriber.received_object_router;
        object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");

        let sync_point = the_only_video_access_unit(
            object_router
                .route_received_object(
                    &media_track_name(VIDEO_TRACK_ID),
                    &a_video_fragment(1, 0, true, 0xAA),
                )
                .expect("a fragment after the init segment reconstitutes"),
        );
        let delta = the_only_video_access_unit(
            object_router
                .route_received_object(
                    &media_track_name(VIDEO_TRACK_ID),
                    &a_video_fragment(2, 33_000_000, false, 0xBB),
                )
                .expect("a fragment after the init segment reconstitutes"),
        );

        assert!(
            contains_the_byte_run(
                &sync_point.annex_b_access_unit,
                &h264_sequence_parameter_set()
            ) && contains_the_byte_run(
                &sync_point.annex_b_access_unit,
                &h264_picture_parameter_set()
            ),
            "`avc1` keeps the parameter sets in the sample entry, so a bag a decoder can enter \
             the stream at has to carry them inline"
        );
        assert!(
            !contains_the_byte_run(&delta.annex_b_access_unit, &h264_sequence_parameter_set())
                && !contains_the_byte_run(
                    &delta.annex_b_access_unit,
                    &h264_picture_parameter_set()
                ),
            "a decoder does not need the parameter sets on a delta frame, and repeating them \
             spends link on bytes nothing reads"
        );
        assert_eq!(
            (sync_point.width, sync_point.height),
            FIXTURE_CODED_EXTENT,
            "the coded extent is recovered from the init segment, which is the only place a \
             CMAF broadcast states it"
        );
        assert_eq!(sync_point.codec, "h264");
        assert_eq!(
            sync_point.color, None,
            "the container keeps colour in the bitstream's VUI, not beside it"
        );
    }

    #[test]
    fn the_minted_sequence_index_never_resets_while_the_group_index_advances_at_sync_points() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
        );
        let object_router = &mut subscriber.received_object_router;
        object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");

        let mut minted_pairs = Vec::new();
        for (index, is_sync_point) in [true, false, false, true, false].into_iter().enumerate() {
            let unit = the_only_video_access_unit(
                object_router
                    .route_received_object(
                        &media_track_name(VIDEO_TRACK_ID),
                        &a_video_fragment(
                            index as u32 + 1,
                            index as u64 * 33_000_000,
                            is_sync_point,
                            index as u8,
                        ),
                    )
                    .expect("a fragment after the init segment reconstitutes"),
            );
            minted_pairs.push((unit.group_index, unit.sequence_index));
        }

        assert_eq!(
            minted_pairs,
            vec![(0, 0), (0, 1), (0, 2), (1, 3), (1, 4)],
            "a sync point after the first bag opens the next group, and `sequence_index` never \
             resets — which is the property a consumer's gap detection rests on"
        );
    }

    #[test]
    fn a_cmaf_video_bag_is_stamped_by_its_distance_from_the_first_sample_on_that_track() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
        );
        let object_router = &mut subscriber.received_object_router;
        object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");

        let first = the_only_video_access_unit(
            object_router
                .route_received_object(
                    &media_track_name(VIDEO_TRACK_ID),
                    &a_video_fragment(1, 7_000_000_000, true, 0x11),
                )
                .expect("a fragment after the init segment reconstitutes"),
        );
        let second = the_only_video_access_unit(
            object_router
                .route_received_object(
                    &media_track_name(VIDEO_TRACK_ID),
                    &a_video_fragment(2, 7_033_000_000, false, 0x22),
                )
                .expect("a fragment after the init segment reconstitutes"),
        );

        assert_eq!(
            second.timestamp_ns - first.timestamp_ns,
            33_000_000,
            "the gap between two bags is the gap between their decode times, whatever the \
             subscriber's clock read when the first one landed"
        );
        assert!(
            first.timestamp_ns > 0,
            "the anchor is this subscriber's own monotonic clock, not the publisher's tfdt epoch"
        );
    }

    #[test]
    fn an_opus_bag_states_the_pre_skip_the_init_segment_carried_rather_than_a_floor() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            None,
            Some(media_track_name(AUDIO_TRACK_ID)),
        );
        let object_router = &mut subscriber.received_object_router;
        object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[an_audio_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");

        let opus_packet_bytes = a_twenty_millisecond_stereo_opus_packet();
        let mut audio_track_timeline = CmafTrackTimeline::on(OPUS_TRACK_TIMESCALE_HZ);
        let packet = the_only_audio_packet(
            object_router
                .route_received_object(
                    &media_track_name(AUDIO_TRACK_ID),
                    &an_audio_fragment_placed_as_the_publisher_places_one(
                        1,
                        &mut audio_track_timeline,
                        9_000_000_000,
                        &opus_packet_bytes,
                        SAMPLES_A_TWENTY_MILLISECOND_OPUS_PACKET_DECODES_TO,
                    ),
                )
                .expect("a fragment after the init segment reconstitutes"),
        );

        assert_eq!(
            packet.pre_skip, PUBLISHED_OPUS_PRE_SKIP,
            "`dOps` PreSkip is the encoder's own lookahead, and a subscriber that invented one \
             would trim real audio away"
        );
        assert_eq!(packet.codec, "opus");
        assert_eq!((packet.channels, packet.sample_rate), (2, 48_000));
        assert!(
            packet.is_sync_point,
            "every Opus packet is a decode entry point"
        );
        assert_eq!(packet.opus_packet, bytes::Bytes::from(opus_packet_bytes));
    }

    /// The bitstream outranks the container. This publisher now writes the
    /// packet's own count beside it, so only a far end that disagrees can tell
    /// a TOC-derived count from a `trun`-derived one — and a third-party CMAF
    /// publisher is exactly who the `cmaf` container is for.
    #[test]
    fn an_opus_bag_counts_the_samples_its_packet_carries_when_the_fragment_states_otherwise() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            None,
            Some(media_track_name(AUDIO_TRACK_ID)),
        );
        let object_router = &mut subscriber.received_object_router;
        object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[an_audio_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");

        let a_duration_no_opus_packet_decodes_to = 1_234;
        let fragment = an_audio_fragment_placed_as_the_publisher_places_one(
            1,
            &mut CmafTrackTimeline::on(OPUS_TRACK_TIMESCALE_HZ),
            9_000_000_000,
            &a_twenty_millisecond_stereo_opus_packet(),
            a_duration_no_opus_packet_decodes_to,
        );
        assert_eq!(
            read_cmaf_fragment(&fragment).expect("the fixture fragment reads back")[0].duration,
            a_duration_no_opus_packet_decodes_to,
            "the fixture really does state a duration the packet contradicts"
        );

        let packet = the_only_audio_packet(
            object_router
                .route_received_object(&media_track_name(AUDIO_TRACK_ID), &fragment)
                .expect("a fragment after the init segment reconstitutes"),
        );

        assert_eq!(
            packet.sample_count, SAMPLES_A_TWENTY_MILLISECOND_OPUS_PACKET_DECODES_TO,
            "RFC 6716 §3.1 puts the count in the packet's own TOC byte, so a far end's \
             arithmetic is never trusted for it"
        );
    }

    #[test]
    fn each_opus_frame_count_code_states_how_many_frames_the_packet_carries() {
        // Config 31 is CELT full-band at 20 ms, so one frame is 960 samples.
        let twenty_millisecond_config = 31;
        assert_eq!(
            opus_sample_count_of_the_packets_table_of_contents(
                &an_opus_packet_stating_config_and_frame_count_code(twenty_millisecond_config, 0)
            )
            .expect("code 0 is one frame"),
            960
        );
        assert_eq!(
            opus_sample_count_of_the_packets_table_of_contents(
                &an_opus_packet_stating_config_and_frame_count_code(twenty_millisecond_config, 1)
            )
            .expect("code 1 is two frames of equal size"),
            1_920
        );
        assert_eq!(
            opus_sample_count_of_the_packets_table_of_contents(
                &an_opus_packet_stating_config_and_frame_count_code(twenty_millisecond_config, 2)
            )
            .expect("code 2 is two frames of different sizes"),
            1_920
        );
        assert_eq!(
            opus_sample_count_of_the_packets_table_of_contents(
                &an_opus_packet_stating_config_and_an_arbitrary_frame_count(
                    twenty_millisecond_config,
                    6
                )
            )
            .expect("code 3 states its own frame count"),
            5_760,
            "a code-3 packet lasts its stated frame count times its frame duration"
        );
    }

    #[test]
    fn each_opus_frame_duration_is_counted_at_forty_eight_samples_a_millisecond() {
        let counted_samples_of_one_frame = |table_of_contents_config: u8| {
            opus_sample_count_of_the_packets_table_of_contents(
                &an_opus_packet_stating_config_and_frame_count_code(table_of_contents_config, 0),
            )
            .expect("a one-frame packet of any config counts")
        };

        // RFC 6716 §3.1: SILK narrowband at 10, 20, 40 and 60 ms.
        assert_eq!(counted_samples_of_one_frame(0), 480);
        assert_eq!(counted_samples_of_one_frame(1), 960);
        assert_eq!(counted_samples_of_one_frame(2), 1_920);
        assert_eq!(counted_samples_of_one_frame(3), 2_880);
        // SILK wideband keeps the same four durations.
        assert_eq!(counted_samples_of_one_frame(11), 2_880);
        // Hybrid super-wideband and full-band, at 10 and 20 ms only.
        assert_eq!(counted_samples_of_one_frame(12), 480);
        assert_eq!(counted_samples_of_one_frame(13), 960);
        assert_eq!(counted_samples_of_one_frame(15), 960);
        // CELT, which reaches down to 2.5 ms.
        assert_eq!(counted_samples_of_one_frame(16), 120);
        assert_eq!(counted_samples_of_one_frame(17), 240);
        assert_eq!(counted_samples_of_one_frame(18), 480);
        assert_eq!(counted_samples_of_one_frame(19), 960);
        assert_eq!(counted_samples_of_one_frame(28), 120);
        assert_eq!(counted_samples_of_one_frame(31), 960);
    }

    #[test]
    fn an_opus_packet_too_short_to_carry_its_own_table_of_contents_is_refused_by_name() {
        for (packet, what_the_packet_is_missing) in [
            (Vec::new(), "the TOC byte itself"),
            (
                an_opus_packet_stating_config_and_an_arbitrary_frame_count(31, 3)[..1].to_vec(),
                "the frame-count byte its code-3 TOC promises",
            ),
        ] {
            let refusal = opus_sample_count_of_the_packets_table_of_contents(&packet)
                .map(drop)
                .expect_err("a packet that does not carry its own count is not guessed at");
            assert!(
                matches!(refusal, MoqExtensionError::MalformedObject { .. }),
                "a packet missing {what_the_packet_is_missing} came from the far end, which is a \
                 runtime condition and not a bad call; got {refusal:?}"
            );
        }
    }

    #[test]
    fn a_code_three_opus_packet_stating_zero_frames_is_refused_rather_than_counted_as_no_audio() {
        let refusal = opus_sample_count_of_the_packets_table_of_contents(
            &an_opus_packet_stating_config_and_an_arbitrary_frame_count(31, 0),
        )
        .map(drop)
        .expect_err("a packet claiming zero frames decodes to nothing");
        assert!(
            matches!(refusal, MoqExtensionError::MalformedObject { .. }),
            "the far end wrote a packet no decoder can read, which is a runtime condition and not \
             a bad call; got {refusal:?}"
        );
    }

    #[test]
    fn media_arriving_before_the_init_segment_is_dropped_and_counted_rather_than_lost_silently() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
        );
        let object_router = &mut subscriber.received_object_router;

        for sequence_number in 1..=3u32 {
            let bags = object_router
                .route_received_object(
                    &media_track_name(VIDEO_TRACK_ID),
                    &a_video_fragment(sequence_number, 0, true, 0x33),
                )
                .expect("an undecodable fragment is dropped, not refused");
            assert!(bags.is_empty(), "nothing can be decoded without the moov");
        }
        assert_eq!(
            object_router.media_objects_dropped_before_the_init_segment_arrived, 3,
            "the loss is accounted, and reported once the init segment lands"
        );

        object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");
        let bags = object_router
            .route_received_object(
                &media_track_name(VIDEO_TRACK_ID),
                &a_video_fragment(4, 0, true, 0x44),
            )
            .expect("a fragment after the init segment reconstitutes");
        assert_eq!(
            bags.len(),
            1,
            "the first fragment after the init segment is the one playback resumes at"
        );
    }

    #[test]
    fn an_object_on_a_track_this_subscriber_did_not_name_is_ignored_rather_than_ending_the_read() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
        );
        let object_router = &mut subscriber.received_object_router;
        object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");

        let bags = object_router
            .route_received_object("99.m4s", b"whatever this is")
            .expect(
                "an unnamed track is ignored, not refused: one stray object must not end a \
                 live subscription",
            );
        assert!(bags.is_empty());
        assert!(
            object_router
                .track_names_reported_as_unnamed
                .contains("99.m4s"),
            "the drop is reported once per track rather than once per object"
        );
    }

    #[test]
    fn an_init_object_is_not_itself_a_sample() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
        );
        let bags = subscriber
            .received_object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");
        assert!(
            bags.is_empty(),
            "a caller polling with a timeout simply calls again"
        );
    }

    #[test]
    fn a_fragment_on_a_medium_the_init_segment_never_described_is_refused_by_name() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            Some(media_track_name(AUDIO_TRACK_ID)),
        );
        let object_router = &mut subscriber.received_object_router;
        object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");

        let mut audio_track_timeline = CmafTrackTimeline::on(OPUS_TRACK_TIMESCALE_HZ);
        let refusal = object_router
            .route_received_object(
                &media_track_name(AUDIO_TRACK_ID),
                &an_audio_fragment_placed_as_the_publisher_places_one(
                    1,
                    &mut audio_track_timeline,
                    9_000_000_000,
                    &a_twenty_millisecond_stereo_opus_packet(),
                    SAMPLES_A_TWENTY_MILLISECOND_OPUS_PACKET_DECODES_TO,
                ),
            )
            .expect_err("the moov describes no audio track, so nothing states its `pre_skip`");
        assert!(
            matches!(refusal, MoqExtensionError::MalformedObject { .. }),
            "the far end's moov and its media tracks disagree, which is a runtime condition and \
             not a bad call; got {refusal:?}"
        );
    }

    #[test]
    fn reading_a_sample_before_connecting_says_so_rather_than_returning_nothing() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::StreamlibBag,
            Some("video".to_owned()),
            None,
        );
        assert!(!subscriber.is_connected());
        let refusal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime builds")
            .block_on(subscriber.next_sample(Duration::from_millis(1)))
            .expect_err("nothing is connected");
        assert!(
            matches!(
                refusal,
                MoqExtensionError::NotConnected {
                    role: SUBSCRIBING_SESSION_ROLE
                }
            ),
            "a silent `None` would read from outside as a broadcast that sends nothing; got \
             {refusal:?}"
        );
    }

    #[test]
    fn closing_a_subscriber_that_never_connected_leaves_it_disconnected() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::StreamlibBag,
            Some("video".to_owned()),
            None,
        );
        subscriber.close();
        assert!(!subscriber.is_connected());
    }

    #[test]
    fn a_reconnected_subscriber_reads_the_new_broadcast_rather_than_the_closed_ones_description() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
        );
        subscriber
            .received_object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description()]),
            )
            .expect("the init object is the one this module's writer wrote");
        for (sequence_number, decode_time) in [(1u32, 7_000_000_000u64), (2, 7_033_000_000)] {
            subscriber
                .received_object_router
                .route_received_object(
                    &media_track_name(VIDEO_TRACK_ID),
                    &a_video_fragment(sequence_number, decode_time, sequence_number == 1, 0x11),
                )
                .expect("a fragment after the init segment reconstitutes");
        }

        subscriber.close();
        let monotonic_after_the_close_ns = monotonic_now_ns();

        let reconnected_coded_extent = (640, 360);
        subscriber
            .received_object_router
            .route_received_object(
                INIT_TRACK_NAME,
                &an_init_object(&[a_video_init_segment_description_of_coded_extent(
                    reconnected_coded_extent,
                )]),
            )
            .expect("the init object is the one this module's writer wrote");
        let mut bags_of_the_second_broadcast = Vec::new();
        for (sequence_number, decode_time) in [(1u32, 0u64), (2, 33_000_000)] {
            bags_of_the_second_broadcast.push(the_only_video_access_unit(
                subscriber
                    .received_object_router
                    .route_received_object(
                        &media_track_name(VIDEO_TRACK_ID),
                        &a_video_fragment(sequence_number, decode_time, sequence_number == 1, 0x22),
                    )
                    .expect("a fragment after the init segment reconstitutes"),
            ));
        }

        assert_eq!(
            bags_of_the_second_broadcast
                .iter()
                .map(|unit| (unit.width, unit.height))
                .collect::<Vec<_>>(),
            vec![reconnected_coded_extent; 2],
            "the coded extent a bag states is the one the broadcast it belongs to described"
        );
        assert_eq!(
            bags_of_the_second_broadcast
                .iter()
                .map(|unit| (unit.group_index, unit.sequence_index))
                .collect::<Vec<_>>(),
            vec![(0, 0), (0, 1)],
            "a new broadcast's ordering starts at its own first bag, so a consumer's gap \
             detection is not handed a jump it cannot explain"
        );
        assert!(
            bags_of_the_second_broadcast[0].timestamp_ns >= monotonic_after_the_close_ns,
            "the new broadcast's track is anchored where the subscriber's clock stood when it \
             began, never at the closed broadcast's anchor"
        );
    }

    #[test]
    fn stray_track_names_are_held_only_up_to_the_cap_that_keeps_the_reporting_once_per_track() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::Cmaf,
            Some(media_track_name(VIDEO_TRACK_ID)),
            None,
        );
        let object_router = &mut subscriber.received_object_router;

        for stray_track_index in 0..MOST_STRAY_TRACK_NAMES_HELD_TO_REPORT_EACH_ONCE * 4 {
            let bags = object_router
                .route_received_object(
                    &format!("stray-{stray_track_index}.m4s"),
                    b"whatever this is",
                )
                .expect("an unnamed track is ignored, not refused");
            assert!(bags.is_empty());
        }

        assert_eq!(
            object_router.track_names_reported_as_unnamed.len(),
            MOST_STRAY_TRACK_NAMES_HELD_TO_REPORT_EACH_ONCE,
            "a far end that misroutes every object under a fresh name must not be able to grow \
             what this subscriber holds for as long as the subscription lives"
        );
    }

    #[test]
    fn closing_on_decoded_bags_nothing_read_reports_that_loss_as_loudly_as_every_other_loss() {
        let mut subscriber = a_subscriber_of(
            MoqContainerFormat::StreamlibBag,
            Some("video".to_owned()),
            None,
        );
        subscriber
            .samples_awaiting_the_reader
            .push_back(MoqTrackSample::EncodedMedia(
                EncodedMediaSample::AudioPacket(EncodedAudioPacket {
                    codec: "opus".to_owned(),
                    opus_packet: bytes::Bytes::from(a_twenty_millisecond_stereo_opus_packet()),
                    is_sync_point: true,
                    group_index: 0,
                    sequence_index: 0,
                    sample_rate: 48_000,
                    channels: 2,
                    sample_count: 960,
                    pre_skip: PUBLISHED_OPUS_PRE_SKIP,
                    timestamp_ns: 1,
                }),
            ));

        let captured_levels = TracingEventLevelsCapturedWhileASubscriberRuns::default();
        tracing::subscriber::with_default(captured_levels.clone(), || subscriber.close());

        assert_eq!(
            captured_levels.levels(),
            vec![tracing::Level::INFO],
            "media that arrived, was decoded into a bag and was then thrown away is the same \
             class of loss as every other one this module reports"
        );
    }

    #[test]
    fn one_track_name_cannot_carry_both_media_because_one_track_is_one_medium() {
        let refusal = MoqBroadcastSubscriber::new(
            a_relay_config(),
            MoqContainerFormat::StreamlibBag,
            Some("both".to_owned()),
            Some("both".to_owned()),
            None,
        )
        .map(drop)
        .expect_err("one track is one medium");
        assert!(matches!(refusal, MoqExtensionError::Refused { .. }));
    }

    #[test]
    fn a_track_named_as_the_empty_string_is_refused_rather_than_subscribed_to() {
        let refusal = MoqBroadcastSubscriber::new(
            a_relay_config(),
            MoqContainerFormat::StreamlibBag,
            Some(String::new()),
            None,
            None,
        )
        .map(drop)
        .expect_err("the empty string names no track on the relay");
        assert!(matches!(refusal, MoqExtensionError::Refused { .. }));
    }

    #[test]
    fn a_track_written_on_a_non_nanosecond_timescale_still_stamps_in_nanoseconds() {
        let timescale = NonZeroU32::new(90_000).expect("90 kHz is not zero");
        assert_eq!(
            nanoseconds_of_track_ticks(90_000, timescale),
            1_000_000_000,
            "one second of the MPEG-2 system clock is one second"
        );
        assert_eq!(nanoseconds_of_track_ticks(0, timescale), 0);
    }

    #[test]
    fn an_opus_packet_claiming_more_than_the_standard_allows_is_refused_by_name() {
        // A code-3 packet states its own frame count in the byte after the TOC,
        // so a corrupt count is the one way a packet can claim more audio than
        // Opus permits. Config 3 is SILK at 60 ms; 3 frames of it already
        // exceed the 120 ms ceiling.
        let table_of_contents = (3u8 << 3) | 0b11;
        let refusal = opus_sample_count_of_the_packets_table_of_contents(&[table_of_contents, 3])
            .expect_err("180 ms of audio is more than one packet may carry");

        assert!(refusal.to_string().contains("120 ms"), "{refusal}");
    }

    #[test]
    fn the_longest_packet_the_standard_allows_is_still_read() {
        // Two 60 ms SILK frames are exactly 120 ms, which is legal.
        let table_of_contents = (3u8 << 3) | 0b01;
        let samples = opus_sample_count_of_the_packets_table_of_contents(&[table_of_contents])
            .expect("120 ms is the ceiling, not past it");

        assert_eq!(samples, 120 * 48);
    }
}
