// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RFC 6184 depacketisation: RTP payloads in, whole NAL units out.

use crate::error::{Result, WebRtcExtensionError};
use bytes::Bytes;
use std::collections::HashMap;

/// RFC 6184 §5.2 packetisation modes this reads. Everything outside this set
/// is a single NAL unit, which needs no reassembly.
const NAL_TYPE_STAP_A: u8 = 24;
const NAL_TYPE_FU_A: u8 = 28;

/// RFC 6184 §1.3: an IDR picture's coded slice, the sync point a decoder may
/// enter a stream at.
pub const NAL_TYPE_IDR_SLICE: u8 = 5;
/// The sequence parameter set, which is the only place a stream states its
/// extent and its colour.
pub const NAL_TYPE_SEQUENCE_PARAMETER_SET: u8 = 7;

/// How far apart two RTP timestamps must be before a half-reassembled FU-A is
/// abandoned. 90 kHz × 2 s — long enough that no plausible jitter drops a
/// fragment still in flight, short enough that a stream that never completes
/// one cannot grow the map without bound.
const STALE_FRAGMENT_THRESHOLD_TICKS: u32 = 180_000;

/// Reassembles the NAL units an H.264 RTP stream arrives in.
#[derive(Default)]
pub struct H264RtpDepacketiser {
    fragments_by_rtp_timestamp: HashMap<u32, FragmentedNalUnitUnderReassembly>,
}

struct FragmentedNalUnitUnderReassembly {
    nal_unit_header: u8,
    fragments: Vec<Bytes>,
    next_expected_sequence_number: u16,
    total_fragment_bytes: usize,
}

impl H264RtpDepacketiser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every NAL unit this payload completes — none while a FU-A is still
    /// arriving, one for a single NAL or a completed FU-A, many for a STAP-A.
    pub fn depacketise(
        &mut self,
        payload: Bytes,
        rtp_timestamp: u32,
        rtp_sequence_number: u16,
    ) -> Result<Vec<Bytes>> {
        let Some(&nal_unit_header) = payload.first() else {
            return Err(WebRtcExtensionError::MalformedRtpPayload {
                what: "an empty payload carries no NAL unit header".to_owned(),
            });
        };

        match nal_unit_header & 0x1F {
            NAL_TYPE_FU_A => {
                self.reassemble_fragmentation_unit(payload, rtp_timestamp, rtp_sequence_number)
            }
            NAL_TYPE_STAP_A => split_single_time_aggregation_packet(&payload),
            _ => Ok(vec![payload]),
        }
    }

    /// Drop half-reassembled units older than the threshold, so a sender that
    /// stops mid-fragment cannot leak one entry per abandoned unit.
    pub fn discard_stale_fragments(&mut self, current_rtp_timestamp: u32) {
        self.fragments_by_rtp_timestamp.retain(|&timestamp, _| {
            current_rtp_timestamp.wrapping_sub(timestamp) < STALE_FRAGMENT_THRESHOLD_TICKS
        });
    }

    fn reassemble_fragmentation_unit(
        &mut self,
        payload: Bytes,
        rtp_timestamp: u32,
        rtp_sequence_number: u16,
    ) -> Result<Vec<Bytes>> {
        if payload.len() < 3 {
            return Err(WebRtcExtensionError::MalformedRtpPayload {
                what: format!(
                    "a FU-A needs an indicator, a header and at least one payload byte; got {}",
                    payload.len()
                ),
            });
        }

        let fragmentation_unit_indicator = payload[0];
        let fragmentation_unit_header = payload[1];
        let starts_a_unit = (fragmentation_unit_header & 0x80) != 0;
        let ends_a_unit = (fragmentation_unit_header & 0x40) != 0;

        let fragment = payload.slice(2..);

        if starts_a_unit {
            // RFC 6184 §5.8: the reassembled header takes F and NRI from the
            // indicator and the type from the FU header.
            let nal_unit_header =
                (fragmentation_unit_indicator & 0xE0) | (fragmentation_unit_header & 0x1F);
            let total_fragment_bytes = fragment.len();
            self.fragments_by_rtp_timestamp.insert(
                rtp_timestamp,
                FragmentedNalUnitUnderReassembly {
                    nal_unit_header,
                    fragments: vec![fragment],
                    next_expected_sequence_number: rtp_sequence_number.wrapping_add(1),
                    total_fragment_bytes,
                },
            );
            return Ok(Vec::new());
        }

        // A middle or end fragment with no start means this reader joined the
        // stream mid-unit. Discarding is the only correct move: half a NAL unit
        // handed to a decoder is worse than a gap it can conceal.
        let Some(mut under_reassembly) = self.fragments_by_rtp_timestamp.remove(&rtp_timestamp)
        else {
            tracing::trace!(
                rtp_timestamp,
                rtp_sequence_number,
                "FU-A fragment without a start; joined mid-unit, discarding"
            );
            return Ok(Vec::new());
        };

        if rtp_sequence_number != under_reassembly.next_expected_sequence_number {
            tracing::warn!(
                expected = under_reassembly.next_expected_sequence_number,
                received = rtp_sequence_number,
                "FU-A sequence gap; the reassembled unit is missing fragments"
            );
        }

        under_reassembly.total_fragment_bytes += fragment.len();
        under_reassembly.fragments.push(fragment);
        under_reassembly.next_expected_sequence_number = rtp_sequence_number.wrapping_add(1);

        if !ends_a_unit {
            self.fragments_by_rtp_timestamp
                .insert(rtp_timestamp, under_reassembly);
            return Ok(Vec::new());
        }

        let mut nal_unit = Vec::with_capacity(1 + under_reassembly.total_fragment_bytes);
        nal_unit.push(under_reassembly.nal_unit_header);
        for fragment in &under_reassembly.fragments {
            nal_unit.extend_from_slice(fragment);
        }
        Ok(vec![Bytes::from(nal_unit)])
    }
}

