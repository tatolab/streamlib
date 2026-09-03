// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How a source channel count resolves to an Opus stream layout, shared by
//! the encoder and the decoder so both mint the same shape from the same
//! count.
//!
//! One and two channels ride mapping family 0 — RFC 7845 §5.1.1.1, a single
//! Opus stream, stereo if and only if the count is two, and the channel
//! mapping table is omitted from the header. Three to eight ride family 1
//! (§5.1.1.2), Vorbis channel order, which is the surround order both MP4's
//! `dOps` and WebRTC accept. Family 255 — arbitrary layouts with no defined
//! channel meaning — is deliberately not offered, so a count above eight is
//! refused rather than muxed into something no player can place.
//!
//! The family-1 stream counts and mapping tables below are libopus's own
//! `vorbis_mappings` (`src/opus_multistream_encoder.c`), because
//! `opus::MSEncoder::new` takes them as constructor arguments — the crate
//! wraps `opus_multistream_encoder_create`, not the `_surround_` variant
//! that would derive them. Getting one wrong produces a stream that decodes
//! to the right count with the speakers permuted, which no assertion on
//! channel count would catch, so the table is locked by a test that builds
//! a real encoder at every count.

/// The most channels Opus mapping family 1 places on named speakers.
pub const HIGHEST_CHANNEL_COUNT_OPUS_PLACES: u32 = 8;

/// One row of libopus's `vorbis_mappings`: the stream layout a family-1
/// channel count is coded as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusMappingFamilyOneStreamLayout {
    /// Total Opus streams the encoder produces per frame.
    pub streams: u8,
    /// How many of those streams are coupled (stereo) pairs. The rest are
    /// mono, so the decoded channel count is `streams + coupled_streams`.
    pub coupled_streams: u8,
    /// Vorbis-order channel mapping table: for each output channel, the
    /// index of the decoded channel that feeds it.
    pub vorbis_channel_order_mapping: &'static [u8],
}

/// The Opus stream layout a source channel count resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusStreamLayoutForSourceChannelCount {
    /// Mapping family 0: one Opus stream, mono or stereo, no mapping table.
    MappingFamilyZeroSingleStream { channels: opus::Channels },
    /// Mapping family 1: several Opus streams in Vorbis channel order.
    MappingFamilyOneMultistream(OpusMappingFamilyOneStreamLayout),
}

/// libopus's `vorbis_mappings`, indexed by channel count minus one.
///
/// Rows one and two are present for completeness and are never reached by
/// [`OpusStreamLayoutForSourceChannelCount::resolve`], which sends those
/// counts to family 0.
const VORBIS_CHANNEL_ORDER_STREAM_LAYOUTS: [OpusMappingFamilyOneStreamLayout;
    HIGHEST_CHANNEL_COUNT_OPUS_PLACES as usize] = [
    // 1: mono
    OpusMappingFamilyOneStreamLayout {
        streams: 1,
        coupled_streams: 0,
        vorbis_channel_order_mapping: &[0],
    },
    // 2: stereo
    OpusMappingFamilyOneStreamLayout {
        streams: 1,
        coupled_streams: 1,
        vorbis_channel_order_mapping: &[0, 1],
    },
    // 3: linear surround
    OpusMappingFamilyOneStreamLayout {
        streams: 2,
        coupled_streams: 1,
        vorbis_channel_order_mapping: &[0, 2, 1],
    },
    // 4: quadraphonic
    OpusMappingFamilyOneStreamLayout {
        streams: 2,
        coupled_streams: 2,
        vorbis_channel_order_mapping: &[0, 1, 2, 3],
    },
    // 5: 5.0 surround
    OpusMappingFamilyOneStreamLayout {
        streams: 3,
        coupled_streams: 2,
        vorbis_channel_order_mapping: &[0, 4, 1, 2, 3],
    },
    // 6: 5.1 surround
    OpusMappingFamilyOneStreamLayout {
        streams: 4,
        coupled_streams: 2,
        vorbis_channel_order_mapping: &[0, 4, 1, 2, 3, 5],
    },
    // 7: 6.1 surround
    OpusMappingFamilyOneStreamLayout {
        streams: 4,
        coupled_streams: 3,
        vorbis_channel_order_mapping: &[0, 4, 1, 2, 3, 5, 6],
    },
    // 8: 7.1 surround
    OpusMappingFamilyOneStreamLayout {
        streams: 5,
        coupled_streams: 3,
        vorbis_channel_order_mapping: &[0, 6, 1, 2, 3, 4, 5, 7],
    },
];

