// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `OpusEncoder` body: one 20 ms audio window in, one encoded-audio
//! packet out.
//!
//! Separated from the port surface so it is driven directly by a test with
//! no `Runtime` — the shape the video encode body already takes.
//!
//! The encoder is minted from the first window's channel count, the video
//! encoder's first-frame pattern, because the input port declares no channel
//! count and so follows its source. A window whose count changes re-mints:
//! libopus fixes an instance's channel count at construction and offers no
//! ctl that changes it, so re-minting is the only mechanism there is.
//! `OPUS_SET_FORCE_CHANNELS` is not that mechanism — it moves the *coded*
//! mono/stereo of an already-constructed instance, is bounded by that
//! instance's own count, and has no mapping-family-1 equivalent at all.
//!
//! What a re-mint costs is continuity, not decodability: a fresh instance
//! has lost its prediction and analysis state, so the seam is a quality
//! discontinuity in exactly the place the window stage already flushed its
//! accumulator and resampler. `sequence_index` does not reset across it —
//! the counter belongs to the encoder, not the instance — so a consumer
//! still reads a gap as loss and never as a restart.

use serde::{Deserialize, Serialize};
use streamlib::sdk::error::{Error, Result};

use crate::audio_block::{AudioBlock, AudioSampleDtype};
use crate::encoded_audio_packet::{EncodedAudioCodec, EncodedAudioPacket};
use crate::encoded_stream_ordering::EncodedStreamOrderingPairCounter;
#[cfg(test)]
use crate::opus_stream_layout::HIGHEST_CHANNEL_COUNT_OPUS_PLACES;
use crate::opus_stream_layout::OpusStreamLayoutForSourceChannelCount;

/// Registration name, and what every refusal and log line names itself by.
pub const OPUS_ENCODER_PROCESSOR_NAME: &str = "OpusEncoder";

/// Opus's own clock. Every packet is stamped in it whatever the source rate
/// was, and the input port's window contract resamples to it.
pub const OPUS_SAMPLE_RATE_HZ: u32 = 48_000;

/// The most bytes one Opus frame can occupy — RFC 6716 §3.2.1 caps a frame
/// at 1275 bytes, and the TOC byte and per-stream length prefix ride beside
/// it. A multistream packet carries one such frame per stream, so the output
/// buffer scales with the stream count and no valid packet can overrun it.
const HIGHEST_PACKET_BYTES_PER_OPUS_STREAM: usize = 1276;

/// Headroom over the per-stream cap for a multistream packet's own framing.
const MULTISTREAM_PACKET_FRAMING_HEADROOM_BYTES: usize = 256;

/// Which libopus tuning an [`OpusEncoderConfig`] asks for, spelled the way
/// the wire spells it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpusEncoderApplication {
    /// Broadcast and high fidelity: the decoded audio should be as close as
    /// possible to the input. The default, because a recording rung wants
    /// fidelity rather than intelligibility under loss.
    #[default]
    #[serde(rename = "audio")]
    Audio,
    /// Speech: listening quality and intelligibility matter most.
    #[serde(rename = "voip")]
    Voip,
    /// Lowest achievable latency, at a cost in quality.
    #[serde(rename = "lowdelay")]
    LowDelay,
}

impl OpusEncoderApplication {
    fn as_libopus_application(self) -> opus::Application {
        match self {
            OpusEncoderApplication::Audio => opus::Application::Audio,
            OpusEncoderApplication::Voip => opus::Application::Voip,
            OpusEncoderApplication::LowDelay => opus::Application::LowDelay,
        }
    }
}

/// Configuration for `OpusEncoder`. Both knobs are optional, so `{}` is a
/// legal config: the channel count follows the source, the sample rate and
/// framing are the port's window contract, and what is left is the rate and
/// the tuning.
///
/// In-band FEC and DTX are off and are not knobs. FEC spends bitrate on
/// redundancy a recording never reads, and DTX replaces silence with nothing
/// — a gap the plan's own doctrine says must stay derivable from the stamps
/// rather than be invented back by a decoder.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpusEncoderConfig {
    /// Target bitrate in bits per second. Absent, libopus picks its own from
    /// the sample rate and channel count.
    pub bitrate_bps: Option<u32>,
    /// Which tuning libopus encodes for. Absent means
    /// [`OpusEncoderApplication::Audio`].
    pub application: Option<OpusEncoderApplication>,
}

