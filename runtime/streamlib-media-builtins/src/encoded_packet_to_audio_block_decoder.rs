// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `OpusDecoder` body: one encoded-audio packet in, one audio block out.
//!
//! Separated from the port surface so it is driven directly by a test with
//! no `Runtime` — the shape the video decode body already takes.
//!
//! Two things it does not do, both doctrine rather than omission: it never
//! conceals and it never invents. A `sequence_index` step other than one is
//! loss, and the answer is to reset, re-enter at the next packet — which is
//! every packet, since each one is a sync point — and log what was not seen.
//! No packet-loss concealment, no FEC decode: the gap stays derivable from
//! the stamps either side of it, which is the whole of the drop-at-the-edge
//! and flush-not-interpolate doctrine.
//!
//! Stamps are derived, not copied. The encoder's lookahead means the decoded
//! stream lags its input by `pre_skip` samples, so a decoder that trimmed
//! those and then stamped each block with its own packet's stamp would put
//! every block 6.5 ms later than the audio it holds. Instead the first
//! packet after entry anchors the run and every block's stamp is
//! `anchor + emitted × 1_000_000_000 / 48_000` in integer rational
//! arithmetic — the window stage's own rule, applied on the decode side. The
//! first emitted sample therefore *is* the anchoring packet's stamped
//! instant, and each later block's derived stamp lands on the stamp of the
//! packet whose input it carries.

use streamlib::sdk::error::{Error, Result};

use crate::audio_block::{AudioBlock, AudioSampleDtype};
use crate::audio_window_to_encoded_packet_encoder::OPUS_SAMPLE_RATE_HZ;
use crate::encoded_audio_packet::EncodedAudioPacket;
use crate::encoded_stream_ordering::{ArrivingEncodedBagDisposition, EncodedStreamSyncPointGate};
use crate::opus_stream_layout::OpusStreamLayoutForSourceChannelCount;

/// Registration name, and what every refusal and log line names itself by.
pub const OPUS_DECODER_PROCESSOR_NAME: &str = "OpusDecoder";

/// The most per-channel samples one Opus packet can span: RFC 6716 §2.1.4
/// caps a packet at 120 ms, which is 5 760 samples at Opus's 48 kHz clock.
///
/// This, not the bag's `sample_count`, sizes the decode buffer. `sample_count`
/// is an unvalidated `u32` off the wire that any producer in any language can
/// write, so believing it would let one declare `u32::MAX` and ask for a
/// hundred gigabytes. The packet's own TOC byte is the authority on how long
/// it is, and libopus reports that as the decoded count.
const HIGHEST_PER_CHANNEL_SAMPLES_IN_ONE_OPUS_PACKET: usize = 5_760;

/// A `pre_skip` past this is a producer bug rather than an encoder lookahead.
///
/// Real lookahead at 48 kHz is 312 samples — `Fs/400 + Fs/250` — or 120 at
/// `lowdelay`. The bound is a whole packet rather than 312 so a future libopus
/// cannot fail here for having grown, while still refusing a value that would
/// otherwise trim the stream away silently: an unbounded `pre_skip` makes
/// every decode emit nothing at all, and a decoder that goes quiet without
/// saying why is exactly the concealment this module refuses to do.
const HIGHEST_CREDIBLE_PRE_SKIP_SAMPLES: u32 =
    HIGHEST_PER_CHANNEL_SAMPLES_IN_ONE_OPUS_PACKET as u32;

/// Dispatch one expression over either shape of instance. Same reason the
/// encode body has one: the two libopus wrappers share ctl names but no
/// trait.
macro_rules! on_either_decoder_instance {
    ($instance:expr, |$decoder:ident| $body:expr) => {
        match $instance {
            MintedOpusDecoderInstance::SingleStream($decoder) => $body,
            MintedOpusDecoderInstance::Multistream($decoder) => $body,
        }
    };
}

enum MintedOpusDecoderInstance {
    SingleStream(opus::Decoder),
    Multistream(opus::MSDecoder),
}

impl MintedOpusDecoderInstance {
    fn decode_into_interleaved_f32(
        &mut self,
        opus_packet_bytes: &[u8],
        interleaved_output: &mut [f32],
    ) -> Result<usize> {
        on_either_decoder_instance!(self, |decoder| decoder.decode_float(
            opus_packet_bytes,
            interleaved_output,
            false
        ))
        .map_err(|failure| Error::Runtime(format!("libopus refused the packet: {failure}")))
    }

