// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Turning received RTP into everything an encoded bag has to carry.
//!
//! RTP states none of it: no extent, no colour, no ordering pair, no sample
//! count, no channel count. Each one comes from the stream itself — the SPS,
//! the access unit, the Opus TOC — because a player that took them from config
//! gets them wrong the first time the far end changes.

use crate::encoded_stream_ordering::EncodedStreamOrderingPairCounter;
use crate::error::Result;
use crate::h264_rtp_depacketiser::{
    H264RtpDepacketiser, NAL_TYPE_IDR_SLICE, NAL_TYPE_SEQUENCE_PARAMETER_SET,
};
use crate::h264_sequence_parameter_set::{
    ColorDescription, SequenceParameterSet, parse_sequence_parameter_set,
};
use crate::monotonic_clock::RtpClockAnchoredToMonotonic;
use crate::opus_packet::{OPUS_WIRE_SAMPLE_RATE_HZ, describe_opus_packet};
use bytes::Bytes;

use crate::webrtc_peer_connection::H264_CLOCK_RATE_HZ;
/// RFC 6716 §4.2: an Opus stream's encoder lookahead is signalled in the
/// container's header, and RTP has no container. A player states no skip
/// rather than inventing one, so a decoder trims nothing.
const PRE_SKIP_RTP_CARRIES_NONE: u32 = 0;
const ANNEX_B_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// One whole access unit, with everything the video wire contract requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceivedVideoAccessUnit {
    pub annex_b_access_unit: Vec<u8>,
    pub is_sync_point: bool,
    pub group_index: u64,
    pub sequence_index: u64,
    pub width: u32,
    pub height: u32,
    pub color: Option<ColorDescription>,
    pub timestamp_ns: i64,
}

/// One Opus packet, with everything the audio wire contract requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceivedOpusPacket {
    pub opus_packet: Bytes,
    pub group_index: u64,
    pub sequence_index: u64,
    pub sample_rate: u32,
    pub channels: u32,
    pub sample_count: u32,
    pub pre_skip: u32,
    pub timestamp_ns: i64,
}

/// Collects RTP payloads into whole access units.
pub(crate) struct VideoAccessUnitAssembler {
    depacketiser: H264RtpDepacketiser,
    nal_units_in_progress: Vec<Bytes>,
    rtp_timestamp_in_progress: Option<u32>,
    latched_sequence_parameter_set: Option<SequenceParameterSet>,
    ordering: EncodedStreamOrderingPairCounter,
    clock: RtpClockAnchoredToMonotonic,
    reported_waiting_for_a_parameter_set: bool,
}

impl Default for VideoAccessUnitAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoAccessUnitAssembler {
    pub(crate) fn new() -> Self {
        Self {
            depacketiser: H264RtpDepacketiser::new(),
            nal_units_in_progress: Vec::new(),
            rtp_timestamp_in_progress: None,
            latched_sequence_parameter_set: None,
            ordering: EncodedStreamOrderingPairCounter::default(),
            clock: RtpClockAnchoredToMonotonic::new(H264_CLOCK_RATE_HZ),
            reported_waiting_for_a_parameter_set: false,
        }
    }