/// Why a channel count resolves to no Opus stream layout.
///
/// Typed rather than a formatted string, so the caller names itself: the same
/// count is refused on the encode side and the decode side, and a refusal that
/// hard-coded "cannot be encoded" would be wrong in one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusStreamLayoutRefusal {
    /// Opus places 1 to 8 channels on named speakers and nothing else.
    ChannelCountOpusCannotPlace { channels: u32 },
}

impl std::error::Error for OpusStreamLayoutRefusal {}

impl std::fmt::Display for OpusStreamLayoutRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpusStreamLayoutRefusal::ChannelCountOpusCannotPlace { channels } => write!(
                formatter,
                "{channels} channels resolve to no Opus stream layout — Opus places 1 to \
                 {HIGHEST_CHANNEL_COUNT_OPUS_PLACES} channels on named speakers (mapping \
                 family 0 for 1 and 2, family 1 in Vorbis order for 3 to \
                 {HIGHEST_CHANNEL_COUNT_OPUS_PLACES}), and the arbitrary-layout family is \
                 deliberately not offered because nothing downstream could place its channels"
            ),
        }
    }
}

impl OpusStreamLayoutForSourceChannelCount {
    /// Resolve a source channel count, refusing by name one Opus cannot
    /// place on named speakers.
    pub fn resolve(source_channels: u32) -> std::result::Result<Self, OpusStreamLayoutRefusal> {
        match source_channels {
            1 => Ok(Self::MappingFamilyZeroSingleStream {
                channels: opus::Channels::Mono,
            }),
            2 => Ok(Self::MappingFamilyZeroSingleStream {
                channels: opus::Channels::Stereo,
            }),
            3..=HIGHEST_CHANNEL_COUNT_OPUS_PLACES => Ok(Self::MappingFamilyOneMultistream(
                VORBIS_CHANNEL_ORDER_STREAM_LAYOUTS[source_channels as usize - 1],
            )),
            channels => Err(OpusStreamLayoutRefusal::ChannelCountOpusCannotPlace { channels }),
        }
    }