    fn reset_state(&mut self) -> Result<()> {
        on_either_decoder_instance!(self, |decoder| decoder.reset_state()).map_err(|failure| {
            Error::Runtime(format!("libopus would not reset the decoder: {failure}"))
        })
    }
}

/// The decoder minted for one packet channel count.
struct MintedOpusDecoder {
    instance: MintedOpusDecoderInstance,
    /// The count this instance was constructed for. A packet declaring any
    /// other count re-mints, because libopus cannot be told a new one.
    minted_for_channels: u32,
    /// Interleaved output, allocated once at mint to the longest packet the
    /// format allows so no arriving bag can size an allocation.
    decode_scratch: Vec<f32>,
}

/// Where a contiguous run of decoded audio started, and how far into it the
/// decoder has emitted.
struct DecodedRunAnchor {
    /// The stamp of the packet that opened the run — and, after the
    /// `pre_skip` trim, the instant of the first sample emitted from it.
    first_emitted_sample_timestamp_ns: i64,
    /// Per-channel samples emitted since the anchor, which is the offset
    /// every later block's stamp is derived from.
    samples_emitted_since_anchor: u64,
    /// Per-channel samples to discard before this run emits anything.
    ///
    /// At stream start these are the encoder's priming — output produced
    /// before the encoder had real input to produce it from. At a re-entry
    /// after a gap they are *not*: the encoder never reset, so its output is
    /// still input-delayed by the same lookahead, and what these samples hold
    /// is real input that arrived inside the re-entry packet but sits before
    /// the stamp the run re-anchored on. Discarding them is what makes the
    /// first emitted sample exactly the anchoring stamp; the count is named
    /// in the re-entry line rather than dropped silently.
    samples_still_to_trim: u32,
}

/// Decodes encoded-audio-packet bags into audio blocks.
#[derive(Default)]
pub struct EncodedPacketToAudioBlockDecoder {
    minted: Option<MintedOpusDecoder>,
    sync_point_gate: EncodedStreamSyncPointGate,
    anchor: Option<DecodedRunAnchor>,
    blocks_published: u64,
}