    /// Feed one RTP packet; hands back every access unit it completed.
    ///
    /// RFC 6184 §5.1 sets the marker bit on an access unit's last packet. A
    /// timestamp change closes one too, so a sender that omits the marker does
    /// not merge every picture into one growing unit.
    ///
    /// Both can fire on one packet — a new timestamp closes the unit before it
    /// while the packet's own marker closes a single-packet unit of its own —
    /// which is reachable whenever the previous unit's marker packet was lost.
    /// Returning only one of the two would spend an ordering pair on a unit
    /// nobody receives, and the gap reads downstream as loss of the whole group
    /// rather than of the one packet that actually went missing.
    pub(crate) fn accept_rtp_packet(
        &mut self,
        payload: Bytes,
        rtp_timestamp: u32,
        rtp_sequence_number: u16,
        marks_the_end_of_an_access_unit: bool,
    ) -> Vec<ReceivedVideoAccessUnit> {
        let mut completed = Vec::new();

        if self
            .rtp_timestamp_in_progress
            .is_some_and(|in_progress| in_progress != rtp_timestamp)
        {
            completed.extend(self.complete_access_unit());
        }

        match self
            .depacketiser
            .depacketise(payload, rtp_timestamp, rtp_sequence_number)
        {
            Ok(nal_units) => self.nal_units_in_progress.extend(nal_units),
            Err(refusal) => tracing::warn!(%refusal, "an RTP payload was dropped"),
        }
        self.rtp_timestamp_in_progress = Some(rtp_timestamp);
        self.depacketiser.discard_stale_fragments(rtp_timestamp);

        if marks_the_end_of_an_access_unit {
            completed.extend(self.complete_access_unit());
        }
        completed
    }

    fn complete_access_unit(&mut self) -> Option<ReceivedVideoAccessUnit> {
        let nal_units = std::mem::take(&mut self.nal_units_in_progress);
        let rtp_timestamp = self.rtp_timestamp_in_progress.take()?;
        if nal_units.is_empty() {
            return None;
        }

        for nal_unit in &nal_units {
            if nal_unit
                .first()
                .is_some_and(|header| header & 0x1F == NAL_TYPE_SEQUENCE_PARAMETER_SET)
            {
                match parse_sequence_parameter_set(nal_unit) {
                    Ok(parsed) => self.latched_sequence_parameter_set = Some(parsed),
                    Err(refusal) => {
                        tracing::warn!(%refusal, "a sequence parameter set could not be read")
                    }
                }
            }
        }

        // Every required key must be present, and only the SPS states the
        // extent — so a stream entered before its first one has nothing to
        // publish yet. A decoder could not have entered before it either.
        let Some(parameters) = self.latched_sequence_parameter_set.clone() else {
            if !self.reported_waiting_for_a_parameter_set {
                self.reported_waiting_for_a_parameter_set = true;
                tracing::info!(
                    "waiting for the stream's first sequence parameter set before publishing"
                );
            }
            return None;
        };

        let is_sync_point = nal_units.iter().any(|nal_unit| {
            nal_unit
                .first()
                .is_some_and(|header| header & 0x1F == NAL_TYPE_IDR_SLICE)
        });

        let mut annex_b_access_unit =
            Vec::with_capacity(nal_units.iter().map(|unit| unit.len() + 4).sum());
        for nal_unit in &nal_units {
            annex_b_access_unit.extend_from_slice(&ANNEX_B_START_CODE);
            annex_b_access_unit.extend_from_slice(nal_unit);
        }

        let pair = self.ordering.account_published_bag(is_sync_point);
        Some(ReceivedVideoAccessUnit {
            annex_b_access_unit,
            is_sync_point,
            group_index: pair.group_index,
            sequence_index: pair.sequence_index,
            width: parameters.width,
            height: parameters.height,
            color: parameters.color,
            timestamp_ns: self.clock.stamp_for(rtp_timestamp),
        })
    }
}

/// Describes each arriving Opus packet from the packet itself.
pub(crate) struct OpusPacketAssembler {
    ordering: EncodedStreamOrderingPairCounter,
    clock: RtpClockAnchoredToMonotonic,
    /// The stream's declared output channel count, settled once and then
    /// constant. The wire contract's `channels` is "the declared output count,
    /// not the mono/stereo the TOC byte codes each frame at" — libopus in
    /// two-channel mode codes mono frames on quiet or mono-ish content, so a
    /// per-packet TOC reading would flip mid-stream and re-mint the decoder
    /// downstream on every flip.
    declared_channels: Option<u32>,
    sender_declared_stereo: Option<bool>,
    reported_a_disagreement_with_the_answer: bool,
}

