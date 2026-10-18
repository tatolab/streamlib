// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The media frame an encoder's port carries: hang's legacy container, a QUIC
//! varint timestamp in microseconds followed by the codec's own payload
//! (`hang::container::Frame`), which every moq-dev player reads as is.
//!
//! The varint duplicates the envelope's stamp by construction. Locally the
//! envelope stamp is authoritative; the varint is what a player off the
//! machine reads.

use hang::moq_net::VarInt;

/// One media frame read off a port, borrowing the bytes it was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HangLegacyContainerMediaFrame<'payload> {
    /// The presentation timestamp the producer wrote, in microseconds.
    pub timestamp_microseconds: u64,
    /// The codec payload after the varint — an Annex-B access unit or an Opus
    /// packet.
    pub codec_payload_bytes: &'payload [u8],
}

/// Why a timestamp cannot be written as a media frame's varint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaFrameTimestampOutsideTheVarintRange {
    /// The stamp that was refused.
    pub timestamp_ns: i64,
}

impl std::error::Error for MediaFrameTimestampOutsideTheVarintRange {}

impl std::fmt::Display for MediaFrameTimestampOutsideTheVarintRange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "a media frame stamped {} ns cannot be written: the hang container's timestamp is \
             an unsigned QUIC varint in microseconds, below 2^62",
            self.timestamp_ns
        )
    }
}

/// Why bytes read off a port are not a media frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageIsNotAHangLegacyContainerMediaFrame {
    /// How many bytes the message held.
    pub message_bytes: usize,
}

impl std::error::Error for MessageIsNotAHangLegacyContainerMediaFrame {}

impl std::fmt::Display for MessageIsNotAHangLegacyContainerMediaFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "a {}-byte message is not a media frame: it ends inside the QUIC varint timestamp \
             the hang container opens with",
            self.message_bytes
        )
    }
}

/// Write `codec_payload_bytes` behind the varint timestamp `timestamp_ns`
/// truncates to, as one media frame.
pub fn hang_legacy_container_media_frame_bytes(
    timestamp_ns: i64,
    codec_payload_bytes: &[u8],
) -> Result<Vec<u8>, MediaFrameTimestampOutsideTheVarintRange> {
    let timestamp_varint = u64::try_from(timestamp_ns)
        .ok()
        .and_then(|timestamp_ns| VarInt::from_u64(timestamp_ns / 1_000))
        .ok_or(MediaFrameTimestampOutsideTheVarintRange { timestamp_ns })?;
    let mut media_frame_bytes = Vec::with_capacity(8 + codec_payload_bytes.len());
    timestamp_varint
        .encode_quic(&mut media_frame_bytes)
        .map_err(|_| MediaFrameTimestampOutsideTheVarintRange { timestamp_ns })?;
    media_frame_bytes.extend_from_slice(codec_payload_bytes);
    Ok(media_frame_bytes)
}

/// Read one media frame, borrowing its codec payload from `message_bytes`.
pub fn read_hang_legacy_container_media_frame(
    message_bytes: &[u8],
) -> Result<HangLegacyContainerMediaFrame<'_>, MessageIsNotAHangLegacyContainerMediaFrame> {
    let mut unread_bytes = message_bytes;
    let timestamp_varint = VarInt::decode_quic(&mut unread_bytes).map_err(|_| {
        MessageIsNotAHangLegacyContainerMediaFrame {
            message_bytes: message_bytes.len(),
        }
    })?;
    Ok(HangLegacyContainerMediaFrame {
        timestamp_microseconds: timestamp_varint.into_inner(),
        codec_payload_bytes: unread_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one claim this module makes is that hang reads what it writes, so
    /// the proof is hang's own decoder, not this module's reader.
    #[test]
    fn hangs_own_frame_decoder_reads_the_written_media_frame() {
        let access_unit = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84];
        let media_frame_bytes =
            hang_legacy_container_media_frame_bytes(1_234_567_891_000, &access_unit)
                .expect("a stamp inside the varint range");

        let decoded_by_hang =
            hang::container::Frame::decode(&media_frame_bytes[..]).expect("hang decodes the frame");
        assert_eq!(decoded_by_hang.timestamp.as_micros(), 1_234_567_891);
        assert_eq!(&decoded_by_hang.payload[..], &access_unit[..]);
    }

    #[test]
    fn the_varint_takes_the_quic_width_its_value_needs() {
        assert_eq!(
            hang_legacy_container_media_frame_bytes(63_000, b"p").unwrap(),
            [63, b'p']
        );
        assert_eq!(
            hang_legacy_container_media_frame_bytes(64_000, b"p").unwrap(),
            [0x40, 64, b'p']
        );
        assert_eq!(
            hang_legacy_container_media_frame_bytes(2_000_000_000, b"p")
                .unwrap()
                .len(),
            1 + 4
        );
    }

    #[test]
    fn the_reader_hands_back_the_stamp_and_borrows_the_payload_after_the_varint() {
        let media_frame_bytes =
            hang_legacy_container_media_frame_bytes(33_366_000, b"opus packet").unwrap();
        let media_frame = read_hang_legacy_container_media_frame(&media_frame_bytes).unwrap();
        assert_eq!(media_frame.timestamp_microseconds, 33_366);
        assert_eq!(media_frame.codec_payload_bytes, b"opus packet");
    }

    #[test]
    fn a_negative_stamp_is_refused_rather_than_wrapped() {
        assert_eq!(
            hang_legacy_container_media_frame_bytes(-1, b"p"),
            Err(MediaFrameTimestampOutsideTheVarintRange { timestamp_ns: -1 })
        );
    }

    #[test]
    fn a_message_ending_inside_the_varint_is_refused_naming_its_length() {
        assert_eq!(
            read_hang_legacy_container_media_frame(&[0x80, 0x00]),
            Err(MessageIsNotAHangLegacyContainerMediaFrame { message_bytes: 2 })
        );
        assert_eq!(
            read_hang_legacy_container_media_frame(&[]),
            Err(MessageIsNotAHangLegacyContainerMediaFrame { message_bytes: 0 })
        );
    }
}