/// Dispatch one expression over either shape of instance.
///
/// The two libopus wrappers share every ctl name through the crate's own
/// `encoder_ctls!` macro but no trait, so a call has to be written per
/// variant. Writing it once here is what keeps the settings below from
/// being two copies that can drift apart.
macro_rules! on_either_encoder_instance {
    ($instance:expr, |$encoder:ident| $body:expr) => {
        match $instance {
            MintedOpusEncoderInstance::SingleStream($encoder) => $body,
            MintedOpusEncoderInstance::Multistream($encoder) => $body,
        }
    };
}

/// A libopus encoder instance, in whichever of the two shapes its channel
/// count resolved to.
enum MintedOpusEncoderInstance {
    SingleStream(opus::Encoder),
    Multistream(opus::MSEncoder),
}

impl MintedOpusEncoderInstance {
    fn encode_interleaved_f32(&mut self, samples: &[f32], output_bytes: usize) -> Result<Vec<u8>> {
        on_either_encoder_instance!(self, |encoder| encoder
            .encode_vec_float(samples, output_bytes))
        .map_err(|failure| Error::Runtime(format!("libopus refused the window: {failure}")))
    }

    fn lookahead_samples(&mut self) -> Result<u32> {
        let lookahead = on_either_encoder_instance!(self, |encoder| encoder.get_lookahead())
            .map_err(|failure| {
                Error::Runtime(format!("libopus would not report its lookahead: {failure}"))
            })?;
        u32::try_from(lookahead).map_err(|_| {
            Error::Runtime(format!(
                "libopus reported a negative lookahead of {lookahead}"
            ))
        })
    }

    /// Everything the config and the doctrine fix, applied to a freshly
    /// minted instance.
    fn apply_settings(&mut self, config: &OpusEncoderConfig) -> Result<()> {
        let named = |what: &str, failure: opus::Error| {
            Error::Runtime(format!(
                "{OPUS_ENCODER_PROCESSOR_NAME}: libopus refused {what}: {failure}"
            ))
        };
        if let Some(bitrate_bps) = config.bitrate_bps {
            let bitrate_bps = i32::try_from(bitrate_bps).map_err(|_| {
                Error::Runtime(format!(
                    "{OPUS_ENCODER_PROCESSOR_NAME}: a bitrate of {bitrate_bps} bit/s is past what libopus \
                     takes"
                ))
            })?;
            on_either_encoder_instance!(self, |encoder| encoder
                .set_bitrate(opus::Bitrate::Bits(bitrate_bps)))
            .map_err(|failure| named("the bitrate", failure))?;
        }
        on_either_encoder_instance!(self, |encoder| encoder.set_inband_fec(false))
            .map_err(|failure| named("in-band FEC off", failure))?;
        on_either_encoder_instance!(self, |encoder| encoder.set_dtx(false))
            .map_err(|failure| named("DTX off", failure))?;
        Ok(())
    }
}

/// The encoder minted for one source channel count, and what a packet it
/// produces carries beside its bytes.
struct MintedOpusEncoder {
    instance: MintedOpusEncoderInstance,
    /// The count this instance was constructed for. A window arriving with
    /// any other count re-mints, because libopus cannot be told a new one.
    minted_for_source_channels: u32,
    /// The instance's reported lookahead, which every packet carries so a
    /// decoder entering the stream knows what to discard.
    pre_skip: u32,
    /// Output buffer size, from the stream count the layout resolved to.
    highest_packet_bytes: usize,
}