    /// The channel mapping family a container writes beside the stream —
    /// `dOps.ChannelMappingFamily`, `OpusHead`'s eighth byte.
    pub fn channel_mapping_family(self) -> u8 {
        match self {
            Self::MappingFamilyZeroSingleStream { .. } => 0,
            Self::MappingFamilyOneMultistream(_) => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_and_two_channels_ride_mapping_family_zero() {
        assert_eq!(
            OpusStreamLayoutForSourceChannelCount::resolve(1).expect("resolves"),
            OpusStreamLayoutForSourceChannelCount::MappingFamilyZeroSingleStream {
                channels: opus::Channels::Mono
            }
        );
        assert_eq!(
            OpusStreamLayoutForSourceChannelCount::resolve(2).expect("resolves"),
            OpusStreamLayoutForSourceChannelCount::MappingFamilyZeroSingleStream {
                channels: opus::Channels::Stereo
            }
        );
        for channels in [1, 2] {
            assert_eq!(
                OpusStreamLayoutForSourceChannelCount::resolve(channels)
                    .expect("resolves")
                    .channel_mapping_family(),
                0
            );
        }
    }

    #[test]
    fn three_to_eight_channels_ride_mapping_family_one_in_vorbis_order() {
        for channels in 3..=HIGHEST_CHANNEL_COUNT_OPUS_PLACES {
            let layout =
                OpusStreamLayoutForSourceChannelCount::resolve(channels).expect("resolves");
            assert_eq!(layout.channel_mapping_family(), 1);
            let OpusStreamLayoutForSourceChannelCount::MappingFamilyOneMultistream(multistream) =
                layout
            else {
                panic!("{channels} channels must resolve to family 1, got {layout:?}");
            };
            assert_eq!(
                multistream.vorbis_channel_order_mapping.len(),
                channels as usize,
                "the mapping table has one entry per output channel"
            );
            assert_eq!(
                u32::from(multistream.streams) + u32::from(multistream.coupled_streams),
                channels,
                "streams + coupled streams is the decoded channel count"
            );
            let decoded_channels =
                u32::from(multistream.streams) + u32::from(multistream.coupled_streams);
            assert!(
                multistream
                    .vorbis_channel_order_mapping
                    .iter()
                    .all(|decoded_index| u32::from(*decoded_index) < decoded_channels),
                "every mapping entry indexes a decoded channel that exists"
            );
            let mut placed: Vec<u8> = multistream.vorbis_channel_order_mapping.to_vec();
            placed.sort_unstable();
            placed.dedup();
            assert_eq!(
                placed.len(),
                channels as usize,
                "no decoded channel feeds two outputs and none is dropped"
            );
        }
    }

    /// A second, independent transcription of libopus's `vorbis_mappings`
    /// (`opus/src/opus_multistream_encoder.c:53-62` in the `opusic-sys`
    /// checkout), written out here rather than derived from the table above.
    ///
    /// This is the only thing that locks the first transcription, and it has
    /// to be a second copy to do that. Every other test in this file passes
    /// with a corrupted table: libopus's own `validate_layout` only
    /// range-checks the mapping entries, the structural test below only reads
    /// length, sum and distinctness, and the round trip cannot see a
    /// permutation because the encoder and the decoder read the same wrong
    /// row, so it cancels. What a slip actually produces is an MP4 whose
    /// `dOps` StreamCount, CoupledCount and ChannelMapping send 5.1 audio to
    /// the wrong speakers in every third-party player — silent here,
    /// obvious there.
    #[test]
    fn the_family_one_table_matches_libopus_vorbis_mappings_transcribed_a_second_time() {
        let libopus_vorbis_mappings: [(u8, u8, &[u8]); 8] = [
            (1, 0, &[0]),
            (1, 1, &[0, 1]),
            (2, 1, &[0, 2, 1]),
            (2, 2, &[0, 1, 2, 3]),
            (3, 2, &[0, 4, 1, 2, 3]),
            (4, 2, &[0, 4, 1, 2, 3, 5]),
            (4, 3, &[0, 4, 1, 2, 3, 5, 6]),
            (5, 3, &[0, 6, 1, 2, 3, 4, 5, 7]),
        ];
        for (channel_count_less_one, (streams, coupled_streams, mapping)) in
            libopus_vorbis_mappings.into_iter().enumerate()
        {
            let ours = VORBIS_CHANNEL_ORDER_STREAM_LAYOUTS[channel_count_less_one];
            assert_eq!(
                (
                    ours.streams,
                    ours.coupled_streams,
                    ours.vorbis_channel_order_mapping
                ),
                (streams, coupled_streams, mapping),
                "row for {} channels drifted from libopus's own table",
                channel_count_less_one + 1
            );
        }
    }

    /// The table is libopus's own. Building the real encoder proves the stream
    /// counts and the mapping are consistent with each other — it does not
    /// prove they are the *right* ones, which is what the transcription test
    /// above is for.
    #[test]
    fn every_family_one_layout_builds_a_real_multistream_encoder_and_decoder() {
        for channels in 3..=HIGHEST_CHANNEL_COUNT_OPUS_PLACES {
            let OpusStreamLayoutForSourceChannelCount::MappingFamilyOneMultistream(multistream) =
                OpusStreamLayoutForSourceChannelCount::resolve(channels).expect("resolves")
            else {
                panic!("{channels} channels must resolve to family 1");
            };
            opus::MSEncoder::new(
                48_000,
                multistream.streams,
                multistream.coupled_streams,
                multistream.vorbis_channel_order_mapping,
                opus::Application::Audio,
            )
            .unwrap_or_else(|failure| {
                panic!("libopus refused the family-1 layout for {channels} channels: {failure}")
            });
            opus::MSDecoder::new(
                48_000,
                multistream.streams,
                multistream.coupled_streams,
                multistream.vorbis_channel_order_mapping,
            )
            .unwrap_or_else(|failure| {
                panic!("libopus refused the family-1 layout for {channels} channels: {failure}")
            });
        }
    }

    #[test]
    fn a_channel_count_opus_cannot_place_is_refused_naming_the_count_and_the_range() {
        for refused in [0, 9, 16] {
            let refusal = OpusStreamLayoutForSourceChannelCount::resolve(refused)
                .expect_err("refused")
                .to_string();
            assert!(
                refusal.contains(&refused.to_string()),
                "the refusal names the count it was handed: {refusal}"
            );
            assert!(
                refusal.contains('8'),
                "the refusal names the range Opus can place: {refusal}"
            );
            assert!(
                !refusal.contains("encoded") && !refusal.contains("decoded"),
                "the refusal is shared by both directions, so it names neither: {refusal}"
            );
        }
    }
}