impl EncodedPacketToAudioBlockDecoder {
    /// Decode one arriving packet, or hand back `None` when the doctrine
    /// says to discard it or when the whole packet was encoder priming.
    pub fn decode_one_arriving_packet(
        &mut self,
        packet: &EncodedAudioPacket,
        packet_timestamp_ns: i64,
    ) -> Result<Option<AudioBlock>> {
        let mut re_entering = match self
            .sync_point_gate
            .admit(packet.sequence_index, packet.is_sync_point)
        {
            ArrivingEncodedBagDisposition::Decode => false,
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint => {
                if self.sync_point_gate.sync_points_entered_at() > 1 {
                    tracing::warn!(
                        packets_not_seen = self.sync_point_gate.bags_lost_to_gaps(),
                        sequence_index = packet.sequence_index,
                        samples_discarded_realigning = packet.pre_skip,
                        "{OPUS_DECODER_PROCESSOR_NAME}: a gap in the encoded stream — resetting \
                         and re-entering here. Nothing is invented to bridge it; the gap stays \
                         derivable from the stamps either side, and the samples discarded to \
                         re-align on this packet's stamp are counted rather than dropped quietly"
                    );
                }
                true
            }
            // Unreachable while every Opus packet is a sync point, and kept
            // rather than collapsed because the gate is the shared one: a
            // producer that ever writes `is_sync_point = false` gets the
            // doctrine rather than a decode of a packet this reader has no
            // state for.
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint => return Ok(None),
        };

        if let Some(stale) = self
            .minted
            .take_if(|minted| minted.minted_for_channels != packet.channels)
        {
            tracing::info!(
                minted_for_channels = stale.minted_for_channels,
                packet_channels = packet.channels,
                "{OPUS_DECODER_PROCESSOR_NAME}: the producer's channel count changed — re-minting the decoder"
            );
            // A new instance holds none of the old one's state and primes
            // again, so the run restarts exactly as it does after a gap.
            re_entering = true;
        }
        let minted = match &mut self.minted {
            Some(minted) => minted,
            empty_slot => empty_slot.insert(mint_decoder_for_packet(packet)?),
        };

        if re_entering {
            minted.instance.reset_state()?;
            self.anchor = None;
        }

        let channels = packet.channels as usize;
        let decoded_samples = minted
            .instance
            .decode_into_interleaved_f32(&packet.opus_packet_bytes, &mut minted.decode_scratch)?;
        let decoded_interleaved = &minted.decode_scratch[..decoded_samples * channels];

        if packet.sample_rate != OPUS_SAMPLE_RATE_HZ {
            return Err(Error::Runtime(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: the packet declares {} Hz, but Opus codes at \
                 {OPUS_SAMPLE_RATE_HZ} Hz and a decoder reconstructs at that rate whatever the \
                 source was resampled from — refused rather than decoded at 48 kHz and \
                 published as though it had always been 48 kHz",
                packet.sample_rate
            )));
        }
        if packet.pre_skip > HIGHEST_CREDIBLE_PRE_SKIP_SAMPLES {
            return Err(Error::Runtime(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: the packet declares a `pre_skip` of {} samples, \
                 past the {HIGHEST_CREDIBLE_PRE_SKIP_SAMPLES} one whole Opus packet spans — an \
                 encoder's lookahead at 48 kHz is 312. Refused rather than trimmed, because \
                 honouring it would emit nothing for the rest of the run without saying why",
                packet.pre_skip
            )));
        }
        let anchor = self.anchor.get_or_insert(DecodedRunAnchor {
            first_emitted_sample_timestamp_ns: packet_timestamp_ns,
            samples_emitted_since_anchor: 0,
            samples_still_to_trim: packet.pre_skip,
        });

        let trimmed = (anchor.samples_still_to_trim as usize).min(decoded_samples);
        anchor.samples_still_to_trim -= trimmed as u32;
        let emitted_samples = decoded_samples - trimmed;
        if emitted_samples == 0 {
            return Ok(None);
        }

        let first_sample_timestamp_ns = anchor.first_emitted_sample_timestamp_ns
            + timestamp_offset_ns_for(anchor.samples_emitted_since_anchor);
        anchor.samples_emitted_since_anchor += emitted_samples as u64;

        let interleaved_sample_bytes: Vec<u8> = decoded_interleaved[trimmed * channels..]
            .iter()
            .flat_map(|scalar| scalar.to_le_bytes())
            .collect();

        self.blocks_published += 1;
        Ok(Some(AudioBlock {
            interleaved_sample_bytes,
            sample_rate: OPUS_SAMPLE_RATE_HZ,
            channels: packet.channels,
            sample_count: emitted_samples as u32,
            dtype: AudioSampleDtype::F32,
            first_sample_timestamp_ns,
        }))
    }

    /// How many blocks this decoder has published, for the teardown line.
    pub fn blocks_published(&self) -> u64 {
        self.blocks_published
    }

    /// How many packets the `sequence_index` gaps say the link lost.
    pub fn packets_lost_to_gaps(&self) -> u64 {
        self.sync_point_gate.bags_lost_to_gaps()
    }
}

/// Per-channel sample offset as nanoseconds at Opus's rate, in integer
/// rational arithmetic widened so a long run cannot overflow the multiply.
fn timestamp_offset_ns_for(samples_emitted_since_anchor: u64) -> i64 {
    (u128::from(samples_emitted_since_anchor) * 1_000_000_000u128 / u128::from(OPUS_SAMPLE_RATE_HZ))
        as i64
}

