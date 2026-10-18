// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Opus identification header (`OpusHead`, RFC 7845 §5.1): what an Opus
//! port's catalog entry carries hex-encoded in `description`, so a decoder
//! learns the channel count and the encoder's lookahead before the first
//! packet.
//!
//! Written for the layouts [`OpusStreamLayoutForSourceChannelCount`] resolves
//! — family 0 for one and two channels, family 1 in Vorbis order for three to
//! eight — and read for the two fields a decoder needs from it.

use std::fmt::Write;

use crate::audio_window_to_encoded_packet_encoder::OPUS_SAMPLE_RATE_HZ;
use crate::opus_stream_layout::{OpusStreamLayoutForSourceChannelCount, OpusStreamLayoutRefusal};

const OPUS_HEAD_MAGIC_SIGNATURE: &[u8; 8] = b"OpusHead";
const OPUS_HEAD_VERSION: u8 = 1;
/// Magic, version, channel count, pre-skip, input rate, output gain, family.
const OPUS_HEAD_FIXED_FIELDS_BYTES: usize = 19;

/// What a decoder reads from an `OpusHead`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusIdentificationHeader {
    /// Output channel count.
    pub channels: u32,
    /// Samples at 48 kHz to discard from the decoder's output at stream start.
    pub pre_skip: u32,
}

/// Why an encoder's `OpusHead` cannot be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusIdentificationHeaderCannotBeWritten {
    /// The channel count resolves to no layout Opus places.
    ChannelLayout(OpusStreamLayoutRefusal),
    /// The header carries pre-skip in sixteen bits.
    PreSkipPastSixteenBits { pre_skip: u32 },
}

impl From<OpusStreamLayoutRefusal> for OpusIdentificationHeaderCannotBeWritten {
    fn from(refusal: OpusStreamLayoutRefusal) -> Self {
        Self::ChannelLayout(refusal)
    }
}

impl std::error::Error for OpusIdentificationHeaderCannotBeWritten {}

impl std::fmt::Display for OpusIdentificationHeaderCannotBeWritten {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChannelLayout(refusal) => refusal.fmt(formatter),
            Self::PreSkipPastSixteenBits { pre_skip } => write!(
                formatter,
                "a pre-skip of {pre_skip} samples does not fit the OpusHead's sixteen-bit field"
            ),
        }
    }
}

/// Why bytes are not an `OpusHead` this decoder reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpusIdentificationHeaderRefusal {
    /// Shorter than the fixed fields, or than the mapping table it declares.
    Truncated { header_bytes: usize },
    /// Not opening with `OpusHead`.
    MissingMagicSignature,
    /// A major version past 0; RFC 7845 §5.1 says a reader refuses it.
    UnsupportedVersion { version: u8 },
    /// A channel layout other than the one this decoder mints for the count:
    /// family 0 for one or two channels, family 1 in Vorbis order for three to
    /// eight.
    LayoutThisDecoderDoesNotMint {
        channels: u8,
        channel_mapping_family: u8,
    },
    /// Not hex.
    NotHex,
}

impl std::error::Error for OpusIdentificationHeaderRefusal {}

impl std::fmt::Display for OpusIdentificationHeaderRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { header_bytes } => write!(
                formatter,
                "a {header_bytes}-byte OpusHead ends before the fields it declares"
            ),
            Self::MissingMagicSignature => {
                write!(formatter, "the description does not open with `OpusHead`")
            }
            Self::UnsupportedVersion { version } => write!(
                formatter,
                "OpusHead version {version} is past the major version RFC 7845 defines"
            ),
            Self::NotHex => write!(formatter, "the description is not hex-encoded"),
            Self::LayoutThisDecoderDoesNotMint {
                channels,
                channel_mapping_family,
            } => write!(
                formatter,
                "the OpusHead lays {channels} channels out under mapping family \
                 {channel_mapping_family} with a stream layout other than the one this decoder \
                 mints — family 0 for one or two channels, family 1 in Vorbis order for three \
                 to eight"
            ),
        }
    }
}

