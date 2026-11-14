// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The `OpusDecoder` body: one Opus packet in, one audio block out.
//!
//! The channel count and the encoder's lookahead come from the upstream
//! port's `OpusHead`. An audio entry carrying none names the channel count;
//! with no entry at all the first packet's TOC fixes it for the run. Either
//! way nothing is trimmed.
//!
//! Two things it does not do, both doctrine rather than omission: it never
//! conceals and it never invents. A read reporting a loss is the answer to
//! reset, re-enter at the next packet — which is every packet, since each
//! one opens a group — and log what was not seen.
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

use crate::audio_window::DecodedAudioBlock;
use crate::audio_window_to_encoded_packet_encoder::{
    OPUS_RFC6381_CODEC_STRING, OPUS_SAMPLE_RATE_HZ,
};
use crate::message_group_loss_recovery_gate::{
    MessageGroupLossRecoveryDisposition, MessageGroupLossRecoveryGate,
};
use crate::opus_codec_refusal::{OpusCodecNotice, OpusCodecRefusal, Result};
use crate::opus_identification_header::{
    OpusIdentificationHeader, channels_coded_by_the_opus_packet_toc,
};
use crate::opus_stream_layout::OpusStreamLayoutForSourceChannelCount;

/// The node's class name, which every refusal and notice names itself by.
pub const OPUS_DECODER_PROCESSOR_NAME: &str = "OpusDecoder";

/// One message off the decoder's input link: a `tatolab.stream.Message`'s
/// fields, borrowed.
#[derive(Debug, Clone, Copy)]
pub struct ArrivingEncodedAudioMessage<'payload> {
    pub payload_bytes: &'payload [u8],
    pub timestamp_ns: i64,
    pub starts_group: bool,
    pub lost_since_last_read: u64,
}

/// The fields of the upstream output port's Packets stream facts the decoder
/// reads, as `upstream_stream_facts` hands them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamPacketsStreamFacts {
    pub rfc6381_codec: Option<String>,
    pub channel_count: Option<u32>,
    pub codec_configuration_hex: Option<String>,
}

