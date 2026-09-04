// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What an Opus packet's own TOC byte says about itself.
//!
//! RTP carries no sample count and no channel count — the rtpmap's encoding
//! parameter is fixed at 2 for every Opus stream by RFC 7587 §7, mono included
//! — so the packet is the only honest source for either.

use crate::error::{Result, WebRtcExtensionError};

/// Opus always codes at 48 kHz on the wire, whatever the source rate was.
pub(crate) const OPUS_WIRE_SAMPLE_RATE_HZ: u32 = 48_000;

/// RFC 6716 §3.1 caps one packet at 120 ms, which is 5 760 samples per channel
/// at the wire rate.
const HIGHEST_PER_CHANNEL_SAMPLES_IN_ONE_PACKET: u32 = 5_760;

/// What one Opus packet declares about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpusPacketDescription {
    /// Per-channel samples at 48 kHz across every frame the packet carries.
    pub sample_count: u32,
    /// 1 or 2, from the TOC's stereo bit.
    pub channels: u32,
}

/// Read the TOC byte, and the frame-count byte when there is one.
pub(crate) fn describe_opus_packet(packet: &[u8]) -> Result<OpusPacketDescription> {
    let Some(&table_of_contents) = packet.first() else {
        return Err(WebRtcExtensionError::MalformedOpusPacket {
            what: "an empty packet carries no TOC byte".to_owned(),
        });
    };

    let configuration = table_of_contents >> 3;
    let channels = if table_of_contents & 0x04 == 0 { 1 } else { 2 };
    let frame_count = match table_of_contents & 0x03 {
        0 => 1,
        1 | 2 => 2,
        // Code 3 spends a second byte on the count: six bits of it, with the
        // top two given over to VBR and padding flags.
        _ => {
            let Some(&frame_count_byte) = packet.get(1) else {
                return Err(WebRtcExtensionError::MalformedOpusPacket {
                    what: "a code-3 packet ends before its frame-count byte".to_owned(),
                });
            };
            let frame_count = u32::from(frame_count_byte & 0x3F);
            if frame_count == 0 {
                return Err(WebRtcExtensionError::MalformedOpusPacket {
                    what: "a code-3 packet declaring zero frames".to_owned(),
                });
            }
            frame_count
        }
    };

    let sample_count = samples_per_frame_at_the_wire_rate(configuration) * frame_count;
    if sample_count > HIGHEST_PER_CHANNEL_SAMPLES_IN_ONE_PACKET {
        return Err(WebRtcExtensionError::MalformedOpusPacket {
            what: format!(
                "a packet spanning {sample_count} samples, past the {HIGHEST_PER_CHANNEL_SAMPLES_IN_ONE_PACKET} \
                 RFC 6716 §3.1 allows"
            ),
        });
    }

    Ok(OpusPacketDescription {
        sample_count,
        channels,
    })
}

/// RFC 6716 §3.1's configuration table, in samples at 48 kHz: SILK below 12,
/// hybrid below 16, CELT above.
fn samples_per_frame_at_the_wire_rate(configuration: u8) -> u32 {
    match configuration {
        0..=11 => [480, 960, 1920, 2880][(configuration % 4) as usize],
        12..=15 => [480, 960][(configuration % 2) as usize],
        _ => [120, 240, 480, 960][(configuration % 4) as usize],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_of_contents(configuration: u8, stereo: bool, frame_count_code: u8) -> u8 {
        (configuration << 3) | (u8::from(stereo) << 2) | frame_count_code
    }

    #[test]
    fn the_twenty_millisecond_silk_frame_webrtc_sends_is_960_samples() {
        let packet = [table_of_contents(1, true, 0), 0x00];

        let described = describe_opus_packet(&packet).unwrap();

        assert_eq!(described.sample_count, 960);
        assert_eq!(described.channels, 2);
    }

    #[test]
    fn the_stereo_bit_and_not_the_rtpmap_is_what_says_mono() {
        let mono = describe_opus_packet(&[table_of_contents(1, false, 0), 0x00]).unwrap();
        let stereo = describe_opus_packet(&[table_of_contents(1, true, 0), 0x00]).unwrap();

        assert_eq!(mono.channels, 1);
        assert_eq!(stereo.channels, 2);
    }

    #[test]
    fn every_configuration_class_reports_its_own_frame_length() {
        // SILK 10/20/40/60 ms, hybrid 10/20 ms, CELT 2.5/5/10/20 ms.
        let cases = [
            (0u8, 480u32),
            (1, 960),
            (2, 1920),
            (3, 2880),
            (12, 480),
            (13, 960),
            (16, 120),
            (17, 240),
            (18, 480),
            (19, 960),
            (31, 960),
        ];
        for (configuration, expected_samples) in cases {
            let packet = [table_of_contents(configuration, false, 0), 0x00];
            assert_eq!(
                describe_opus_packet(&packet).unwrap().sample_count,
                expected_samples,
                "configuration {configuration}"
            );
        }
    }

    #[test]
    fn a_two_frame_packet_spans_both_frames() {
        for frame_count_code in [1u8, 2] {
            let packet = [table_of_contents(1, false, frame_count_code), 0x00];
            assert_eq!(describe_opus_packet(&packet).unwrap().sample_count, 1920);
        }
    }

    #[test]
    fn a_code_three_packet_takes_its_frame_count_from_the_second_byte() {
        // Six 20 ms SILK frames — 120 ms, the longest a packet may be.
        let packet = [table_of_contents(1, false, 3), 6, 0x00];

        assert_eq!(describe_opus_packet(&packet).unwrap().sample_count, 5760);
    }

    #[test]
    fn the_vbr_and_padding_flags_are_not_read_as_frame_count() {
        let packet = [table_of_contents(1, false, 3), 0xC0 | 2, 0x00];

        assert_eq!(describe_opus_packet(&packet).unwrap().sample_count, 1920);
    }

    #[test]
    fn a_packet_longer_than_the_spec_allows_is_refused_rather_than_believed() {
        // Seven 20 ms frames is 140 ms.
        let packet = [table_of_contents(1, false, 3), 7, 0x00];

        let refusal = describe_opus_packet(&packet).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedOpusPacket { .. }
        ));
    }

    #[test]
    fn a_code_three_packet_declaring_no_frames_is_refused() {
        let refusal = describe_opus_packet(&[table_of_contents(1, false, 3), 0]).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedOpusPacket { .. }
        ));
    }

    #[test]
    fn a_code_three_packet_missing_its_frame_count_byte_is_refused() {
        let refusal = describe_opus_packet(&[table_of_contents(1, false, 3)]).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedOpusPacket { .. }
        ));
    }

    #[test]
    fn an_empty_packet_is_refused_rather_than_read_past() {
        let refusal = describe_opus_packet(&[]).unwrap_err();

        assert!(matches!(
            refusal,
            WebRtcExtensionError::MalformedOpusPacket { .. }
        ));
    }
}
