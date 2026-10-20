// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Opus identification header (`OpusHead`, RFC 7845 §5.1) a WHEP player's
//! audio catalog entry carries hex-encoded in `description`, so a decoder
//! learns the channel count before the first packet.
//!
//! RTP carries mono and stereo Opus only (RFC 7587), so the header is always
//! channel mapping family 0, and RTP states no encoder lookahead, so its
//! pre-skip is 0.

use std::fmt::Write;

use crate::opus_packet::{OPUS_WIRE_SAMPLE_RATE_HZ, OpusRtpChannelLayout};

/// Magic, version, channel count, pre-skip, input rate, output gain, family.
const OPUS_HEAD_FAMILY_ZERO_LENGTH_BYTES: usize = 19;
const OPUS_HEAD_MAGIC_SIGNATURE: &[u8; 8] = b"OpusHead";
const OPUS_HEAD_VERSION: u8 = 1;
const PRE_SKIP_RTP_CARRIES_NONE: u16 = 0;
const OUTPUT_GAIN_UNCHANGED: i16 = 0;
const CHANNEL_MAPPING_FAMILY_MONO_OR_STEREO: u8 = 0;

/// The hex-encoded `OpusHead` of an RTP Opus stream laid out as `channel_layout`.
pub(crate) fn hex_encoded_opus_identification_header_for_rtp_stream(
    channel_layout: OpusRtpChannelLayout,
) -> String {
    let mut header = Vec::with_capacity(OPUS_HEAD_FAMILY_ZERO_LENGTH_BYTES);
    header.extend_from_slice(OPUS_HEAD_MAGIC_SIGNATURE);
    header.push(OPUS_HEAD_VERSION);
    header.push(channel_layout.channel_count());
    header.extend_from_slice(&PRE_SKIP_RTP_CARRIES_NONE.to_le_bytes());
    header.extend_from_slice(&OPUS_WIRE_SAMPLE_RATE_HZ.to_le_bytes());
    header.extend_from_slice(&OUTPUT_GAIN_UNCHANGED.to_le_bytes());
    header.push(CHANNEL_MAPPING_FAMILY_MONO_OR_STEREO);
    header
        .iter()
        .fold(String::with_capacity(header.len() * 2), |mut hex, byte| {
            // Writing to a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stereo_header_lays_out_rfc_7845s_nineteen_fixed_bytes() {
        let expected: [u8; OPUS_HEAD_FAMILY_ZERO_LENGTH_BYTES] = [
            b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', // magic
            1,    // version
            2,    // channels
            0, 0, // pre-skip, little-endian
            0x80, 0xBB, 0, 0, // 48 000 Hz, little-endian
            0, 0, // output gain
            0, // mapping family
        ];
        let expected_hex: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();

        assert_eq!(
            hex_encoded_opus_identification_header_for_rtp_stream(OpusRtpChannelLayout::Stereo),
            expected_hex
        );
    }

    #[test]
    fn a_mono_header_differs_only_in_its_channel_count() {
        let mono =
            hex_encoded_opus_identification_header_for_rtp_stream(OpusRtpChannelLayout::Mono);
        let stereo =
            hex_encoded_opus_identification_header_for_rtp_stream(OpusRtpChannelLayout::Stereo);

        assert_eq!(&mono[18..20], "01");
        assert_eq!(mono[..18], stereo[..18]);
        assert_eq!(mono[20..], stereo[20..]);
    }
}
