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
use crate::cmaf_track_timeline::{OPUS_TRACK_TIMESCALE_HZ, rescale_nanoseconds};
use crate::encoded_media_sample::{
    EncodedAudioPacket, EncodedMediaSample, EncodedVideoAccessUnit, TrackMedium,
};
use crate::error::{MoqExtensionError, Result};
use crate::monotonic_clock::monotonic_now_ns;
use crate::moq_broadcast_catalog::{CMAF_PACKAGING, INIT_TRACK_NAME};
use crate::moq_broadcast_publisher::MoqContainerFormat;
use crate::moq_relay_config::MoqRelayConfig;
use crate::moq_session::MoqBroadcastSubscribingSession;
use crate::streamlib_bag_object::decode_object;

/// What a refusal from this path calls the session it was reading.
const SUBSCRIBING_SESSION_ROLE: &str = "subscribing";

/// One subscribed broadcast, read as encoded bags.
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
    samples_awaiting_the_reader: VecDeque<EncodedMediaSample>,
}

impl MoqBroadcastSubscriber {
    /// Describe a subscription: where to connect, which container to read, and
    /// which track carries each medium.
    pub(crate) fn new(
        relay_config: MoqRelayConfig,
        container_format: MoqContainerFormat,
        video_track_name: Option<String>,
        audio_track_name: Option<String>,
    ) -> Result<Self> {
        if video_track_name.is_none() && audio_track_name.is_none() {
            return Err(MoqExtensionError::Refused {
                what: "a subscriber naming neither a video track nor an audio track subscribes \
                       to nothing and publishes nothing, which reads from outside as a hang — \
                       name at least one of `video_track` and `audio_track`"
                    .to_owned(),
            });
        }
        for (medium, configured_track_name) in [
            (TrackMedium::Video, &video_track_name),
            (TrackMedium::Audio, &audio_track_name),
        ] {
            if configured_track_name.as_deref().is_some_and(str::is_empty) {
                return Err(MoqExtensionError::Refused {
                    what: format!(
                        "`{}_track` is the empty string, which names no track on the relay; \
                         leave it unset to subscribe to no {} at all",
                        medium.as_str(),
                        medium.as_str()
                    ),
                });
            }
        }
        if video_track_name.is_some() && video_track_name == audio_track_name {
            return Err(MoqExtensionError::Refused {
                what: format!(
                    "`video_track` and `audio_track` are both `{}`, and one track is one medium, \
                     so every object on it would be decoded twice under two different contracts",
                    video_track_name.as_deref().unwrap_or_default()
                ),
            });
        }

        let mut subscribed_track_names = Vec::with_capacity(3);
        if container_format == MoqContainerFormat::Cmaf {
            // First, and subscribed to even when it is the only track that ever
            // sends: a fragment is undecodable without the parameter sets, the
            // coded extent and the Opus parameters this object carries.
            subscribed_track_names.push(INIT_TRACK_NAME.to_owned());
        }
        subscribed_track_names.extend(video_track_name.iter().cloned());
        subscribed_track_names.extend(audio_track_name.iter().cloned());

        Ok(Self {
            relay_config,
            subscribed_track_names,
            received_object_router: ReceivedMoqObjectToEncodedSampleRouter::of(
                container_format,
                video_track_name,
                audio_track_name,
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

    /// The next bag, or `None` if none was reconstituted inside `timeout`.
    ///
    /// `None` is also what an object that is not itself a sample returns — the
    /// init segment, an object on a track this subscriber did not name — so a
    /// caller polls rather than treating one `None` as end of stream.
    pub(crate) async fn next_sample(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<EncodedMediaSample>> {
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
    pub(crate) fn close(&mut self) {
        if let Some(subscribing_session) = self.subscribing_session.take() {
            subscribing_session.close();
        }
        let unread = self.samples_awaiting_the_reader.len();
        if unread > 0 {
            tracing::debug!(
                unread,
                "the subscriber closed with samples nothing had read yet"
            );
        }
        self.samples_awaiting_the_reader.clear();
    }
}

/// Everything that turns one arriving object into bags, and nothing that
/// touches a socket — so the object source is a seam a test feeds by hand.
struct ReceivedMoqObjectToEncodedSampleRouter {
    container_format: MoqContainerFormat,
    video_track_name: Option<String>,
    audio_track_name: Option<String>,
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
}

impl ReceivedMoqObjectToEncodedSampleRouter {
    fn of(
        container_format: MoqContainerFormat,
        video_track_name: Option<String>,
        audio_track_name: Option<String>,
    ) -> Self {
        Self {
            container_format,
            video_track_name,
            audio_track_name,
            the_init_segment_has_arrived: false,
            video_track_reconstitution: None,
            audio_track_reconstitution: None,
            media_objects_dropped_before_the_init_segment_arrived: 0,
            the_pre_init_drop_has_been_reported: false,
            track_names_reported_as_unnamed: BTreeSet::new(),
        }
    }

    /// The bags one object carries: none for an object that is not a sample.
    fn route_received_object(
        &mut self,
        track_name: &str,
        payload: &[u8],
    ) -> Result<Vec<EncodedMediaSample>> {
        match self.container_format {
            MoqContainerFormat::StreamlibBag => {
                let Some(medium) = self.medium_of_track(track_name) else {
                    self.report_an_object_on_a_track_this_subscriber_did_not_name(track_name);
                    return Ok(Vec::new());
                };
                Ok(vec![decode_object(payload, medium)?])
            }
            MoqContainerFormat::Cmaf => self.route_a_cmaf_object(track_name, payload),
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
        let Some(medium) = self.medium_of_track(track_name) else {
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
            if let Some(video_parameters) = description.video_parameters {
                if self.video_track_reconstitution.is_some() {
                    tracing::warn!(
                        track_id = description.track_id,
                        "the init segment describes a second video track; this subscriber has \
                         one video port, so the later description is ignored"
                    );
                    continue;
                }
                self.video_track_reconstitution = Some(CmafVideoTrackReconstitution {
                    codec: description.codec,
                    media_timescale_hz,
                    parameter_set_nal_units: video_parameters.parameter_set_nal_units,
                    coded_width: video_parameters.coded_width,
                    coded_height: video_parameters.coded_height,
                    ordering_pair_counter: SubscriberMintedOrderingPairCounter::default(),
                    stamp_anchor: None,
                });
            } else if let Some(opus_parameters) = description.opus_parameters {
                if self.audio_track_reconstitution.is_some() {
                    tracing::warn!(
                        track_id = description.track_id,
                        "the init segment describes a second audio track; this subscriber has \
                         one audio port, so the later description is ignored"
                    );
                    continue;
                }
                self.audio_track_reconstitution = Some(CmafAudioTrackReconstitution {
                    codec: description.codec,
                    media_timescale_hz,
                    channels: opus_parameters.channels,
                    sample_rate: opus_parameters.sample_rate,
                    pre_skip: opus_parameters.pre_skip,
                    ordering_pair_counter: SubscriberMintedOrderingPairCounter::default(),
                    stamp_anchor: None,
                });
            } else {
                tracing::warn!(
                    track_id = description.track_id,
                    codec = %description.codec,
                    "the init segment describes a track this subscriber cannot reconstitute a \
                     bag from; its fragments will be refused if any arrive"
                );
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
            let sample_count = opus_samples_of_track_ticks(
                u64::from(fragment_sample.duration),
                audio.media_timescale_hz,
            )?;
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

    fn medium_of_track(&self, track_name: &str) -> Option<TrackMedium> {
        if self.video_track_name.as_deref() == Some(track_name) {
            Some(TrackMedium::Video)
        } else if self.audio_track_name.as_deref() == Some(track_name) {
            Some(TrackMedium::Audio)
        } else {
            None
        }
    }

    /// Ignore the object, and say so once per track rather than once per
    /// object: a relay that misroutes one object misroutes all of them.
    fn report_an_object_on_a_track_this_subscriber_did_not_name(&mut self, track_name: &str) {
        if self
            .track_names_reported_as_unnamed
            .insert(track_name.to_owned())
        {
            tracing::warn!(
                track = %track_name,
                "an object arrived on a track this subscriber did not name; ignoring it, \
                 because neither output port has a contract for it"
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

/// A fragment's sample duration as the Opus sample count the bag states.
///
/// A track written on Opus's own 48 kHz clock makes this the identity, which is
/// what the publisher writes; the rescale is here for a track that was not.
fn opus_samples_of_track_ticks(ticks: u64, media_timescale_hz: NonZeroU32) -> Result<u32> {
    let samples = rescale_nanoseconds(
        nanoseconds_of_track_ticks(ticks, media_timescale_hz),
        OPUS_TRACK_TIMESCALE_HZ,
    );
    u32::try_from(samples).map_err(|_| MoqExtensionError::MalformedObject {
        container: CMAF_PACKAGING,
        what: format!(
            "a fragment claims a duration of {ticks} ticks at {media_timescale_hz} Hz, which is \
             {samples} Opus samples — more than an encoded-audio bag's `sample_count` states"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annex_b_access_unit::{
        ANNEX_B_START_CODE, AnnexBNalHeaderGrammar, length_prefix_annex_b_access_unit,
    };
    use crate::cmaf_fragment::{CMAF_FRAGMENT_TRACK_ID, build_cmaf_fragment};
    use crate::cmaf_init_segment::{
        CmafTrackDescriptionForTheInitSegment, build_cmaf_init_segment,
    };
    use crate::cmaf_sample_entry::{build_opus_sample_entry, build_video_sample_entry};
    use crate::cmaf_track_timeline::VIDEO_TRACK_TIMESCALE_HZ;
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
        )
        .expect("the fixture names at least one track, which is the only refusal `new` makes")
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

    fn a_video_init_segment_description() -> CmafTrackDescriptionForTheInitSegment {
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
            320,
            180,
        )
        .expect("the fixture parameter sets describe an H.264 track");
        CmafTrackDescriptionForTheInitSegment {
            track_id: VIDEO_TRACK_ID,
            inbound_link_name: "encoder/encoded_video".to_owned(),
            cmaf_track_sample_entry: sample_entry.cmaf_track_sample_entry,
            media_timescale_hz: VIDEO_TRACK_TIMESCALE_HZ,
            coded_extent: Some((320, 180)),
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

    fn an_audio_fragment(sequence_number: u32, decode_time: u64, packet: &[u8]) -> bytes::Bytes {
        build_cmaf_fragment(
            AUDIO_TRACK_ID,
            sequence_number,
            decode_time,
            // 20 ms at 48 kHz, which is the frame every Opus encoder in this
            // tree mints.
            960,
            true,
            packet,
        )
        .expect("the fixture packet is small enough for an mdat")
    }

    fn the_only_video_access_unit(bags: Vec<EncodedMediaSample>) -> EncodedVideoAccessUnit {
        match bags.into_iter().next() {
            Some(EncodedMediaSample::VideoAccessUnit(unit)) => unit,
            other => panic!("expected exactly one video access unit, got {other:?}"),
        }
    }

    fn the_only_audio_packet(bags: Vec<EncodedMediaSample>) -> EncodedAudioPacket {
        match bags.into_iter().next() {
            Some(EncodedMediaSample::AudioPacket(packet)) => packet,
            other => panic!("expected exactly one Opus packet, got {other:?}"),
        }
    }

    fn contains_the_byte_run(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn a_subscriber_naming_no_track_at_all_is_refused_rather_than_left_to_produce_nothing() {
        let refusal =
            MoqBroadcastSubscriber::new(a_relay_config(), MoqContainerFormat::Cmaf, None, None)
                .map(drop)
                .expect_err("naming neither track subscribes to nothing");
        assert!(
            matches!(refusal, MoqExtensionError::Refused { .. }),
            "a caller that named no track passed something wrong, so this is a ValueError on the \
             Python side; got {refusal:?}"
        );
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
                &encode_object(&published).expect("the fixture bag encodes"),
            )
            .expect("the object is this subscriber's own container");

        assert_eq!(
            bags,
            vec![published],
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
            (320, 180),
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

        let opus_packet_bytes: Vec<u8> = vec![0xFC, 0x01, 0x02, 0x03, 0x04];
        let packet = the_only_audio_packet(
            object_router
                .route_received_object(
                    &media_track_name(AUDIO_TRACK_ID),
                    &an_audio_fragment(1, 0, &opus_packet_bytes),
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
        assert_eq!(
            packet.sample_count, 960,
            "an Opus track is written on Opus's own 48 kHz clock, so a fragment's duration is \
             its sample count"
        );
        assert!(
            packet.is_sync_point,
            "every Opus packet is a decode entry point"
        );
        assert_eq!(packet.opus_packet, bytes::Bytes::from(opus_packet_bytes));
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

        let refusal = object_router
            .route_received_object(
                &media_track_name(AUDIO_TRACK_ID),
                &an_audio_fragment(1, 0, &[0xFC, 0x00]),
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
    fn one_track_name_cannot_carry_both_media_because_one_track_is_one_medium() {
        let refusal = MoqBroadcastSubscriber::new(
            a_relay_config(),
            MoqContainerFormat::StreamlibBag,
            Some("both".to_owned()),
            Some("both".to_owned()),
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
}