/// Encodes 20 ms audio windows into encoded-audio-packet bags.
#[derive(Default)]
pub struct AudioWindowToEncodedPacketEncoder {
    minted: Option<MintedOpusEncoder>,
    ordering_pair_counter: EncodedStreamOrderingPairCounter,
    /// Latched once a mint has failed for a reason a later window cannot
    /// change — a channel count Opus cannot place. Every later window is
    /// discarded rather than re-attempted, so the refusal is said once
    /// instead of per dispatch.
    ///
    /// A `reactive` processor has no way to reach `ProcessorState::Error`:
    /// the runner logs an `Err` from `process()` and carries on. So the
    /// latch is what "refused by name" means here, and it is the shape the
    /// video encode body already uses for a session it could not mint.
    mint_already_failed: bool,
}

impl AudioWindowToEncodedPacketEncoder {
    /// Encode one window, or hand back `None` when this encoder has already
    /// refused its source and is discarding what follows.
    pub fn encode_one_window(
        &mut self,
        config: &OpusEncoderConfig,
        window: &AudioBlock,
    ) -> Result<Option<EncodedAudioPacket>> {
        if self.mint_already_failed {
            return Ok(None);
        }
        let interleaved_samples = read_window_as_interleaved_f32(window)?;

        if let Some(stale) = self
            .minted
            .take_if(|minted| minted.minted_for_source_channels != window.channels)
        {
            tracing::info!(
                minted_for_source_channels = stale.minted_for_source_channels,
                window_channels = window.channels,
                "{OPUS_ENCODER_PROCESSOR_NAME}: the source's channel count changed — re-minting the encoder"
            );
        }

        let minted = match &mut self.minted {
            Some(minted) => minted,
            empty_slot => match mint_encoder_for_window(config, window) {
                Ok(minted) => empty_slot.insert(minted),
                Err(mint_failure) => {
                    self.mint_already_failed = true;
                    tracing::error!(
                        "{OPUS_ENCODER_PROCESSOR_NAME}: the encoder could not be minted; every later window \
                         is discarded: {mint_failure}"
                    );
                    return Err(mint_failure);
                }
            },
        };

        let opus_packet_bytes = minted
            .instance
            .encode_interleaved_f32(&interleaved_samples, minted.highest_packet_bytes)?;

        // Every Opus packet is a decode entry point, so every one opens its
        // own group; the counter still owns `sequence_index`, which is why
        // it survives the re-mint above.
        let ordering_pair = self.ordering_pair_counter.account_published_bag(true);

        Ok(Some(EncodedAudioPacket {
            codec: EncodedAudioCodec::Opus,
            opus_packet_bytes,
            is_sync_point: true,
            group_index: ordering_pair.group_index,
            sequence_index: ordering_pair.sequence_index,
            sample_rate: OPUS_SAMPLE_RATE_HZ,
            channels: window.channels,
            sample_count: window.sample_count,
            pre_skip: minted.pre_skip,
        }))
    }

    /// How many packets this encoder has published, for the teardown line.
    pub fn packets_encoded(&self) -> u64 {
        self.ordering_pair_counter.bags_accounted()
    }
}

/// Read a window's payload as interleaved `f32`, refusing by name a block
/// that is not what the port's window contract promised rather than
/// reinterpreting its bytes.
fn read_window_as_interleaved_f32(window: &AudioBlock) -> Result<Vec<f32>> {
    if window.dtype != AudioSampleDtype::F32 {
        return Err(Error::Runtime(format!(
            "{OPUS_ENCODER_PROCESSOR_NAME}: a window arrived as {:?}, but the port's `audio_window` contract \
             declares `f32` — the stage converts, so this is a bag that did not come through it",
            window.dtype
        )));
    }
    if window.sample_rate != OPUS_SAMPLE_RATE_HZ {
        return Err(Error::Runtime(format!(
            "{OPUS_ENCODER_PROCESSOR_NAME}: a window arrived at {} Hz, but Opus codes at \
             {OPUS_SAMPLE_RATE_HZ} Hz and the port's `audio_window` contract resamples to it",
            window.sample_rate
        )));
    }
    let expected_scalars = window.sample_count as usize * window.channels as usize;
    let expected_bytes = expected_scalars * AudioSampleDtype::F32.bytes_per_sample();
    if window.interleaved_sample_bytes.len() != expected_bytes {
        return Err(Error::Runtime(format!(
            "{OPUS_ENCODER_PROCESSOR_NAME}: a window carries {} payload bytes, but {} samples in {} channels \
             of `f32` is {expected_bytes} — refused rather than reshaped into a plausible \
             wrong answer",
            window.interleaved_sample_bytes.len(),
            window.sample_count,
            window.channels,
        )));
    }
    Ok(window
        .interleaved_sample_bytes
        .chunks_exact(AudioSampleDtype::F32.bytes_per_sample())
        .map(|scalar| f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]]))
        .collect())
}