/// The most per-channel samples one Opus packet can span: RFC 6716 §2.1.4
/// caps a packet at 120 ms, which is 5 760 samples at Opus's 48 kHz clock.
///
/// This sizes the decode buffer: the packet's own TOC byte is the authority
/// on how long it is, and libopus reports that as the decoded count.
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
        .map_err(|failure| {
            OpusCodecRefusal(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: libopus refused the packet: {failure}"
            ))
        })
    }

    fn reset_state(&mut self) -> Result<()> {
        on_either_decoder_instance!(self, |decoder| decoder.reset_state()).map_err(|failure| {
            OpusCodecRefusal(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: libopus would not reset the decoder: {failure}"
            ))
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
    /// format allows so no arriving packet can size an allocation.
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

/// Decodes Opus packets into audio blocks.
#[derive(Default)]
pub struct EncodedPacketToAudioBlockDecoder {
    minted: Option<MintedOpusDecoder>,
    group_loss_recovery_gate: MessageGroupLossRecoveryGate,
    anchor: Option<DecodedRunAnchor>,
    blocks_published: u64,
    /// The upstream entry last read and the stream format it named, so a
    /// packet converts and parses it only when the entry changed.
    stream_format_from_the_upstream_stream_facts:
        Option<(UpstreamPacketsStreamFacts, OpusIdentificationHeader)>,
    /// The channel count the first packet's TOC coded, for a stream whose
    /// entry names none. Read once: libopus codes a stereo stream's packets
    /// mono at low rates, so the flag of any later packet is no count.
    channels_the_first_packets_toc_coded: Option<u32>,
    notices: Vec<OpusCodecNotice>,
}

impl EncodedPacketToAudioBlockDecoder {
    /// Decode one arriving message — one bare Opus packet —
    /// in the stream format the upstream port's stream facts name, or hand
    /// back `None` when the doctrine says to discard it or when the whole
    /// packet was encoder priming.
    pub fn decode_one_arriving_message(
        &mut self,
        message: &ArrivingEncodedAudioMessage<'_>,
        upstream_packets_stream_facts: Option<&UpstreamPacketsStreamFacts>,
    ) -> Result<Option<DecodedAudioBlock>> {
        // Admitted before anything can fail, so a reported loss is never
        // dropped with a message this decoder could not read.
        let disposition = self
            .group_loss_recovery_gate
            .admit_message(message.starts_group, message.lost_since_last_read);
        if disposition == MessageGroupLossRecoveryDisposition::DiscardUntilTheNextGroupStart {
            return Ok(None);
        }
        let decode_outcome =
            self.decode_one_admitted_message(message, disposition, upstream_packets_stream_facts);
        if decode_outcome.is_err() {
            self.group_loss_recovery_gate
                .discard_the_rest_of_the_current_group();
        }
        decode_outcome
    }

    fn decode_one_admitted_message(
        &mut self,
        message: &ArrivingEncodedAudioMessage<'_>,
        disposition: MessageGroupLossRecoveryDisposition,
        upstream_packets_stream_facts: Option<&UpstreamPacketsStreamFacts>,
    ) -> Result<Option<DecodedAudioBlock>> {
        if message.payload_bytes.is_empty() {
            return Err(OpusCodecRefusal(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: an empty message is no Opus packet"
            )));
        }
        let stream_format = self
            .stream_format_for_the_packet(upstream_packets_stream_facts, message.payload_bytes)?;
        self.decode_one_packet_in_stream_format(
            message,
            disposition,
            message.payload_bytes,
            stream_format,
        )
    }

    /// The stream format a packet decodes in: the upstream port's `OpusHead`
    /// when its stream facts carry one; otherwise their channel count, or with
    /// no facts the count the first packet's TOC coded, with nothing to trim.
    /// Facts naming another codec, and an `OpusHead` that does not read, are
    /// refused by name rather than passed over.
    fn stream_format_for_the_packet(
        &mut self,
        upstream_packets_stream_facts: Option<&UpstreamPacketsStreamFacts>,
        opus_packet_bytes: &[u8],
    ) -> Result<OpusIdentificationHeader> {
        let refused = |refusal: &dyn std::fmt::Display| {
            OpusCodecRefusal(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: the upstream port's stream facts describe no \
                 Opus stream — {refusal}"
            ))
        };
        let Some(upstream_packets_stream_facts) = upstream_packets_stream_facts else {
            return Ok(OpusIdentificationHeader {
                channels: self.channels_fixed_by_the_first_packets_toc(opus_packet_bytes)?,
                pre_skip: 0,
            });
        };
        if let Some((cached_upstream_stream_facts, stream_format)) =
            &self.stream_format_from_the_upstream_stream_facts
            && cached_upstream_stream_facts == upstream_packets_stream_facts
        {
            return Ok(*stream_format);
        }
        match upstream_packets_stream_facts.rfc6381_codec.as_deref() {
            Some(OPUS_RFC6381_CODEC_STRING) => {}
            Some(codec) => {
                return Err(refused(&format!(
                    "it names codec `{codec}`, not `{OPUS_RFC6381_CODEC_STRING}`"
                )));
            }
            None => return Err(refused(&"it names no codec")),
        }
        let stream_format = match (
            &upstream_packets_stream_facts.codec_configuration_hex,
            upstream_packets_stream_facts.channel_count,
        ) {
            (Some(hex_encoded_opus_head), _) => {
                OpusIdentificationHeader::parse_hex_encoded(hex_encoded_opus_head)
                    .map_err(|refusal| refused(&refusal))?
            }
            (None, Some(channels)) => OpusIdentificationHeader {
                channels,
                pre_skip: 0,
            },
            (None, None) => {
                return Err(refused(
                    &"it carries neither an `OpusHead` nor a channel count",
                ));
            }
        };
        self.stream_format_from_the_upstream_stream_facts =
            Some((upstream_packets_stream_facts.clone(), stream_format));
        Ok(stream_format)
    }

    fn channels_fixed_by_the_first_packets_toc(&mut self, opus_packet_bytes: &[u8]) -> Result<u32> {
        if let Some(channels) = self.channels_the_first_packets_toc_coded {
            return Ok(channels);
        }
        let channels = channels_coded_by_the_opus_packet_toc(opus_packet_bytes).ok_or_else(|| {
            OpusCodecRefusal(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: an empty packet has no TOC byte to read a channel \
                 count from"
            ))
        })?;
        Ok(*self.channels_the_first_packets_toc_coded.insert(channels))
    }

    fn decode_one_packet_in_stream_format(
        &mut self,
        message: &ArrivingEncodedAudioMessage<'_>,
        disposition: MessageGroupLossRecoveryDisposition,
        opus_packet_bytes: &[u8],
        stream_format: OpusIdentificationHeader,
    ) -> Result<Option<DecodedAudioBlock>> {
        let lost_since_last_read = message.lost_since_last_read;
        let packet_timestamp_ns = message.timestamp_ns;
        let mut re_entering = match disposition {
            MessageGroupLossRecoveryDisposition::Read => false,
            MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart => {
                if self.group_loss_recovery_gate.group_starts_entered_at() > 1 {
                    self.notices.push(OpusCodecNotice::warn(format!(
                        "{OPUS_DECODER_PROCESSOR_NAME}: the link lost packets — resetting and \
                         re-entering here. Nothing is invented to bridge the loss; it stays \
                         derivable from the stamps either side, and the samples discarded to \
                         re-align on this packet's stamp are counted rather than dropped \
                         quietly — packets_not_seen={lost_since_last_read} \
                         packets_lost_on_the_link={} samples_discarded_realigning={}",
                        self.group_loss_recovery_gate.messages_lost_on_the_link(),
                        stream_format.pre_skip,
                    )));
                }
                true
            }
            // Unreachable while every Opus packet opens a group, and kept
            // rather than collapsed: a producer writing a packet that
            // continues a group gets the doctrine rather than a decode of a
            // packet this reader has no state for.
            MessageGroupLossRecoveryDisposition::DiscardUntilTheNextGroupStart => return Ok(None),
        };

        if let Some(stale) = self
            .minted
            .take_if(|minted| minted.minted_for_channels != stream_format.channels)
        {
            self.notices.push(OpusCodecNotice::info(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: the producer's channel count changed from {} to {} \
                 — re-minting the decoder",
                stale.minted_for_channels, stream_format.channels
            )));
            // A new instance holds none of the old one's state and primes
            // again, so the run restarts exactly as it does after a gap.
            re_entering = true;
        }
        let minted = match &mut self.minted {
            Some(minted) => minted,
            empty_slot => {
                let (minted, notice) = mint_decoder_for_stream_format(stream_format)?;
                self.notices.push(notice);
                empty_slot.insert(minted)
            }
        };

        if re_entering {
            minted.instance.reset_state()?;
            self.anchor = None;
        }

        let channels = stream_format.channels as usize;
        let decoded_samples = minted
            .instance
            .decode_into_interleaved_f32(opus_packet_bytes, &mut minted.decode_scratch)?;
        let decoded_interleaved = &minted.decode_scratch[..decoded_samples * channels];

        if stream_format.pre_skip > HIGHEST_CREDIBLE_PRE_SKIP_SAMPLES {
            return Err(OpusCodecRefusal(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: the OpusHead declares a `pre_skip` of {} samples, \
                 past the {HIGHEST_CREDIBLE_PRE_SKIP_SAMPLES} one whole Opus packet spans — an \
                 encoder's lookahead at 48 kHz is 312. Refused rather than trimmed, because \
                 honouring it would emit nothing for the rest of the run without saying why",
                stream_format.pre_skip
            )));
        }
        let anchor = self.anchor.get_or_insert(DecodedRunAnchor {
            first_emitted_sample_timestamp_ns: packet_timestamp_ns,
            samples_emitted_since_anchor: 0,
            samples_still_to_trim: stream_format.pre_skip,
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
        Ok(Some(DecodedAudioBlock {
            interleaved_sample_bytes,
            sample_rate: OPUS_SAMPLE_RATE_HZ,
            channels: stream_format.channels,
            sample_count: emitted_samples as u32,
            first_sample_timestamp_ns,
        }))
    }

    /// How many blocks this decoder has published, for the teardown line.
    pub fn blocks_published(&self) -> u64 {
        self.blocks_published
    }

    /// How many packets the link reported lost.
    pub fn packets_lost_on_the_link(&self) -> u64 {
        self.group_loss_recovery_gate.messages_lost_on_the_link()
    }

    /// What the decoder did since the last call that an operator reads.
    pub fn take_notices(&mut self) -> Vec<OpusCodecNotice> {
        std::mem::take(&mut self.notices)
    }
}