impl OpusIdentificationHeader {
    /// The `OpusHead` an encoder of `channels` with this lookahead emits,
    /// hex-encoded as a catalog entry's `description`.
    pub fn hex_encoded_for_encoder(
        channels: u32,
        pre_skip: u32,
    ) -> Result<String, OpusIdentificationHeaderCannotBeWritten> {
        let layout = OpusStreamLayoutForSourceChannelCount::resolve(channels)?;
        let pre_skip = u16::try_from(pre_skip).map_err(|_| {
            OpusIdentificationHeaderCannotBeWritten::PreSkipPastSixteenBits { pre_skip }
        })?;
        let mut header = Vec::with_capacity(OPUS_HEAD_FIXED_FIELDS_BYTES + 2 + channels as usize);
        header.extend_from_slice(OPUS_HEAD_MAGIC_SIGNATURE);
        header.push(OPUS_HEAD_VERSION);
        header.push(channels as u8);
        header.extend_from_slice(&pre_skip.to_le_bytes());
        header.extend_from_slice(&OPUS_SAMPLE_RATE_HZ.to_le_bytes());
        header.extend_from_slice(&0i16.to_le_bytes());
        header.push(layout.channel_mapping_family());
        if let OpusStreamLayoutForSourceChannelCount::MappingFamilyOneMultistream(multistream) =
            layout
        {
            header.push(multistream.streams);
            header.push(multistream.coupled_streams);
            header.extend_from_slice(multistream.vorbis_channel_order_mapping);
        }
        let mut hex_encoded_header = String::with_capacity(header.len() * 2);
        for byte in header {
            // Writing to a `String` cannot fail.
            let _ = write!(hex_encoded_header, "{byte:02x}");
        }
        Ok(hex_encoded_header)
    }

    /// Read the channel count and pre-skip from a hex-encoded `OpusHead`.
    pub fn parse_hex_encoded(
        hex_encoded_header: &str,
    ) -> Result<Self, OpusIdentificationHeaderRefusal> {
        let header = decode_hex(hex_encoded_header)?;
        if header.len() < OPUS_HEAD_FIXED_FIELDS_BYTES {
            return Err(OpusIdentificationHeaderRefusal::Truncated {
                header_bytes: header.len(),
            });
        }
        if &header[..8] != OPUS_HEAD_MAGIC_SIGNATURE {
            return Err(OpusIdentificationHeaderRefusal::MissingMagicSignature);
        }
        // The upper four bits are the major version; a reader accepts any
        // minor version of major 0.
        if header[8] & 0xF0 != 0 {
            return Err(OpusIdentificationHeaderRefusal::UnsupportedVersion { version: header[8] });
        }
        let channels = header[9];
        let channel_mapping_family = header[18];
        if channel_mapping_family != 0
            && header.len() < OPUS_HEAD_FIXED_FIELDS_BYTES + 2 + usize::from(channels)
        {
            return Err(OpusIdentificationHeaderRefusal::Truncated {
                header_bytes: header.len(),
            });
        }
        let layout_this_decoder_mints =
            OpusStreamLayoutForSourceChannelCount::resolve(u32::from(channels)).ok();
        let declared_layout_is_the_one_minted = match layout_this_decoder_mints {
            Some(OpusStreamLayoutForSourceChannelCount::MappingFamilyZeroSingleStream {
                ..
            }) => channel_mapping_family == 0,
            Some(OpusStreamLayoutForSourceChannelCount::MappingFamilyOneMultistream(
                multistream,
            )) => {
                channel_mapping_family == 1
                    && header[19] == multistream.streams
                    && header[20] == multistream.coupled_streams
                    && header[21..21 + usize::from(channels)]
                        == *multistream.vorbis_channel_order_mapping
            }
            None => false,
        };
        if !declared_layout_is_the_one_minted {
            return Err(
                OpusIdentificationHeaderRefusal::LayoutThisDecoderDoesNotMint {
                    channels,
                    channel_mapping_family,
                },
            );
        }
        Ok(Self {
            channels: u32::from(channels),
            pre_skip: u32::from(u16::from_le_bytes([header[10], header[11]])),
        })
    }
}