fn mint_encoder_for_window(
    config: &OpusEncoderConfig,
    window: &AudioBlock,
) -> Result<MintedOpusEncoder> {
    let layout =
        OpusStreamLayoutForSourceChannelCount::resolve(window.channels).map_err(|refusal| {
            Error::Runtime(format!(
                "{OPUS_ENCODER_PROCESSOR_NAME}: this window cannot be encoded — {refusal}"
            ))
        })?;
    let application = config
        .application
        .unwrap_or_default()
        .as_libopus_application();

    let (mut instance, streams) = match layout {
        OpusStreamLayoutForSourceChannelCount::MappingFamilyZeroSingleStream { channels } => (
            MintedOpusEncoderInstance::SingleStream(
                opus::Encoder::new(OPUS_SAMPLE_RATE_HZ, channels, application).map_err(
                    |failure| {
                        Error::Runtime(format!(
                            "{OPUS_ENCODER_PROCESSOR_NAME}: libopus refused a {} channel encoder: {failure}",
                            window.channels
                        ))
                    },
                )?,
            ),
            1usize,
        ),
        OpusStreamLayoutForSourceChannelCount::MappingFamilyOneMultistream(multistream) => (
            MintedOpusEncoderInstance::Multistream(
                opus::MSEncoder::new(
                    OPUS_SAMPLE_RATE_HZ,
                    multistream.streams,
                    multistream.coupled_streams,
                    multistream.vorbis_channel_order_mapping,
                    application,
                )
                .map_err(|failure| {
                    Error::Runtime(format!(
                        "{OPUS_ENCODER_PROCESSOR_NAME}: libopus refused a {} channel multistream encoder: \
                         {failure}",
                        window.channels
                    ))
                })?,
            ),
            usize::from(multistream.streams),
        ),
    };

    instance.apply_settings(config)?;
    let pre_skip = instance.lookahead_samples()?;

    tracing::info!(
        source_channels = window.channels,
        channel_mapping_family = layout.channel_mapping_family(),
        streams,
        pre_skip,
        application = ?config.application.unwrap_or_default(),
        bitrate_bps = ?config.bitrate_bps,
        "{OPUS_ENCODER_PROCESSOR_NAME}: minted the Opus encoder from the first window's channel count"
    );

    Ok(MintedOpusEncoder {
        instance,
        minted_for_source_channels: window.channels,
        pre_skip,
        highest_packet_bytes: streams * HIGHEST_PACKET_BYTES_PER_OPUS_STREAM
            + MULTISTREAM_PACKET_FRAMING_HEADROOM_BYTES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW_SAMPLE_COUNT: u32 = 960;

    /// One 20 ms window of a 440 Hz tone, the shape the port's window
    /// contract hands `process()`.
    fn a_window_of(channels: u32, first_sample_index: u32) -> AudioBlock {
        let mut interleaved_sample_bytes = Vec::new();
        for sample in 0..WINDOW_SAMPLE_COUNT {
            let phase = (first_sample_index + sample) as f32 * 440.0 * std::f32::consts::TAU
                / OPUS_SAMPLE_RATE_HZ as f32;
            for channel in 0..channels {
                let scalar = 0.5 * (phase + channel as f32 * 0.1).sin();
                interleaved_sample_bytes.extend_from_slice(&scalar.to_le_bytes());
            }
        }
        AudioBlock {
            interleaved_sample_bytes,
            sample_rate: OPUS_SAMPLE_RATE_HZ,
            channels,
            sample_count: WINDOW_SAMPLE_COUNT,
            dtype: AudioSampleDtype::F32,
            first_sample_timestamp_ns: 1_000,
        }
    }

    #[test]
    fn an_empty_config_deserialises_because_both_knobs_are_optional() {
        let from_empty_map: OpusEncoderConfig =
            serde_json::from_str("{}").expect("`{}` is a legal config");
        assert_eq!(from_empty_map, OpusEncoderConfig::default());
        assert_eq!(from_empty_map.bitrate_bps, None);
        assert_eq!(from_empty_map.application, None);
        assert_eq!(
            from_empty_map.application.unwrap_or_default(),
            OpusEncoderApplication::Audio,
            "absent means `audio`"
        );
    }

    #[test]
    fn the_application_spellings_are_the_three_the_wire_names() {
        for (wire, expected) in [
            ("audio", OpusEncoderApplication::Audio),
            ("voip", OpusEncoderApplication::Voip),
            ("lowdelay", OpusEncoderApplication::LowDelay),
        ] {
            let config: OpusEncoderConfig =
                serde_json::from_str(&format!(r#"{{"application":"{wire}"}}"#))
                    .expect("reads the wire spelling");
            assert_eq!(config.application, Some(expected));
        }
        assert!(
            serde_json::from_str::<OpusEncoderConfig>(r#"{"application":"surround"}"#).is_err(),
            "a tuning libopus does not have is refused rather than defaulted"
        );
    }

    #[test]
    fn the_encoder_mints_from_the_first_windows_channel_count() {
        for channels in 1..=HIGHEST_CHANNEL_COUNT_OPUS_PLACES {
            let mut encoder = AudioWindowToEncodedPacketEncoder::default();
            let packet = encoder
                .encode_one_window(&OpusEncoderConfig::default(), &a_window_of(channels, 0))
                .expect("encodes")
                .expect("publishes a packet");

            assert_eq!(
                packet.channels, channels,
                "the packet carries the source's count"
            );
            assert_eq!(packet.sample_rate, OPUS_SAMPLE_RATE_HZ);
            assert_eq!(packet.sample_count, WINDOW_SAMPLE_COUNT);
            assert_eq!(packet.codec, EncodedAudioCodec::Opus);
            assert!(
                packet.is_sync_point,
                "every Opus packet is a decode entry point"
            );
            assert!(!packet.opus_packet_bytes.is_empty());
        }
    }

    #[test]
    fn pre_skip_is_the_minted_encoders_own_lookahead_and_does_not_vary_with_the_channel_count() {
        let mut pre_skips = Vec::new();
        for channels in 1..=HIGHEST_CHANNEL_COUNT_OPUS_PLACES {
            let mut encoder = AudioWindowToEncodedPacketEncoder::default();
            let packet = encoder
                .encode_one_window(&OpusEncoderConfig::default(), &a_window_of(channels, 0))
                .expect("encodes")
                .expect("publishes a packet");
            pre_skips.push(packet.pre_skip);
        }
        // `OPUS_GET_LOOKAHEAD` is `Fs/400 + Fs/250` at every application but
        // `lowdelay`, so 48 kHz reports 312 whatever the channel count — the
        // property that makes a re-mint invisible to a decoder's trimming.
        assert!(
            pre_skips.iter().all(|reported| *reported == pre_skips[0]),
            "the lookahead is a function of the rate alone, got {pre_skips:?}"
        );
        assert_eq!(pre_skips[0], 312);
    }

    #[test]
    fn a_window_whose_channel_count_changes_re_mints_without_resetting_the_sequence() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let config = OpusEncoderConfig::default();

        let mut published = Vec::new();
        for (window_index, channels) in [2, 2, 6, 6, 1].into_iter().enumerate() {
            published.push(
                encoder
                    .encode_one_window(
                        &config,
                        &a_window_of(channels, window_index as u32 * WINDOW_SAMPLE_COUNT),
                    )
                    .expect("encodes across the re-mint")
                    .expect("publishes a packet"),
            );
        }

        assert_eq!(
            published.iter().map(|p| p.channels).collect::<Vec<_>>(),
            vec![2, 2, 6, 6, 1],
            "every packet declares the count its own window arrived with"
        );
        assert_eq!(
            published
                .iter()
                .map(|p| p.sequence_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4],
            "the counter belongs to the encoder, not the instance, so a re-mint is not a restart"
        );
        assert_eq!(
            published.iter().map(|p| p.group_index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4],
            "every Opus packet is its own group"
        );
    }

    #[test]
    fn a_channel_count_opus_cannot_place_is_refused_once_and_then_discarded() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let config = OpusEncoderConfig::default();

        let refusal = encoder
            .encode_one_window(&config, &a_window_of(9, 0))
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains('9'), "names the count: {refusal}");
        assert!(
            refusal.contains(OPUS_ENCODER_PROCESSOR_NAME),
            "names the processor: {refusal}"
        );

        // Latched: a `reactive` processor cannot reach `Error`, so the
        // alternative to discarding is the same refusal logged per dispatch
        // for the life of the run.
        for window_index in 1..4 {
            assert_eq!(
                encoder
                    .encode_one_window(&config, &a_window_of(9, window_index * WINDOW_SAMPLE_COUNT))
                    .expect("discards rather than refusing again"),
                None
            );
        }
        assert_eq!(
            encoder.packets_encoded(),
            0,
            "nothing was published, so nothing was counted"
        );
    }

    #[test]
    fn a_window_that_is_not_what_the_contract_promised_is_refused_rather_than_reinterpreted() {
        let config = OpusEncoderConfig::default();

        let mut short_payload = a_window_of(2, 0);
        short_payload.interleaved_sample_bytes.truncate(64);
        let refusal = AudioWindowToEncodedPacketEncoder::default()
            .encode_one_window(&config, &short_payload)
            .expect_err("refused")
            .to_string();
        assert!(
            refusal.contains("64"),
            "names the length it was handed: {refusal}"
        );

        let mut wrong_dtype = a_window_of(2, 0);
        wrong_dtype.dtype = AudioSampleDtype::I16;
        assert!(
            AudioWindowToEncodedPacketEncoder::default()
                .encode_one_window(&config, &wrong_dtype)
                .is_err(),
            "an `i16` window is refused, not read as `f32` bytes"
        );

        let mut wrong_rate = a_window_of(2, 0);
        wrong_rate.sample_rate = 44_100;
        let refusal = AudioWindowToEncodedPacketEncoder::default()
            .encode_one_window(&config, &wrong_rate)
            .expect_err("refused")
            .to_string();
        assert!(
            refusal.contains("44100"),
            "names the rate it was handed: {refusal}"
        );
    }

    #[test]
    fn a_declared_bitrate_reaches_libopus_and_bounds_the_packet() {
        let mut at_low_rate = AudioWindowToEncodedPacketEncoder::default();
        let mut at_high_rate = AudioWindowToEncodedPacketEncoder::default();

        let mut low_rate_bytes = 0usize;
        let mut high_rate_bytes = 0usize;
        for window_index in 0..10u32 {
            let window = a_window_of(2, window_index * WINDOW_SAMPLE_COUNT);
            low_rate_bytes += at_low_rate
                .encode_one_window(
                    &OpusEncoderConfig {
                        bitrate_bps: Some(24_000),
                        application: None,
                    },
                    &window,
                )
                .expect("encodes")
                .expect("publishes")
                .opus_packet_bytes
                .len();
            high_rate_bytes += at_high_rate
                .encode_one_window(
                    &OpusEncoderConfig {
                        bitrate_bps: Some(192_000),
                        application: None,
                    },
                    &window,
                )
                .expect("encodes")
                .expect("publishes")
                .opus_packet_bytes
                .len();
        }
        assert!(
            high_rate_bytes > low_rate_bytes * 2,
            "the declared bitrate reaches the encoder: 24 kbit/s gave {low_rate_bytes} bytes, \
             192 kbit/s gave {high_rate_bytes}"
        );
    }
}
