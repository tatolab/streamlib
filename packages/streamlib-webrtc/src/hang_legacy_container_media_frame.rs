// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The media frame an encoded port carries: hang's legacy container, a QUIC
//! varint timestamp in microseconds (RFC 9000 §16) followed by the codec's own
//! payload — an Annex-B access unit or an Opus packet.
//!
//! Spelled here rather than imported: an extension links no engine crate, and
//! this is the layout every moq-dev player and the engine's codec built-ins
//! read and write.

use crate::error::{Result, WebRtcExtensionError};

/// The largest value a QUIC varint carries, 2^62 - 1.
const QUIC_VARINT_LARGEST_VALUE: u64 = (1 << 62) - 1;

/// One media frame read off a port, borrowing the bytes it was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HangLegacyContainerMediaFrame<'payload> {
    /// The presentation timestamp the producer wrote, in microseconds.
    pub timestamp_microseconds: u64,
    /// The codec payload after the varint.
    pub codec_payload_bytes: &'payload [u8],
}

/// Read one media frame, borrowing its codec payload from `message_bytes`.
pub(crate) fn read_hang_legacy_container_media_frame(
    message_bytes: &[u8],
) -> Result<HangLegacyContainerMediaFrame<'_>> {
    let not_a_media_frame = || WebRtcExtensionError::Refused {
        what: format!(
            "a {}-byte message is not a media frame: it ends inside the QUIC varint timestamp \
             the hang container opens with",
            message_bytes.len()
        ),
    };
    let first_byte = *message_bytes.first().ok_or_else(not_a_media_frame)?;
    // The top two bits of the first byte give the encoding's width: 1, 2, 4 or 8 bytes.
    let varint_bytes = 1usize << (first_byte >> 6);
    let (encoded_varint, codec_payload_bytes) = message_bytes
        .split_at_checked(varint_bytes)
        .ok_or_else(not_a_media_frame)?;
    let timestamp_microseconds = encoded_varint
        .iter()
        .skip(1)
        .fold(u64::from(first_byte & 0x3F), |value, &byte| {
            (value << 8) | u64::from(byte)
        });
    Ok(HangLegacyContainerMediaFrame {
        timestamp_microseconds,
        codec_payload_bytes,
    })
}

/// `codec_payload_bytes` behind the varint `timestamp_ns` truncates to, as one
/// media frame.
pub(crate) fn hang_legacy_container_media_frame_bytes(
    timestamp_ns: i64,
    codec_payload_bytes: &[u8],
) -> Result<Vec<u8>> {
    let timestamp_microseconds = u64::try_from(timestamp_ns)
        .ok()
        .map(|timestamp_ns| timestamp_ns / 1_000)
        .filter(|&microseconds| microseconds <= QUIC_VARINT_LARGEST_VALUE)
        .ok_or_else(|| WebRtcExtensionError::Refused {
            what: format!(
                "a media frame stamped {timestamp_ns} ns cannot be written: the hang container's \
                 timestamp is an unsigned QUIC varint in microseconds, below 2^62"
            ),
        })?;
    let (varint_bytes, width_prefix): (usize, u64) = match timestamp_microseconds {
        0..=0x3F => (1, 0b00),
        0x40..=0x3FFF => (2, 0b01),
        0x4000..=0x3FFF_FFFF => (4, 0b10),
        _ => (8, 0b11),
    };
    let prefixed = timestamp_microseconds | (width_prefix << (varint_bytes * 8 - 2));
    let mut media_frame = Vec::with_capacity(varint_bytes + codec_payload_bytes.len());
    media_frame.extend_from_slice(&prefixed.to_be_bytes()[8 - varint_bytes..]);
    media_frame.extend_from_slice(codec_payload_bytes);
    Ok(media_frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9000 Appendix A.1's sample encodings, each followed by one payload byte.
    const RFC_9000_SAMPLE_VARINTS: [(&[u8], u64); 4] = [
        (
            &[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c],
            151_288_809_941_952_652,
        ),
        (&[0x9d, 0x7f, 0x3e, 0x7d], 494_878_333),
        (&[0x7b, 0xbd], 15_293),
        (&[0x25], 37),
    ];

    #[test]
    fn the_rfc_9000_sample_varints_read_as_their_values() {
        for (encoded, value) in RFC_9000_SAMPLE_VARINTS {
            let mut message = encoded.to_vec();
            message.push(0xAB);

            let media_frame = read_hang_legacy_container_media_frame(&message).unwrap();

            assert_eq!(media_frame.timestamp_microseconds, value);
            assert_eq!(media_frame.codec_payload_bytes, [0xAB]);
        }
    }

    #[test]
    fn the_rfc_9000_sample_values_a_nanosecond_stamp_reaches_are_written_as_their_encodings() {
        for (encoded, value) in RFC_9000_SAMPLE_VARINTS {
            let Some(timestamp_ns) = value
                .checked_mul(1_000)
                .and_then(|timestamp_ns| i64::try_from(timestamp_ns).ok())
            else {
                continue;
            };

            let media_frame = hang_legacy_container_media_frame_bytes(timestamp_ns, &[0xAB]);

            assert_eq!(media_frame.unwrap(), [encoded, &[0xAB]].concat());
        }
    }

    #[test]
    fn a_stamp_past_thirty_bits_of_microseconds_takes_the_eight_byte_encoding() {
        let media_frame =
            hang_legacy_container_media_frame_bytes(0x4000_0000 * 1_000, &[0xAB]).unwrap();

        assert_eq!(
            media_frame,
            [0xC0, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0xAB]
        );
    }

    #[test]
    fn a_written_media_frame_reads_back_to_its_microseconds_and_payload() {
        let access_unit = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84];

        let media_frame =
            hang_legacy_container_media_frame_bytes(1_234_567_891_999, &access_unit).unwrap();
        let read = read_hang_legacy_container_media_frame(&media_frame).unwrap();

        assert_eq!(read.timestamp_microseconds, 1_234_567_891);
        assert_eq!(read.codec_payload_bytes, access_unit);
    }

    #[test]
    fn a_message_ending_inside_its_varint_is_refused() {
        for truncated in [&[][..], &[0x40][..], &[0x80, 0x00, 0x00][..]] {
            assert!(matches!(
                read_hang_legacy_container_media_frame(truncated),
                Err(WebRtcExtensionError::Refused { .. })
            ));
        }
    }

    #[test]
    fn a_negative_stamp_is_refused_rather_than_wrapped() {
        assert!(matches!(
            hang_legacy_container_media_frame_bytes(-1_000, b"p"),
            Err(WebRtcExtensionError::Refused { .. })
        ));
    }
}