/// Per-channel sample offset as nanoseconds at Opus's rate, in integer
/// rational arithmetic widened so a long run cannot overflow the multiply.
fn timestamp_offset_ns_for(samples_emitted_since_anchor: u64) -> i64 {
    (u128::from(samples_emitted_since_anchor) * 1_000_000_000u128 / u128::from(OPUS_SAMPLE_RATE_HZ))
        as i64
}

fn mint_decoder_for_stream_format(
    stream_format: OpusIdentificationHeader,
) -> Result<(MintedOpusDecoder, OpusCodecNotice)> {
    let layout = OpusStreamLayoutForSourceChannelCount::resolve(stream_format.channels).map_err(
        |refusal| {
            OpusCodecRefusal(format!(
                "{OPUS_DECODER_PROCESSOR_NAME}: this stream cannot be decoded — {refusal}"
            ))
        },
    )?;
    let instance = match layout {
        OpusStreamLayoutForSourceChannelCount::MappingFamilyZeroSingleStream { channels } => {
            MintedOpusDecoderInstance::SingleStream(
                opus::Decoder::new(OPUS_SAMPLE_RATE_HZ, channels).map_err(|failure| {
                    OpusCodecRefusal(format!(
                        "{OPUS_DECODER_PROCESSOR_NAME}: libopus refused a {} channel decoder: {failure}",
                        stream_format.channels
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
                    OpusCodecRefusal(format!(
                        "{OPUS_DECODER_PROCESSOR_NAME}: libopus refused a {} channel multistream decoder: \
                         {failure}",
                        stream_format.channels
                    ))
                })?,
            )
        }
    };
    let notice = OpusCodecNotice::info(format!(
        "{OPUS_DECODER_PROCESSOR_NAME}: minted the Opus decoder from the stream's channel count — \
         channels={} channel_mapping_family={} pre_skip={}",
        stream_format.channels,
        layout.channel_mapping_family(),
        stream_format.pre_skip,
    ));
    let minted = MintedOpusDecoder {
        instance,
        minted_for_channels: stream_format.channels,
        decode_scratch: vec![
            0f32;
            HIGHEST_PER_CHANNEL_SAMPLES_IN_ONE_OPUS_PACKET
                * stream_format.channels as usize
        ],
    };
    Ok((minted, notice))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_window::{AudioSampleDtype, AudioWindowToEncode};
    use crate::audio_window_to_encoded_packet_encoder::{
        AudioWindowToEncodedPacketEncoder, OpusEncoderApplication, OpusEncoderConfig,
        OpusEncoderPortStreamFacts,
    };

    /// One message as the decoder reads it off its link, every one opening a
    /// group as every Opus packet does.
    struct OwnedArrivingMessage {
        payload_bytes: Vec<u8>,
        timestamp_ns: i64,
        lost_since_last_read: u64,
    }

    impl OwnedArrivingMessage {
        fn borrowed(&self) -> ArrivingEncodedAudioMessage<'_> {
            ArrivingEncodedAudioMessage {
                payload_bytes: &self.payload_bytes,
                timestamp_ns: self.timestamp_ns,
                starts_group: true,
                lost_since_last_read: self.lost_since_last_read,
            }
        }
    }

    /// One Opus packet, as the encoder publishes it, arriving on the link.
    fn arriving(
        opus_packet_bytes: &[u8],
        timestamp_ns: i64,
        lost_since_last_read: u64,
    ) -> OwnedArrivingMessage {
        OwnedArrivingMessage {
            payload_bytes: opus_packet_bytes.to_vec(),
            timestamp_ns,
            lost_since_last_read,
        }
    }

    /// A 20 ms window of samples the encoder is handed.
    struct OwnedAudioWindow {
        interleaved_sample_bytes: Vec<u8>,
        channels: u32,
        first_sample_timestamp_ns: i64,
    }

    impl OwnedAudioWindow {
        fn borrowed(&self) -> AudioWindowToEncode<'_> {
            AudioWindowToEncode {
                interleaved_sample_bytes: &self.interleaved_sample_bytes,
                sample_rate: OPUS_SAMPLE_RATE_HZ,
                channels: self.channels,
                sample_count: WINDOW_SAMPLE_COUNT,
                dtype: AudioSampleDtype::F32,
            }
        }
    }

    fn upstream_packets_stream_facts(
        rfc6381_codec: &str,
        channel_count: u32,
        codec_configuration_hex: Option<String>,
    ) -> UpstreamPacketsStreamFacts {
        UpstreamPacketsStreamFacts {
            rfc6381_codec: Some(rfc6381_codec.to_owned()),
            channel_count: Some(channel_count),
            codec_configuration_hex,
        }
    }

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
    ) -> OwnedAudioWindow {
        let first = (window_index * WINDOW_SAMPLE_COUNT * channels) as usize;
        let last = first + (WINDOW_SAMPLE_COUNT * channels) as usize;
        let mut interleaved_sample_bytes = Vec::with_capacity((last - first) * 4);
        for scalar in &interleaved[first..last] {
            interleaved_sample_bytes.extend_from_slice(&scalar.to_le_bytes());
        }
        OwnedAudioWindow {
            interleaved_sample_bytes,
            channels,
            first_sample_timestamp_ns: anchor_ns
                + (window_index as i64 * WINDOW_SAMPLE_COUNT as i64 * NANOSECONDS_PER_SECOND
                    / OPUS_SAMPLE_RATE_HZ as i64),
        }
    }

    fn read_block_samples(block: &DecodedAudioBlock) -> Vec<f32> {
        block
            .interleaved_sample_bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|scalar| f32::from_le_bytes(*scalar))
            .collect()
    }

    struct RoundTrip {
        decoded_interleaved: Vec<f32>,
        blocks: Vec<DecodedAudioBlock>,
        packet_stamps: Vec<i64>,
        /// What the encoder port's `OpusHead` said, for the assertions.
        stream_format: OpusIdentificationHeader,
    }

    /// The stream facts a decoder downstream of `encoder` reads off the
    /// encoder's port, when the last window's mint owed it some.
    fn upstream_stream_facts_owed_by(
        encoder: &mut AudioWindowToEncodedPacketEncoder,
    ) -> Option<UpstreamPacketsStreamFacts> {
        encoder.take_stream_facts_owed_to_the_port().map(
            |OpusEncoderPortStreamFacts {
                 channel_count,
                 codec_configuration_hex,
                 ..
             }| {
                upstream_packets_stream_facts(
                    OPUS_RFC6381_CODEC_STRING,
                    channel_count,
                    Some(codec_configuration_hex),
                )
            },
        )
    }

    fn opus_head_of(
        upstream_packets_stream_facts: &UpstreamPacketsStreamFacts,
    ) -> OpusIdentificationHeader {
        OpusIdentificationHeader::parse_hex_encoded(
            upstream_packets_stream_facts
                .codec_configuration_hex
                .as_deref()
                .expect("Opus stream facts carry their OpusHead"),
        )
        .expect("the OpusHead reads back")
    }

    fn round_trip(channels: u32, windows: u32, anchor_ns: i64) -> RoundTrip {
        let source = a_tone(channels, windows * WINDOW_SAMPLE_COUNT);
        // An explicit rate, so the measurement does not move with libopus's
        // own automatic one; ample for a tone at any of these counts.
        let mut encoder = AudioWindowToEncodedPacketEncoder::new(OpusEncoderConfig {
            bitrate_bps: Some(64_000 * channels),
            application: OpusEncoderApplication::Audio,
        });
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();

        let mut round_trip = RoundTrip {
            decoded_interleaved: Vec::new(),
            blocks: Vec::new(),
            packet_stamps: Vec::new(),
            stream_format: OpusIdentificationHeader {
                channels,
                pre_skip: 0,
            },
        };
        let mut upstream_packets_stream_facts = None;
        for window_index in 0..windows {
            let window = a_window_from(&source, channels, window_index, anchor_ns);
            let opus_packet_bytes = encoder
                .encode_one_window(&window.borrowed())
                .expect("encodes")
                .expect("publishes a packet");
            if let Some(owed_entry) = upstream_stream_facts_owed_by(&mut encoder) {
                round_trip.stream_format = opus_head_of(&owed_entry);
                upstream_packets_stream_facts = Some(owed_entry);
            }
            round_trip
                .packet_stamps
                .push(window.first_sample_timestamp_ns);
            if let Some(block) = decoder
                .decode_one_arriving_message(
                    &arriving(&opus_packet_bytes, window.first_sample_timestamp_ns, 0).borrowed(),
                    upstream_packets_stream_facts.as_ref(),
                )
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
            let pre_skip = round_trip.stream_format.pre_skip as usize;
            assert_eq!(
                pre_skip, 312,
                "the lookahead reached the decoder through the entry"
            );
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
            WINDOW_SAMPLE_COUNT - round_trip.stream_format.pre_skip,
            "the first block is short by exactly the priming the trim discarded, which is what \
             makes its first sample the stamped instant rather than 6.5 ms of encoder warm-up"
        );
    }

    #[test]
    fn a_later_blocks_derived_stamp_lands_on_the_stamp_of_the_packet_whose_input_it_carries() {
        let anchor_ns = 1_000_000;
        let round_trip = round_trip(2, 6, anchor_ns);
        let pre_skip = i64::from(round_trip.stream_format.pre_skip);

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
        let last_packet_stamp = round_trip
            .packet_stamps
            .last()
            .expect("packets were published");
        assert_eq!(
            last.first_sample_timestamp_ns,
            last_packet_stamp - pre_skip * NANOSECONDS_PER_SECOND / i64::from(OPUS_SAMPLE_RATE_HZ),
            "the decoded stream lags its input by the encoder's lookahead, and the derived \
             stamp says so rather than claiming the packet's own instant"
        );
        assert_eq!(last.channels, round_trip.stream_format.channels);
        assert_eq!(last.sample_rate, OPUS_SAMPLE_RATE_HZ);
    }

    #[test]
    fn a_reported_loss_resets_the_decoder_and_re_enters_counting_what_was_lost() {
        let channels = 2;
        let source = a_tone(channels, 12 * WINDOW_SAMPLE_COUNT);
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();

        let mut upstream_packets_stream_facts = None;
        let mut published_after_the_loss = Vec::new();
        for window_index in 0..12u32 {
            let window = a_window_from(&source, channels, window_index, 0);
            let opus_packet_bytes = encoder
                .encode_one_window(&window.borrowed())
                .expect("encodes")
                .expect("publishes");
            if let Some(owed_entry) = upstream_stream_facts_owed_by(&mut encoder) {
                upstream_packets_stream_facts = Some(owed_entry);
            }
            // Four packets never reach the decoder, and the read after them
            // says so.
            if (4..8).contains(&window_index) {
                continue;
            }
            let lost_since_last_read = if window_index == 8 { 4 } else { 0 };
            let block = decoder
                .decode_one_arriving_message(
                    &arriving(
                        &opus_packet_bytes,
                        window.first_sample_timestamp_ns,
                        lost_since_last_read,
                    )
                    .borrowed(),
                    upstream_packets_stream_facts.as_ref(),
                )
                .expect("decodes across the loss");
            if window_index >= 8 {
                published_after_the_loss.push(block.expect("re-enters and publishes"));
            }
        }

        assert_eq!(
            decoder.packets_lost_on_the_link(),
            4,
            "the loss is counted, not concealed"
        );
        let first_after_the_loss = &published_after_the_loss[0];
        assert_eq!(
            first_after_the_loss.sample_count,
            WINDOW_SAMPLE_COUNT - 312,
            "re-entry primes again, so the first block after the loss is trimmed again"
        );
        assert_eq!(
            first_after_the_loss.first_sample_timestamp_ns,
            8 * i64::from(WINDOW_SAMPLE_COUNT) * NANOSECONDS_PER_SECOND
                / i64::from(OPUS_SAMPLE_RATE_HZ),
            "the run re-anchors on the packet it re-entered at, so the loss stays derivable \
             from the stamps either side rather than being bridged"
        );
        assert_eq!(
            published_after_the_loss[1].sample_count, WINDOW_SAMPLE_COUNT,
            "a packet after the re-entry is read whole"
        );
    }

    #[test]
    fn a_packet_whose_channel_count_changes_re_mints_the_decoder_and_re_enters() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();

        let mut published = Vec::new();
        let mut upstream_packets_stream_facts = None;
        for (window_index, channels) in [2u32, 2, 6, 6].into_iter().enumerate() {
            let source = a_tone(channels, WINDOW_SAMPLE_COUNT);
            let window = a_window_from(&source, channels, 0, window_index as i64 * 20_000_000);
            let opus_packet_bytes = encoder
                .encode_one_window(&window.borrowed())
                .expect("encodes")
                .expect("publishes");
            if let Some(owed_entry) = upstream_stream_facts_owed_by(&mut encoder) {
                upstream_packets_stream_facts = Some(owed_entry);
            }
            if let Some(block) = decoder
                .decode_one_arriving_message(
                    &arriving(&opus_packet_bytes, window.first_sample_timestamp_ns, 0).borrowed(),
                    upstream_packets_stream_facts.as_ref(),
                )
                .expect("decodes across the re-mint")
            {
                published.push(block);
            }
        }

        assert_eq!(
            published.iter().map(|b| b.channels).collect::<Vec<_>>(),
            vec![2, 2, 6, 6],
            "every block carries the count the entry named for its packet"
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

    fn one_stereo_packet() -> Vec<u8> {
        let source = a_tone(2, WINDOW_SAMPLE_COUNT);
        AudioWindowToEncodedPacketEncoder::default()
            .encode_one_window(&a_window_from(&source, 2, 0, 0).borrowed())
            .expect("encodes")
            .expect("publishes")
    }

    /// An unbounded `pre_skip` would trim every sample of every packet for the
    /// rest of the run, and a decoder that goes silent without saying why is
    /// the concealment this module refuses to do.
    #[test]
    fn a_pre_skip_past_a_whole_packet_is_refused_rather_than_silently_swallowing_the_run() {
        let opus_packet_bytes = one_stereo_packet();
        let refusal = EncodedPacketToAudioBlockDecoder::default()
            .decode_one_packet_in_stream_format(
                &arriving(&opus_packet_bytes, 0, 0).borrowed(),
                MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart,
                &opus_packet_bytes,
                OpusIdentificationHeader {
                    channels: 2,
                    pre_skip: u32::MAX,
                },
            )
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
        let block = EncodedPacketToAudioBlockDecoder::default()
            .decode_one_packet_in_stream_format(
                &arriving(&opus_packet_bytes, 0, 0).borrowed(),
                MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart,
                &opus_packet_bytes,
                OpusIdentificationHeader {
                    channels: 2,
                    pre_skip: 312,
                },
            )
            .expect("a real lookahead decodes")
            .expect("publishes a block");
        assert_eq!(block.sample_count, WINDOW_SAMPLE_COUNT - 312);
    }

    #[test]
    fn a_stream_naming_a_channel_count_opus_cannot_place_is_refused_by_name() {
        let refusal = EncodedPacketToAudioBlockDecoder::default()
            .decode_one_packet_in_stream_format(
                &arriving(&[0x00, 0xfc, 0xff, 0xfe], 0, 0).borrowed(),
                MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart,
                &[0xfc, 0xff, 0xfe],
                OpusIdentificationHeader {
                    channels: 12,
                    pre_skip: 312,
                },
            )
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains("12"), "names the count: {refusal}");
        assert!(
            refusal.contains(OPUS_DECODER_PROCESSOR_NAME),
            "names the processor: {refusal}"
        );
    }

    /// A stream whose upstream entry carries no `OpusHead` — a foreign
    /// producer, or a port before its first packet — still decodes, at the
    /// count the TOC codes and with nothing trimmed.
    #[test]
    fn with_no_opus_head_upstream_the_toc_names_the_channels_and_nothing_is_trimmed() {
        let opus_packet_bytes = one_stereo_packet();
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();
        assert_eq!(
            decoder
                .stream_format_for_the_packet(None, &opus_packet_bytes)
                .unwrap(),
            OpusIdentificationHeader {
                channels: 2,
                pre_skip: 0
            }
        );
        let block = decoder
            .decode_one_arriving_message(&arriving(&opus_packet_bytes, 0, 0).borrowed(), None)
            .unwrap()
            .expect("publishes a block");
        assert_eq!(block.sample_count, WINDOW_SAMPLE_COUNT);
        assert_eq!(block.channels, 2);
    }

    #[test]
    fn an_upstream_codec_configuration_that_is_no_opus_head_is_refused_by_name() {
        let refusal = EncodedPacketToAudioBlockDecoder::default()
            .stream_format_for_the_packet(
                Some(&upstream_packets_stream_facts(
                    "opus",
                    2,
                    Some("00".repeat(19)),
                )),
                &one_stereo_packet(),
            )
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains("OpusHead"), "{refusal}");
        assert!(refusal.contains(OPUS_DECODER_PROCESSOR_NAME), "{refusal}");
    }

    #[test]
    fn upstream_stream_facts_naming_another_codec_are_refused_by_name() {
        let refusal = EncodedPacketToAudioBlockDecoder::default()
            .stream_format_for_the_packet(
                Some(&upstream_packets_stream_facts("mp4a.40.2", 2, None)),
                &one_stereo_packet(),
            )
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains("mp4a.40.2"), "{refusal}");
    }

    #[test]
    fn an_empty_message_is_refused_by_name_rather_than_concealed() {
        let refusal = EncodedPacketToAudioBlockDecoder::default()
            .decode_one_arriving_message(&arriving(&[], 0, 0).borrowed(), None)
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains("no Opus packet"), "{refusal}");
    }

    #[test]
    fn with_no_entry_upstream_the_first_packets_toc_fixes_the_channel_count() {
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();
        let stereo_toc_packet = one_stereo_packet();
        let mono_toc_packet = [0b1111_1000, 0xFF, 0xFE];
        assert_eq!(
            decoder
                .stream_format_for_the_packet(None, &stereo_toc_packet)
                .unwrap()
                .channels,
            2
        );
        assert_eq!(
            decoder
                .stream_format_for_the_packet(None, &mono_toc_packet)
                .unwrap()
                .channels,
            2,
            "a later packet coded mono does not re-mint a stereo stream's decoder"
        );
    }

    #[test]
    fn an_entry_carrying_no_opus_head_names_the_channel_count() {
        let stream_format = EncodedPacketToAudioBlockDecoder::default()
            .stream_format_for_the_packet(
                Some(&upstream_packets_stream_facts("opus", 6, None)),
                &one_stereo_packet(),
            )
            .unwrap();
        assert_eq!(
            stream_format,
            OpusIdentificationHeader {
                channels: 6,
                pre_skip: 0
            }
        );
    }

    /// The loss rides the message that could not be read, so the gate takes
    /// it before the read fails, and the next packet re-enters: a fresh
    /// priming trim, re-anchored on its own stamp.
    #[test]
    fn a_loss_reported_on_an_unreadable_message_still_re_enters_at_the_next_packet() {
        let channels = 2;
        let source = a_tone(channels, 4 * WINDOW_SAMPLE_COUNT);
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let mut decoder = EncodedPacketToAudioBlockDecoder::default();
        let mut upstream_packets_stream_facts = None;
        let mut blocks = Vec::new();
        for window_index in 0..4u32 {
            let window = a_window_from(&source, channels, window_index, 0);
            let opus_packet_bytes = encoder
                .encode_one_window(&window.borrowed())
                .expect("encodes")
                .expect("publishes");
            if let Some(owed_entry) = upstream_stream_facts_owed_by(&mut encoder) {
                upstream_packets_stream_facts = Some(owed_entry);
            }
            if window_index == 2 {
                let unreadable_message_reporting_a_loss =
                    arriving(&[], window.first_sample_timestamp_ns, 3);
                decoder
                    .decode_one_arriving_message(
                        &unreadable_message_reporting_a_loss.borrowed(),
                        upstream_packets_stream_facts.as_ref(),
                    )
                    .expect_err("an empty message is no Opus packet");
                continue;
            }
            blocks.extend(
                decoder
                    .decode_one_arriving_message(
                        &arriving(&opus_packet_bytes, window.first_sample_timestamp_ns, 0)
                            .borrowed(),
                        upstream_packets_stream_facts.as_ref(),
                    )
                    .expect("decodes"),
            );
        }

        assert_eq!(decoder.packets_lost_on_the_link(), 3);
        let first_after_the_loss = blocks.last().expect("the last packet decodes");
        assert_eq!(
            first_after_the_loss.sample_count,
            WINDOW_SAMPLE_COUNT - 312,
            "the packet after the loss re-enters, priming and trimming again"
        );
        assert_eq!(
            first_after_the_loss.first_sample_timestamp_ns,
            3 * i64::from(WINDOW_SAMPLE_COUNT) * NANOSECONDS_PER_SECOND
                / i64::from(OPUS_SAMPLE_RATE_HZ),
            "and re-anchors on its own stamp rather than bridging the loss"
        );
    }
}