fn mint_decoder_for_packet(packet: &EncodedAudioPacket) -> Result<MintedOpusDecoder> {
    let layout =
        OpusStreamLayoutForSourceChannelCount::resolve(packet.channels).map_err(|refusal| {
            Error::Runtime(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: this packet cannot be decoded — {refusal}"
            ))
        })?;
    let instance = match layout {
        OpusStreamLayoutForSourceChannelCount::MappingFamilyZeroSingleStream { channels } => {
            MintedOpusDecoderInstance::SingleStream(
                opus::Decoder::new(OPUS_SAMPLE_RATE_HZ, channels).map_err(|failure| {
                    Error::Runtime(format!(
                        "{OPUS_DECODER_PROCESSOR_NAME}: libopus refused a {} channel decoder: {failure}",
                        packet.channels
                    ))
                })?,
            )
        }
        OpusStreamLayoutForSourceChannelCount::MappingFamilyOneMultistream(multistream) => {
            MintedOpusDecoderInstance::Multistream(
                opus::MSDecoder::new(
                    OPUS_SAMPLE_RATE_HZ,
                    multistream.streams,
                    multistream.coupled_streams,
                    multistream.vorbis_channel_order_mapping,
                )
                .map_err(|failure| {
                    Error::Runtime(format!(
                        "{OPUS_DECODER_PROCESSOR_NAME}: libopus refused a {} channel multistream decoder: \
                         {failure}",
                        packet.channels
                    ))
                })?,
            )
        }
    };
    tracing::info!(
        channels = packet.channels,
        channel_mapping_family = layout.channel_mapping_family(),
        pre_skip = packet.pre_skip,
        "{OPUS_DECODER_PROCESSOR_NAME}: minted the Opus decoder from the packet's channel count"
    );
    Ok(MintedOpusDecoder {
        instance,
        minted_for_channels: packet.channels,
        decode_scratch: vec![
            0f32;
            HIGHEST_PER_CHANNEL_SAMPLES_IN_ONE_OPUS_PACKET
                * packet.channels as usize
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_window_to_encoded_packet_encoder::{
        AudioWindowToEncodedPacketEncoder, OpusEncoderConfig,
    };

    const WINDOW_SAMPLE_COUNT: u32 = 960;
    const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;

    /// Stated floor for the tone round trip. libopus 1.6.1 at 64 kbit/s per
    /// channel measures 35.9 dB mono, 36.4 dB stereo and 39.2 dB at 5.1, so
    /// the floor sits far enough below to survive a library bump and far
    /// enough above to catch a channel mapping or framing regression, which
    /// costs tens of dB rather than fractions.
    const RECONSTRUCTION_SNR_FLOOR_DB: f64 = 30.0;

    /// A tone per channel, at a different frequency in each so a mapping
    /// that permuted the channels would show up as a swap rather than
    /// cancel out.
    fn a_tone(channels: u32, total_samples: u32) -> Vec<f32> {
        let mut interleaved = Vec::with_capacity((total_samples * channels) as usize);
        for sample in 0..total_samples {
            for channel in 0..channels {
                let hz = 220.0 * (channel as f32 + 1.0);
                let phase = sample as f32 * hz * std::f32::consts::TAU / OPUS_SAMPLE_RATE_HZ as f32;
                interleaved.push(0.4 * phase.sin());
            }
        }
        interleaved
    }

    fn a_window_from(
        interleaved: &[f32],
        channels: u32,
        window_index: u32,
        anchor_ns: i64,
    ) -> AudioBlock {
        let first = (window_index * WINDOW_SAMPLE_COUNT * channels) as usize;
        let last = first + (WINDOW_SAMPLE_COUNT * channels) as usize;
        let mut interleaved_sample_bytes = Vec::with_capacity((last - first) * 4);
        for scalar in &interleaved[first..last] {
            interleaved_sample_bytes.extend_from_slice(&scalar.to_le_bytes());
        }
        AudioBlock {
            interleaved_sample_bytes,
            sample_rate: OPUS_SAMPLE_RATE_HZ,
            channels,
            sample_count: WINDOW_SAMPLE_COUNT,
            dtype: AudioSampleDtype::F32,
            first_sample_timestamp_ns: anchor_ns
                + (window_index as i64 * WINDOW_SAMPLE_COUNT as i64 * NANOSECONDS_PER_SECOND
                    / OPUS_SAMPLE_RATE_HZ as i64),
        }
    }

    fn read_block_samples(block: &AudioBlock) -> Vec<f32> {
        block
            .interleaved_sample_bytes
            .chunks_exact(4)
            .map(|scalar| f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]]))
            .collect()
    }

    struct RoundTrip {
        decoded_interleaved: Vec<f32>,
        blocks: Vec<AudioBlock>,
        packets: Vec<(EncodedAudioPacket, i64)>,
    }

    fn round_trip(channels: u32, windows: u32, anchor_ns: i64) -> RoundTrip {
        let source = a_tone(channels, windows * WINDOW_SAMPLE_COUNT);
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();
        let config = OpusEncoderConfig {
            // Explicit so the measurement does not move with libopus's own
            // automatic rate; ample for a tone at any of these counts.
            bitrate_bps: Some(64_000 * channels),
            application: None,
        };

        let mut round_trip = RoundTrip {
            decoded_interleaved: Vec::new(),
            blocks: Vec::new(),
            packets: Vec::new(),
        };
        for window_index in 0..windows {
            let window = a_window_from(&source, channels, window_index, anchor_ns);
            let packet = encoder
                .encode_one_window(&config, &window)
                .expect("encodes")
                .expect("publishes a packet");
            round_trip
                .packets
                .push((packet.clone(), window.first_sample_timestamp_ns));
            if let Some(block) = decoder
                .decode_one_arriving_packet(&packet, window.first_sample_timestamp_ns)
                .expect("decodes")
            {
                round_trip
                    .decoded_interleaved
                    .extend(read_block_samples(&block));
                round_trip.blocks.push(block);
            }
        }
        round_trip
    }

    /// Signal-to-noise of the reconstruction against the source it was made
    /// from, over whatever length both share.
    fn reconstruction_snr_db(source: &[f32], decoded: &[f32]) -> f64 {
        let shared = source.len().min(decoded.len());
        assert!(shared > 0, "nothing was reconstructed");
        let mut signal = 0f64;
        let mut noise = 0f64;
        for (source_scalar, decoded_scalar) in source[..shared].iter().zip(&decoded[..shared]) {
            signal += f64::from(*source_scalar) * f64::from(*source_scalar);
            let error = f64::from(*source_scalar) - f64::from(*decoded_scalar);
            noise += error * error;
        }
        10.0 * (signal / noise).log10()
    }

    #[test]
    fn a_tone_survives_the_round_trip_at_one_two_and_six_channels() {
        for channels in [1, 2, 6] {
            let windows = 25;
            let source = a_tone(channels, windows * WINDOW_SAMPLE_COUNT);
            let round_trip = round_trip(channels, windows, 5_000);

            // The first block is short by `pre_skip`, so the reconstruction
            // is that much shorter than the source it aligns with.
            let pre_skip = round_trip.packets[0].0.pre_skip as usize;
            assert_eq!(
                round_trip.decoded_interleaved.len(),
                (source.len() / channels as usize - pre_skip) * channels as usize,
                "every decoded sample but the trimmed priming is emitted"
            );

            // Skip the first and last window: an encoder's very first frame
            // and the tail of a run are where a codec's own priming lives,
            // and the floor is about steady state.
            let steady_state = (WINDOW_SAMPLE_COUNT * channels) as usize;
            let snr_db = reconstruction_snr_db(
                &source[steady_state..source.len() - steady_state],
                &round_trip.decoded_interleaved[steady_state..],
            );
            assert!(
                snr_db >= RECONSTRUCTION_SNR_FLOOR_DB,
                "{channels} channels reconstructed at {snr_db:.1} dB, under the \
                 {RECONSTRUCTION_SNR_FLOOR_DB} dB floor"
            );
        }
    }

    #[test]
    fn the_first_emitted_sample_is_the_anchoring_packets_stamped_instant() {
        let anchor_ns = 7_654_321;
        let round_trip = round_trip(2, 4, anchor_ns);
        assert_eq!(
            round_trip.blocks[0].first_sample_timestamp_ns, anchor_ns,
            "the run anchors on the first packet's stamp"
        );
        // Without this the assertion above holds with the trim disabled
        // entirely, and would lock nothing about `pre_skip` at all.
        assert_eq!(
            round_trip.blocks[0].sample_count,
            WINDOW_SAMPLE_COUNT - round_trip.packets[0].0.pre_skip,
            "the first block is short by exactly the priming the trim discarded, which is what \
             makes its first sample the stamped instant rather than 6.5 ms of encoder warm-up"
        );
    }

    #[test]
    fn a_later_blocks_derived_stamp_lands_on_the_stamp_of_the_packet_whose_input_it_carries() {
        let anchor_ns = 1_000_000;
        let round_trip = round_trip(2, 6, anchor_ns);
        let pre_skip = i64::from(round_trip.packets[0].0.pre_skip);

        // Block 0 is short by `pre_skip`, so every later block starts that
        // much before its own packet's stamp — and lands exactly on the
        // stamp of the packet whose input it actually holds.
        let mut emitted_so_far = 0i64;
        for (block_index, block) in round_trip.blocks.iter().enumerate() {
            let expected = anchor_ns
                + emitted_so_far * NANOSECONDS_PER_SECOND / i64::from(OPUS_SAMPLE_RATE_HZ);
            assert_eq!(
                block.first_sample_timestamp_ns, expected,
                "block {block_index} is stamped from the anchor, not from a clock"
            );
            emitted_so_far += i64::from(block.sample_count);
        }

        let last = round_trip.blocks.last().expect("blocks were published");
        let (last_packet, last_packet_stamp) =
            round_trip.packets.last().expect("packets were published");
        assert_eq!(
            last.first_sample_timestamp_ns,
            last_packet_stamp - pre_skip * NANOSECONDS_PER_SECOND / i64::from(OPUS_SAMPLE_RATE_HZ),
            "the decoded stream lags its input by the encoder's lookahead, and the derived \
             stamp says so rather than claiming the packet's own instant"
        );
        assert_eq!(last.channels, last_packet.channels);
        assert_eq!(last.sample_rate, OPUS_SAMPLE_RATE_HZ);
        assert_eq!(last.dtype, AudioSampleDtype::F32);
    }

    #[test]
    fn a_sequence_index_gap_resets_the_decoder_and_re_enters_counting_what_was_lost() {
        let channels = 2;
        let source = a_tone(channels, 12 * WINDOW_SAMPLE_COUNT);
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();
        let config = OpusEncoderConfig::default();

        let mut published_after_the_gap = Vec::new();
        for window_index in 0..12u32 {
            let window = a_window_from(&source, channels, window_index, 0);
            let packet = encoder
                .encode_one_window(&config, &window)
                .expect("encodes")
                .expect("publishes");
            // Four packets never reach the decoder.
            if (4..8).contains(&window_index) {
                continue;
            }
            let block = decoder
                .decode_one_arriving_packet(&packet, window.first_sample_timestamp_ns)
                .expect("decodes across the gap");
            if window_index >= 8 {
                published_after_the_gap.push(block.expect("re-enters and publishes"));
            }
        }

        assert_eq!(
            decoder.packets_lost_to_gaps(),
            4,
            "the gap is counted, not concealed"
        );
        let first_after_the_gap = &published_after_the_gap[0];
        assert_eq!(
            first_after_the_gap.sample_count,
            WINDOW_SAMPLE_COUNT - 312,
            "re-entry primes again, so the first block after the gap is trimmed again"
        );
        assert_eq!(
            first_after_the_gap.first_sample_timestamp_ns,
            8 * i64::from(WINDOW_SAMPLE_COUNT) * NANOSECONDS_PER_SECOND
                / i64::from(OPUS_SAMPLE_RATE_HZ),
            "the run re-anchors on the packet it re-entered at, so the gap stays derivable \
             from the stamps either side rather than being bridged"
        );
    }

    #[test]
    fn a_packet_whose_channel_count_changes_re_mints_the_decoder_and_re_enters() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();
        let config = OpusEncoderConfig::default();

        let mut published = Vec::new();
        for (window_index, channels) in [2u32, 2, 6, 6].into_iter().enumerate() {
            let source = a_tone(channels, WINDOW_SAMPLE_COUNT);
            let window = a_window_from(&source, channels, 0, window_index as i64 * 20_000_000);
            let packet = encoder
                .encode_one_window(&config, &window)
                .expect("encodes")
                .expect("publishes");
            if let Some(block) = decoder
                .decode_one_arriving_packet(&packet, window.first_sample_timestamp_ns)
                .expect("decodes across the re-mint")
            {
                published.push(block);
            }
        }

        assert_eq!(
            published.iter().map(|b| b.channels).collect::<Vec<_>>(),
            vec![2, 2, 6, 6],
            "every block carries the count its own packet declared"
        );
        assert_eq!(
            published[2].sample_count,
            WINDOW_SAMPLE_COUNT - 312,
            "a re-minted decoder holds none of the old state, so it primes and trims again"
        );
        assert_eq!(
            published[2].first_sample_timestamp_ns, 40_000_000,
            "the re-mint re-anchors on the packet that caused it"
        );
    }

    /// `sample_count` is an unvalidated `u32` any producer can write. It must
    /// not size an allocation: at eight channels a declared `u32::MAX` would
    /// ask for about 137 GB. The packet's own TOC byte is the authority on how
    /// long it is, and the buffer is sized from the format's own ceiling.
    #[test]
    fn a_packet_over_declaring_its_sample_count_neither_allocates_nor_misreports() {
        let channels = 2;
        let source = a_tone(channels, WINDOW_SAMPLE_COUNT);
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let window = a_window_from(&source, channels, 0, 0);
        let mut packet = encoder
            .encode_one_window(&OpusEncoderConfig::default(), &window)
            .expect("encodes")
            .expect("publishes");
        packet.sample_count = u32::MAX;

        let block = EncodedPacketToAudioBlockDecoder::default()
            .decode_one_arriving_packet(&packet, 0)
            .expect("decodes from the packet, not from the declaration")
            .expect("publishes a block");
        assert_eq!(
            block.sample_count,
            WINDOW_SAMPLE_COUNT - 312,
            "the emitted count comes from what libopus actually decoded, less the priming"
        );
    }

    #[test]
    fn a_packet_that_does_not_claim_opuss_own_clock_is_refused_rather_than_relabelled() {
        let channels = 2;
        let source = a_tone(channels, WINDOW_SAMPLE_COUNT);
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let window = a_window_from(&source, channels, 0, 0);
        let mut packet = encoder
            .encode_one_window(&OpusEncoderConfig::default(), &window)
            .expect("encodes")
            .expect("publishes");
        packet.sample_rate = 44_100;

        let refusal = EncodedPacketToAudioBlockDecoder::default()
            .decode_one_arriving_packet(&packet, 0)
            .expect_err("refused")
            .to_string();
        assert!(
            refusal.contains("44100"),
            "names the rate it was handed: {refusal}"
        );
        assert!(
            refusal.contains("48000"),
            "names the rate Opus codes at: {refusal}"
        );
    }

    /// An unbounded `pre_skip` would trim every sample of every packet for the
    /// rest of the run, and a decoder that goes silent without saying why is
    /// the concealment this module refuses to do.
    #[test]
    fn a_pre_skip_past_a_whole_packet_is_refused_rather_than_silently_swallowing_the_run() {
        let channels = 2;
        let source = a_tone(channels, WINDOW_SAMPLE_COUNT);
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let window = a_window_from(&source, channels, 0, 0);
        let mut packet = encoder
            .encode_one_window(&OpusEncoderConfig::default(), &window)
            .expect("encodes")
            .expect("publishes");
        packet.pre_skip = u32::MAX;

        let refusal = EncodedPacketToAudioBlockDecoder::default()
            .decode_one_arriving_packet(&packet, 0)
            .expect_err("refused")
            .to_string();
        assert!(
            refusal.contains(&u32::MAX.to_string()),
            "names the value it was handed: {refusal}"
        );
        assert!(
            refusal.contains(OPUS_DECODER_PROCESSOR_NAME),
            "names the processor: {refusal}"
        );

        // The bound refuses only the incredible: a real lookahead still trims.
        packet.pre_skip = 312;
        let block = EncodedPacketToAudioBlockDecoder::default()
            .decode_one_arriving_packet(&packet, 0)
            .expect("a real lookahead decodes")
            .expect("publishes a block");
        assert_eq!(block.sample_count, WINDOW_SAMPLE_COUNT - 312);
    }

    #[test]
    fn a_packet_naming_a_channel_count_opus_cannot_place_is_refused_by_name() {
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();
        let refusal = decoder
            .decode_one_arriving_packet(
                &EncodedAudioPacket {
                    codec: crate::encoded_audio_packet::EncodedAudioCodec::Opus,
                    opus_packet_bytes: vec![0xfc, 0xff, 0xfe],
                    is_sync_point: true,
                    group_index: 0,
                    sequence_index: 0,
                    sample_rate: OPUS_SAMPLE_RATE_HZ,
                    channels: 12,
                    sample_count: WINDOW_SAMPLE_COUNT,
                    pre_skip: 312,
                },
                0,
            )
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains("12"), "names the count: {refusal}");
        assert!(
            refusal.contains(OPUS_DECODER_PROCESSOR_NAME),
            "names the processor: {refusal}"
        );
    }
}