impl OpusPacketAssembler {
    /// `sender_declared_stereo` is the answer's `sprop-stereo` — the session
    /// description's own statement of what the sender will send, and the
    /// contract's preferred source for `channels`.
    pub(crate) fn new(sender_declared_stereo: Option<bool>) -> Self {
        Self {
            ordering: EncodedStreamOrderingPairCounter::default(),
            clock: RtpClockAnchoredToMonotonic::new(OPUS_WIRE_SAMPLE_RATE_HZ),
            declared_channels: sender_declared_stereo.map(|stereo| if stereo { 2 } else { 1 }),
            sender_declared_stereo,
            reported_a_disagreement_with_the_answer: false,
        }
    }

    pub(crate) fn accept_rtp_packet(
        &mut self,
        payload: Bytes,
        rtp_timestamp: u32,
    ) -> Result<ReceivedOpusPacket> {
        let described = describe_opus_packet(&payload)?;
        // An answer that stated no Opus fmtp leaves the first packet's own TOC
        // as the only statement available — latched here, so the stream's
        // declared count is settled once either way.
        let channels = *self.declared_channels.get_or_insert(described.channels);
        self.report_any_disagreement_with_the_answer(described.channels);

        // Every Opus packet is a sync point, so each is its own group.
        let pair = self.ordering.account_published_bag(true);
        Ok(ReceivedOpusPacket {
            opus_packet: payload,
            group_index: pair.group_index,
            sequence_index: pair.sequence_index,
            sample_rate: OPUS_WIRE_SAMPLE_RATE_HZ,
            channels,
            sample_count: described.sample_count,
            pre_skip: PRE_SKIP_RTP_CARRIES_NONE,
            timestamp_ns: self.clock.stamp_for(rtp_timestamp),
        })
    }