fn decode_hex(hex_encoded: &str) -> Result<Vec<u8>, OpusIdentificationHeaderRefusal> {
    if !hex_encoded.len().is_multiple_of(2) {
        return Err(OpusIdentificationHeaderRefusal::NotHex);
    }
    (0..hex_encoded.len())
        .step_by(2)
        .map(|offset| {
            hex_encoded
                .get(offset..offset + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or(OpusIdentificationHeaderRefusal::NotHex)
        })
        .collect()
}

/// The channel count a packet's TOC byte codes — one or two, the stereo flag
/// (RFC 6716 §3.1) — for a stream whose `OpusHead` never arrived.
pub fn channels_coded_by_the_opus_packet_toc(opus_packet_bytes: &[u8]) -> Option<u32> {
    let table_of_contents = *opus_packet_bytes.first()?;
    Some(if table_of_contents & 0b100 != 0 { 2 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7845 §5.1's layout, byte for byte, for a stereo stream.
    #[test]
    fn a_stereo_header_is_the_nineteen_byte_family_zero_layout() {
        let hex_encoded_header = OpusIdentificationHeader::hex_encoded_for_encoder(2, 312).unwrap();
        assert_eq!(
            hex_encoded_header,
            concat!(
                "4f70757348656164", // OpusHead
                "01",               // version
                "02",               // channel count
                "3801",             // pre-skip 312, little-endian
                "80bb0000",         // input sample rate 48 000
                "0000",             // output gain
                "00",               // mapping family 0, no table
            )
        );
    }

    #[test]
    fn a_surround_header_carries_its_vorbis_order_mapping_table() {
        let hex_encoded_header = OpusIdentificationHeader::hex_encoded_for_encoder(6, 312).unwrap();
        assert_eq!(hex_encoded_header.len() / 2, 19 + 2 + 6);
        assert!(
            hex_encoded_header.ends_with(concat!(
                "01",           // mapping family 1
                "04",           // stream count
                "02",           // coupled stream count
                "000401020305", // 5.1 in Vorbis order
            )),
            "{hex_encoded_header}"
        );
    }

    #[test]
    fn every_placeable_count_reads_back_to_its_channels_and_pre_skip() {
        for channels in 1..=8 {
            let header = OpusIdentificationHeader::parse_hex_encoded(
                &OpusIdentificationHeader::hex_encoded_for_encoder(channels, 312).unwrap(),
            )
            .unwrap();
            assert_eq!(
                header,
                OpusIdentificationHeader {
                    channels,
                    pre_skip: 312
                }
            );
        }
    }

    #[test]
    fn a_count_opus_cannot_place_and_a_pre_skip_past_sixteen_bits_have_no_header() {
        assert!(matches!(
            OpusIdentificationHeader::hex_encoded_for_encoder(9, 312),
            Err(OpusIdentificationHeaderCannotBeWritten::ChannelLayout(_))
        ));
        assert_eq!(
            OpusIdentificationHeader::hex_encoded_for_encoder(2, 70_000),
            Err(
                OpusIdentificationHeaderCannotBeWritten::PreSkipPastSixteenBits {
                    pre_skip: 70_000
                }
            )
        );
    }

    #[test]
    fn what_is_not_an_opus_head_is_refused_by_name() {
        assert_eq!(
            OpusIdentificationHeader::parse_hex_encoded("4f70"),
            Err(OpusIdentificationHeaderRefusal::Truncated { header_bytes: 2 })
        );
        assert_eq!(
            OpusIdentificationHeader::parse_hex_encoded(&"00".repeat(19)),
            Err(OpusIdentificationHeaderRefusal::MissingMagicSignature)
        );
        assert_eq!(
            OpusIdentificationHeader::parse_hex_encoded("zz"),
            Err(OpusIdentificationHeaderRefusal::NotHex)
        );
        let mut version_two = OpusIdentificationHeader::hex_encoded_for_encoder(2, 312).unwrap();
        version_two.replace_range(16..18, "20");
        assert_eq!(
            OpusIdentificationHeader::parse_hex_encoded(&version_two),
            Err(OpusIdentificationHeaderRefusal::UnsupportedVersion { version: 0x20 })
        );
    }

    #[test]
    fn the_toc_stereo_flag_names_one_or_two_channels() {
        assert_eq!(
            channels_coded_by_the_opus_packet_toc(&[0b1111_1100]),
            Some(2)
        );
        assert_eq!(
            channels_coded_by_the_opus_packet_toc(&[0b1111_1000]),
            Some(1)
        );
        assert_eq!(channels_coded_by_the_opus_packet_toc(&[]), None);
    }

    /// A foreign producer's four channels as four uncoupled streams decode
    /// wrongly under the layout this decoder mints, so the header is refused
    /// rather than misread.
    #[test]
    fn a_layout_other_than_the_one_minted_is_refused_naming_it() {
        let mut four_uncoupled_streams =
            OpusIdentificationHeader::hex_encoded_for_encoder(4, 312).unwrap();
        // Stream count 4, coupled 0, in place of the Vorbis-order 2 and 2.
        four_uncoupled_streams.replace_range(38..42, "0400");
        assert_eq!(
            OpusIdentificationHeader::parse_hex_encoded(&four_uncoupled_streams),
            Err(
                OpusIdentificationHeaderRefusal::LayoutThisDecoderDoesNotMint {
                    channels: 4,
                    channel_mapping_family: 1
                }
            )
        );

        let mut ambisonics_family =
            OpusIdentificationHeader::hex_encoded_for_encoder(4, 312).unwrap();
        ambisonics_family.replace_range(36..38, "02");
        assert_eq!(
            OpusIdentificationHeader::parse_hex_encoded(&ambisonics_family),
            Err(
                OpusIdentificationHeaderRefusal::LayoutThisDecoderDoesNotMint {
                    channels: 4,
                    channel_mapping_family: 2
                }
            )
        );
    }
}
