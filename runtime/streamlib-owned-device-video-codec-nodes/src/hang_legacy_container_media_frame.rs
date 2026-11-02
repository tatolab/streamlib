// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The media frame `encoded_video` carries: hang's legacy container, a QUIC
//! varint timestamp in microseconds (RFC 9000 §16) followed by the Annex-B
//! access unit.

/// The largest value a QUIC varint carries, 2^62 - 1.
const QUIC_VARINT_LARGEST_VALUE: u64 = (1 << 62) - 1;

/// Write `annex_b_access_unit` behind the varint `timestamp_ns` truncates to
/// into `media_frame_bytes`, replacing what it held. `None` when the stamp is
/// negative or past the varint's range.
pub fn write_hang_legacy_container_media_frame(
    media_frame_bytes: &mut Vec<u8>,
    timestamp_ns: i64,
    annex_b_access_unit: &[u8],
) -> Option<()> {
    let timestamp_microseconds = u64::try_from(timestamp_ns)
        .ok()
        .map(|timestamp_ns| timestamp_ns / 1_000)
        .filter(|&microseconds| microseconds <= QUIC_VARINT_LARGEST_VALUE)?;
    let (varint_bytes, width_prefix): (usize, u64) = match timestamp_microseconds {
        0..=0x3F => (1, 0b00),
        0x40..=0x3FFF => (2, 0b01),
        0x4000..=0x3FFF_FFFF => (4, 0b10),
        _ => (8, 0b11),
    };
    let prefixed = timestamp_microseconds | (width_prefix << (varint_bytes * 8 - 2));
    media_frame_bytes.clear();
    media_frame_bytes.reserve(varint_bytes + annex_b_access_unit.len());
    media_frame_bytes.extend_from_slice(&prefixed.to_be_bytes()[8 - varint_bytes..]);
    media_frame_bytes.extend_from_slice(annex_b_access_unit);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9000 Appendix A.1's 1-, 2- and 4-byte sample encodings and the
    /// values they carry; its 8-byte sample is past any nanosecond stamp.
    const RFC_9000_SAMPLE_VARINTS: [(&[u8], u64); 3] = [
        (&[0x9d, 0x7f, 0x3e, 0x7d], 494_878_333),
        (&[0x7b, 0xbd], 15_293),
        (&[0x25], 37),
    ];

    #[test]
    fn the_rfc_9000_sample_values_are_written_as_their_encodings() {
        for (encoded, microseconds) in RFC_9000_SAMPLE_VARINTS {
            let mut media_frame = Vec::new();
            write_hang_legacy_container_media_frame(
                &mut media_frame,
                (microseconds * 1_000 + 999) as i64,
                &[0xAB],
            )
            .unwrap();
            assert_eq!(&media_frame[..encoded.len()], encoded);
            assert_eq!(&media_frame[encoded.len()..], [0xAB]);
        }
    }

    #[test]
    fn a_stamp_past_four_bytes_takes_the_eight_byte_width() {
        let mut media_frame = Vec::new();
        write_hang_legacy_container_media_frame(&mut media_frame, 0x4000_0000 * 1_000, &[])
            .unwrap();
        assert_eq!(media_frame, [0xC0, 0, 0, 0, 0x40, 0, 0, 0]);
    }

    #[test]
    fn a_negative_stamp_is_refused() {
        let mut media_frame = vec![1, 2, 3];
        assert_eq!(
            write_hang_legacy_container_media_frame(&mut media_frame, -1, &[0xAB]),
            None
        );
    }
}