    /// Reported once, because a sender that codes a mono frame inside a stereo
    /// stream is doing something ordinary and the bags do not follow it — but a
    /// relay whose fmtp is simply wrong should leave evidence rather than a
    /// mismatch nobody can trace later.
    fn report_any_disagreement_with_the_answer(&mut self, channels_in_the_packet: u32) {
        let Some(declared_stereo) = self.sender_declared_stereo else {
            return;
        };
        let packet_is_stereo = channels_in_the_packet > 1;
        if declared_stereo != packet_is_stereo && !self.reported_a_disagreement_with_the_answer {
            self.reported_a_disagreement_with_the_answer = true;
            tracing::warn!(
                declared_stereo,
                channels_in_the_packet,
                "a packet's TOC codes a different channel count than the answer's \
                 sprop-stereo declared; the bags carry the declared count"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h264_test_bitstreams::{baseline_320x180, no_vui, vui_with_colour};

    /// F=0, NRI=3, type=5 (IDR slice).
    const IDR_SLICE: &[u8] = &[0x65, 0x11, 0x22, 0x33];
    /// F=0, NRI=2, type=1 (non-IDR slice).
    const NON_IDR_SLICE: &[u8] = &[0x41, 0x44, 0x55];
    /// F=0, NRI=3, type=8 (picture parameter set).
    const PICTURE_PARAMETER_SET: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];

    fn single_nal_packet(nal_unit: &[u8]) -> Bytes {
        Bytes::from(nal_unit.to_vec())
    }

    fn annex_b_of(nal_units: &[&[u8]]) -> Vec<u8> {
        let mut assembled = Vec::new();
        for nal_unit in nal_units {
            assembled.extend_from_slice(&ANNEX_B_START_CODE);
            assembled.extend_from_slice(nal_unit);
        }
        assembled
    }

    /// The shape a WHIP sender puts a keyframe on the wire in: parameter sets
    /// then the IDR, all under one RTP timestamp, marker on the last packet.
    fn feed_a_keyframe(
        assembler: &mut VideoAccessUnitAssembler,
        sequence_parameter_set: &[u8],
        rtp_timestamp: u32,
        first_sequence_number: u16,
    ) -> Option<ReceivedVideoAccessUnit> {
        assembler.accept_rtp_packet(
            single_nal_packet(sequence_parameter_set),
            rtp_timestamp,
            first_sequence_number,
            false,
        );
        assembler.accept_rtp_packet(
            single_nal_packet(PICTURE_PARAMETER_SET),
            rtp_timestamp,
            first_sequence_number + 1,
            false,
        );
        assembler
            .accept_rtp_packet(
                single_nal_packet(IDR_SLICE),
                rtp_timestamp,
                first_sequence_number + 2,
                true,
            )
            .into_iter()
            .next()
    }

    /// The one unit this packet completed, refusing a test that expected one
    /// and got two.
    fn only_completed_unit(
        completed: Vec<ReceivedVideoAccessUnit>,
    ) -> Option<ReceivedVideoAccessUnit> {
        assert!(completed.len() <= 1, "expected at most one completed unit");
        completed.into_iter().next()
    }

    #[test]
    fn a_keyframes_parameter_sets_and_slice_arrive_as_one_access_unit() {
        let mut assembler = VideoAccessUnitAssembler::new();
        let sps = baseline_320x180(no_vui);

        let unit = feed_a_keyframe(&mut assembler, &sps, 3000, 1).unwrap();

        // One bag, not three: what a decoder is handed is an access unit.
        assert_eq!(
            unit.annex_b_access_unit,
            annex_b_of(&[&sps, PICTURE_PARAMETER_SET, IDR_SLICE])
        );
        assert!(unit.is_sync_point);
    }

    #[test]
    fn the_coded_extent_comes_from_the_sps_the_stream_carried() {
        let mut assembler = VideoAccessUnitAssembler::new();

        let unit = feed_a_keyframe(&mut assembler, &baseline_320x180(no_vui), 3000, 1).unwrap();

        assert_eq!((unit.width, unit.height), (320, 192));
        assert_eq!(unit.color, None);
    }

    #[test]
    fn the_colour_comes_from_the_vui_when_the_stream_describes_one() {
        let mut assembler = VideoAccessUnitAssembler::new();
        let sps = baseline_320x180(vui_with_colour(1, 13, 1, 0));

        let unit = feed_a_keyframe(&mut assembler, &sps, 3000, 1).unwrap();

        assert_eq!(
            unit.color,
            Some(ColorDescription {
                primaries: Some("bt709"),
                transfer: Some("srgb"),
                matrix: Some("bt709"),
                range: Some("limited"),
            })
        );
    }

    #[test]
    fn a_later_picture_without_its_own_sps_still_carries_the_extent() {
        let mut assembler = VideoAccessUnitAssembler::new();
        feed_a_keyframe(&mut assembler, &baseline_320x180(no_vui), 3000, 1);

        let unit = only_completed_unit(assembler.accept_rtp_packet(
            single_nal_packet(NON_IDR_SLICE),
            6000,
            4,
            true,
        ))
        .unwrap();

        assert_eq!((unit.width, unit.height), (320, 192));
        assert!(!unit.is_sync_point);
    }

    #[test]
    fn nothing_is_published_before_the_streams_first_parameter_set() {
        let mut assembler = VideoAccessUnitAssembler::new();

        // Joined mid-stream: a picture with no extent to state is not a bag
        // the wire contract can carry, and no decoder could enter here anyway.
        let published =
            assembler.accept_rtp_packet(single_nal_packet(NON_IDR_SLICE), 3000, 1, true);

        assert_eq!(published, vec![]);
    }

    #[test]
    fn the_ordering_pair_counts_groups_by_keyframe_and_never_resets() {
        let mut assembler = VideoAccessUnitAssembler::new();
        let sps = baseline_320x180(no_vui);

        let first = feed_a_keyframe(&mut assembler, &sps, 3000, 1).unwrap();
        let second = only_completed_unit(assembler.accept_rtp_packet(
            single_nal_packet(NON_IDR_SLICE),
            6000,
            4,
            true,
        ))
        .unwrap();
        let third = feed_a_keyframe(&mut assembler, &sps, 9000, 5).unwrap();

        assert_eq!(
            [
                first.sequence_index,
                second.sequence_index,
                third.sequence_index
            ],
            [0, 1, 2]
        );
        assert_eq!(
            [first.group_index, second.group_index, third.group_index],
            [0, 0, 1]
        );
    }

    #[test]
    fn a_timestamp_change_closes_an_access_unit_a_sender_never_marked() {
        let mut assembler = VideoAccessUnitAssembler::new();
        let sps = baseline_320x180(no_vui);
        assembler.accept_rtp_packet(single_nal_packet(&sps), 3000, 1, false);
        assembler.accept_rtp_packet(single_nal_packet(IDR_SLICE), 3000, 2, false);

        // No marker ever arrives; the next picture's timestamp is what closes
        // the one before it.
        let closed = only_completed_unit(assembler.accept_rtp_packet(
            single_nal_packet(NON_IDR_SLICE),
            6000,
            3,
            false,
        ))
        .unwrap();

        assert_eq!(closed.annex_b_access_unit, annex_b_of(&[&sps, IDR_SLICE]));
        assert!(closed.is_sync_point);
    }

    #[test]
    fn the_stamps_advance_by_the_rtp_clock_not_by_arrival() {
        let mut assembler = VideoAccessUnitAssembler::new();
        let sps = baseline_320x180(no_vui);
        let first = feed_a_keyframe(&mut assembler, &sps, 3000, 1).unwrap();

        let second = only_completed_unit(assembler.accept_rtp_packet(
            single_nal_packet(NON_IDR_SLICE),
            6000,
            4,
            true,
        ))
        .unwrap();

        assert_eq!(second.timestamp_ns - first.timestamp_ns, 33_333_333);
    }

    #[test]
    fn one_packet_can_close_two_access_units_and_neither_is_dropped() {
        // Reachable whenever the previous unit's marker packet was lost: the
        // new timestamp closes what came before while this packet's own marker
        // closes a single-packet unit of its own. Returning one of the two
        // would spend an ordering pair on a unit nobody receives, and the gap
        // reads downstream as loss of a whole group.
        let mut assembler = VideoAccessUnitAssembler::new();
        let sps = baseline_320x180(no_vui);
        assembler.accept_rtp_packet(single_nal_packet(&sps), 3000, 1, false);
        assembler.accept_rtp_packet(single_nal_packet(IDR_SLICE), 3000, 2, false);

        let completed =
            assembler.accept_rtp_packet(single_nal_packet(NON_IDR_SLICE), 6000, 3, true);

        assert_eq!(completed.len(), 2, "one unit was dropped");
        assert_eq!(
            completed[0].annex_b_access_unit,
            annex_b_of(&[&sps, IDR_SLICE])
        );
        assert_eq!(
            completed[1].annex_b_access_unit,
            annex_b_of(&[NON_IDR_SLICE])
        );
        // The pair is continuous across both, so a consumer sees no loss.
        assert_eq!(
            (completed[0].sequence_index, completed[1].sequence_index),
            (0, 1)
        );
    }

    #[test]
    fn a_malformed_payload_costs_its_own_packet_and_not_the_stream() {
        let mut assembler = VideoAccessUnitAssembler::new();
        let sps = baseline_320x180(no_vui);
        assembler.accept_rtp_packet(single_nal_packet(&sps), 3000, 1, false);
        assembler.accept_rtp_packet(Bytes::new(), 3000, 2, false);

        let unit = only_completed_unit(assembler.accept_rtp_packet(
            single_nal_packet(IDR_SLICE),
            3000,
            3,
            true,
        ))
        .unwrap();

        assert_eq!(unit.annex_b_access_unit, annex_b_of(&[&sps, IDR_SLICE]));
    }

    fn opus_packet(configuration: u8, stereo: bool) -> Bytes {
        Bytes::from(vec![(configuration << 3) | (u8::from(stereo) << 2), 0x00])
    }

    #[test]
    fn the_channel_count_comes_from_the_answers_declaration() {
        // Not from the rtpmap, which RFC 7587 §7 fixes at 2 for every Opus
        // stream ever negotiated, and not from the TOC, which codes a frame
        // rather than declaring a stream.
        let mut assembler = OpusPacketAssembler::new(Some(false));

        let packet = assembler
            .accept_rtp_packet(opus_packet(1, false), 0)
            .unwrap();

        assert_eq!(packet.channels, 1);
        assert_eq!(packet.sample_count, 960);
        assert_eq!(packet.sample_rate, 48_000);
    }

    #[test]
    fn the_channel_count_does_not_flip_when_a_frames_toc_does() {
        // libopus in two-channel mode codes a mono frame on quiet or mono-ish
        // content. The wire contract's `channels` is the stream's declared
        // output count, and a decoder re-mints and resets state on every change
        // of it — so following the TOC per packet would restart the decoder
        // mid-stream on ordinary content.
        let mut assembler = OpusPacketAssembler::new(Some(true));

        let channels: Vec<u32> = [true, false, false, true]
            .into_iter()
            .enumerate()
            .map(|(index, stereo_frame)| {
                assembler
                    .accept_rtp_packet(opus_packet(1, stereo_frame), index as u32 * 960)
                    .unwrap()
                    .channels
            })
            .collect();

        assert_eq!(channels, vec![2, 2, 2, 2]);
    }

    #[test]
    fn an_answer_that_declared_nothing_latches_the_first_packets_own_count() {
        // A relay whose answer carries no Opus fmtp leaves the first packet as
        // the only statement available — and it is still latched, so the
        // stream's count is settled once either way.
        let mut assembler = OpusPacketAssembler::new(None);

        let first = assembler
            .accept_rtp_packet(opus_packet(1, false), 0)
            .unwrap();
        let second = assembler
            .accept_rtp_packet(opus_packet(1, true), 960)
            .unwrap();

        assert_eq!((first.channels, second.channels), (1, 1));
    }

    #[test]
    fn every_opus_packet_is_its_own_group() {
        let mut assembler = OpusPacketAssembler::new(None);

        let pairs: Vec<_> = (0..3)
            .map(|index| {
                let packet = assembler
                    .accept_rtp_packet(opus_packet(1, true), index * 960)
                    .unwrap();
                (packet.group_index, packet.sequence_index)
            })
            .collect();

        assert_eq!(pairs, vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn the_pre_skip_is_zero_because_rtp_carries_no_opus_head() {
        let mut assembler = OpusPacketAssembler::new(None);

        let packet = assembler
            .accept_rtp_packet(opus_packet(1, true), 0)
            .unwrap();

        assert_eq!(packet.pre_skip, 0);
    }

    #[test]
    fn opus_stamps_advance_over_the_forty_eight_kilohertz_clock() {
        let mut assembler = OpusPacketAssembler::new(None);
        let first = assembler
            .accept_rtp_packet(opus_packet(1, true), 0)
            .unwrap();

        let second = assembler
            .accept_rtp_packet(opus_packet(1, true), 960)
            .unwrap();

        assert_eq!(second.timestamp_ns - first.timestamp_ns, 20_000_000);
    }

    #[test]
    fn a_malformed_opus_packet_is_refused_rather_than_bagged() {
        let mut assembler = OpusPacketAssembler::new(None);

        assert!(assembler.accept_rtp_packet(Bytes::new(), 0).is_err());
    }
}