/// RFC 6184 §5.7.1: a STAP-A is a one-byte header then a run of
/// length-prefixed NAL units, each prefix two bytes in network order.
fn split_single_time_aggregation_packet(payload: &Bytes) -> Result<Vec<Bytes>> {
    let mut nal_units = Vec::new();
    let mut offset = 1;

    while offset + 2 <= payload.len() {
        let nal_unit_length = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
        offset += 2;

        if nal_unit_length == 0 {
            return Err(WebRtcExtensionError::MalformedRtpPayload {
                what: "a STAP-A declares a zero-length NAL unit".to_owned(),
            });
        }
        if offset + nal_unit_length > payload.len() {
            return Err(WebRtcExtensionError::MalformedRtpPayload {
                what: format!(
                    "a STAP-A NAL unit of {nal_unit_length} bytes runs past the {} the packet has",
                    payload.len() - offset
                ),
            });
        }

        nal_units.push(payload.slice(offset..offset + nal_unit_length));
        offset += nal_unit_length;
    }

    if nal_units.is_empty() {
        return Err(WebRtcExtensionError::MalformedRtpPayload {
            what: "a STAP-A carrying no NAL unit".to_owned(),
        });
    }
    Ok(nal_units)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F=0, NRI=3, type=28 (FU-A).
    const FU_A_INDICATOR: u8 = 0x7C;
    /// F=0, NRI=3, type=24 (STAP-A).
    const STAP_A_HEADER: u8 = 0x78;
    /// F=0, NRI=3, type=5 (IDR slice).
    const IDR_NAL_UNIT_HEADER: u8 = 0x65;

    fn stap_a(nal_units: &[&[u8]]) -> Bytes {
        let mut packet = vec![STAP_A_HEADER];
        for nal_unit in nal_units {
            packet.extend_from_slice(&(nal_unit.len() as u16).to_be_bytes());
            packet.extend_from_slice(nal_unit);
        }
        Bytes::from(packet)
    }

    fn fragmentation_unit(start: bool, end: bool, nal_type: u8, payload: &[u8]) -> Bytes {
        let header = (u8::from(start) << 7) | (u8::from(end) << 6) | nal_type;
        let mut packet = vec![FU_A_INDICATOR, header];
        packet.extend_from_slice(payload);
        Bytes::from(packet)
    }

    #[test]
    fn a_single_nal_unit_payload_is_the_nal_unit() {
        let mut depacketiser = H264RtpDepacketiser::new();
        let payload = Bytes::from(vec![IDR_NAL_UNIT_HEADER, 0x01, 0x02, 0x03]);

        let nal_units = depacketiser.depacketise(payload.clone(), 1000, 1).unwrap();

        assert_eq!(nal_units, vec![payload]);
    }

    #[test]
    fn three_fragments_reassemble_into_one_nal_unit() {
        let mut depacketiser = H264RtpDepacketiser::new();

        assert!(
            depacketiser
                .depacketise(fragmentation_unit(true, false, 5, &[0x01, 0x02]), 2000, 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            depacketiser
                .depacketise(fragmentation_unit(false, false, 5, &[0x03, 0x04]), 2000, 11)
                .unwrap()
                .is_empty()
        );
        let nal_units = depacketiser
            .depacketise(fragmentation_unit(false, true, 5, &[0x05, 0x06]), 2000, 12)
            .unwrap();

        // The reassembled header takes NRI from the indicator and the type
        // from the FU header, which is what makes this 0x65 and not 0x7C.
        assert_eq!(
            nal_units,
            vec![Bytes::from(vec![
                IDR_NAL_UNIT_HEADER,
                0x01,
                0x02,
                0x03,
                0x04,
                0x05,
                0x06
            ])]
        );
    }

    #[test]
    fn a_fragment_without_its_start_is_discarded_rather_than_half_delivered() {
        let mut depacketiser = H264RtpDepacketiser::new();

        let from_a_middle = depacketiser
            .depacketise(fragmentation_unit(false, false, 5, &[0x03]), 3000, 40)
            .unwrap();
        let from_an_end = depacketiser
            .depacketise(fragmentation_unit(false, true, 5, &[0x04]), 3000, 41)
            .unwrap();

        assert!(from_a_middle.is_empty());
        assert!(from_an_end.is_empty());
    }

    #[test]
    fn a_sequence_gap_still_completes_the_unit_it_can_reach() {
        let mut depacketiser = H264RtpDepacketiser::new();

        depacketiser
            .depacketise(fragmentation_unit(true, false, 5, &[0x01]), 4000, 70)
            .unwrap();
        // 72, not 71: one fragment was lost on the way.
        let nal_units = depacketiser
            .depacketise(fragmentation_unit(false, true, 5, &[0x03]), 4000, 72)
            .unwrap();

        assert_eq!(
            nal_units,
            vec![Bytes::from(vec![IDR_NAL_UNIT_HEADER, 0x01, 0x03])]
        );
    }

    #[test]
    fn two_units_fragmented_at_once_reassemble_independently() {
        let mut depacketiser = H264RtpDepacketiser::new();

        depacketiser
            .depacketise(fragmentation_unit(true, false, 1, &[0xA1]), 5000, 90)
            .unwrap();
        depacketiser
            .depacketise(fragmentation_unit(true, false, 5, &[0xB1]), 6000, 91)
            .unwrap();

        let from_the_second = depacketiser
            .depacketise(fragmentation_unit(false, true, 5, &[0xB2]), 6000, 92)
            .unwrap();
        let from_the_first = depacketiser
            .depacketise(fragmentation_unit(false, true, 1, &[0xA2]), 5000, 93)
            .unwrap();

        assert_eq!(
            from_the_second,
            vec![Bytes::from(vec![IDR_NAL_UNIT_HEADER, 0xB1, 0xB2])]
        );
        assert_eq!(from_the_first, vec![Bytes::from(vec![0x61, 0xA1, 0xA2])]);
    }

    #[test]
    fn a_stale_half_reassembled_unit_is_discarded() {
        let mut depacketiser = H264RtpDepacketiser::new();
        depacketiser
            .depacketise(fragmentation_unit(true, false, 5, &[0x01]), 1_000, 10)
            .unwrap();

        depacketiser.discard_stale_fragments(1_000 + STALE_FRAGMENT_THRESHOLD_TICKS);

        let after_the_discard = depacketiser
            .depacketise(fragmentation_unit(false, true, 5, &[0x02]), 1_000, 11)
            .unwrap();
        assert!(after_the_discard.is_empty());
    }

    #[test]
    fn a_stap_a_yields_every_nal_unit_it_aggregates() {
        let mut depacketiser = H264RtpDepacketiser::new();
        let sequence_parameter_set: &[u8] = &[0x67, 0x42, 0xE0, 0x1F];
        let picture_parameter_set: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];

        let nal_units = depacketiser
            .depacketise(
                stap_a(&[sequence_parameter_set, picture_parameter_set]),
                7000,
                1,
            )
            .unwrap();

        assert_eq!(
            nal_units,
            vec![
                Bytes::from(sequence_parameter_set.to_vec()),
                Bytes::from(picture_parameter_set.to_vec()),
            ]
        );
    }

    #[test]
    fn a_stap_a_whose_length_prefix_overruns_the_packet_is_refused() {
        let mut depacketiser = H264RtpDepacketiser::new();
        let truncated = Bytes::from(vec![STAP_A_HEADER, 0x00, 0x10, 0x67, 0x42]);

        let refusal = depacketiser.depacketise(truncated, 8000, 1).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedRtpPayload { .. }
        ));
    }

    #[test]
    fn a_stap_a_declaring_a_zero_length_nal_unit_is_refused() {
        let mut depacketiser = H264RtpDepacketiser::new();
        let empty_unit = Bytes::from(vec![STAP_A_HEADER, 0x00, 0x00, 0x67]);

        let refusal = depacketiser.depacketise(empty_unit, 8100, 1).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedRtpPayload { .. }
        ));
    }

    #[test]
    fn an_empty_payload_is_refused_rather_than_read_past() {
        let mut depacketiser = H264RtpDepacketiser::new();

        let refusal = depacketiser.depacketise(Bytes::new(), 9000, 1).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedRtpPayload { .. }
        ));
    }

    #[test]
    fn a_fragmentation_unit_with_no_payload_byte_is_refused() {
        let mut depacketiser = H264RtpDepacketiser::new();
        let header_only = Bytes::from(vec![FU_A_INDICATOR, 0x85]);

        let refusal = depacketiser.depacketise(header_only, 9100, 1).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedRtpPayload { .. }
        ));
    }
}
