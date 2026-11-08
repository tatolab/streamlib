// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `OpusEncoder` body: one 20 ms audio window in, one Opus media frame
//! out, and the port's Packets stream facts — the `OpusHead` hex-encoded as
//! its codec configuration — owed whenever an encoder is minted.
//!
//! The encoder is minted from the first window's channel count, because the
//! input port declares no channel count and so follows its source. A window
//! whose count changes re-mints: libopus fixes an instance's channel count at
//! construction and offers no ctl that changes it. `OPUS_SET_FORCE_CHANNELS`
//! moves the *coded* mono/stereo of an existing instance, is bounded by that
//! instance's own count, and has no mapping-family-1 equivalent.
//!
//! A re-mint costs continuity, not decodability: a fresh instance has lost its
//! prediction state, at the seam where the window stage already flushed its
//! accumulator and resampler. The new count reaches a decoder through the
//! port's stream facts.

use crate::audio_window::{AudioSampleDtype, AudioWindowToEncode};
use crate::hang_legacy_container_media_frame::hang_legacy_container_media_frame_bytes;
use crate::opus_codec_refusal::{OpusCodecNotice, OpusCodecRefusal, Result};
use crate::opus_identification_header::OpusIdentificationHeader;
#[cfg(test)]
use crate::opus_stream_layout::HIGHEST_CHANNEL_COUNT_OPUS_PLACES;
use crate::opus_stream_layout::OpusStreamLayoutForSourceChannelCount;

/// The codec Opus stream facts name, as RFC 6381 spells it.
pub const OPUS_RFC6381_CODEC_STRING: &str = "opus";

/// The node's class name, which every refusal and notice names itself by.
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

/// Which libopus tuning an [`OpusEncoderConfig`] asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpusEncoderApplication {
    /// Broadcast and high fidelity: the decoded audio should be as close as
    /// possible to the input.
    #[default]
    Audio,
    /// Speech: listening quality and intelligibility matter most.
    Voip,
    /// Lowest achievable latency, at a cost in quality.
    LowDelay,
}

impl OpusEncoderApplication {
    /// Every tuning by the name a node's config spells it.
    pub const BY_CONFIG_SPELLING: [(&'static str, Self); 3] = [
        ("audio", Self::Audio),
        ("voip", Self::Voip),
        ("lowdelay", Self::LowDelay),
    ];

    /// The tuning `spelling` names, refused by name when libopus has none.
    pub fn from_config_spelling(spelling: &str) -> Result<Self> {
        Self::BY_CONFIG_SPELLING
            .iter()
            .find(|(config_spelling, _)| *config_spelling == spelling)
            .map(|(_, application)| *application)
            .ok_or_else(|| {
                OpusCodecRefusal(format!(
                    "{OPUS_ENCODER_PROCESSOR_NAME}: `application` is {spelling:?}, which libopus \
                     has no tuning for — one of `audio`, `voip`, `lowdelay`"
                ))
            })
    }

    fn as_libopus_application(self) -> opus::Application {
        match self {
            Self::Audio => opus::Application::Audio,
            Self::Voip => opus::Application::Voip,
            Self::LowDelay => opus::Application::LowDelay,
        }
    }
}

/// What an `OpusEncoder` is configured with. The channel count follows the
/// source and the rate and framing are the port's window contract, so what is
/// left is the bitrate and the tuning.
///
/// In-band FEC and DTX are off and are not knobs. DTX replaces silence with
/// nothing — a gap that must stay derivable from the stamps rather than be
/// invented back by a decoder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpusEncoderConfig {
    /// Target bitrate in bits per second; absent, libopus picks its own from
    /// the sample rate and channel count.
    pub bitrate_bps: Option<u32>,
    pub application: OpusEncoderApplication,
}

/// The Packets stream facts an encoder port carries for the instance minted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusEncoderPortStreamFacts {
    pub channel_count: u32,
    pub sample_rate: u32,
    /// The `OpusHead`, hex-encoded.
    pub codec_configuration_hex: String,
}

/// Dispatch one expression over either shape of instance: the two libopus
/// wrappers share every ctl name but no trait.
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
        .map_err(|failure| {
            OpusCodecRefusal(format!(
                "{OPUS_ENCODER_PROCESSOR_NAME}: libopus refused the window: {failure}"
            ))
        })
    }

    fn lookahead_samples(&mut self) -> Result<u32> {
        let lookahead = on_either_encoder_instance!(self, |encoder| encoder.get_lookahead())
            .map_err(|failure| {
                OpusCodecRefusal(format!(
                    "{OPUS_ENCODER_PROCESSOR_NAME}: libopus would not report its lookahead: \
                     {failure}"
                ))
            })?;
        u32::try_from(lookahead).map_err(|_| {
            OpusCodecRefusal(format!(
                "{OPUS_ENCODER_PROCESSOR_NAME}: libopus reported a negative lookahead of \
                 {lookahead}"
            ))
        })
    }

    /// Everything the config and the doctrine fix, applied to a freshly
    /// minted instance.
    fn apply_settings(&mut self, config: &OpusEncoderConfig) -> Result<()> {
        let named = |what: &str, failure: opus::Error| {
            OpusCodecRefusal(format!(
                "{OPUS_ENCODER_PROCESSOR_NAME}: libopus refused {what}: {failure}"
            ))
        };
        if let Some(bitrate_bps) = config.bitrate_bps {
            let bitrate_bps = i32::try_from(bitrate_bps).map_err(|_| {
                OpusCodecRefusal(format!(
                    "{OPUS_ENCODER_PROCESSOR_NAME}: a bitrate of {bitrate_bps} bit/s is past what \
                     libopus takes"
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

/// The encoder minted for one source channel count.
struct MintedOpusEncoder {
    instance: MintedOpusEncoderInstance,
    /// The count this instance was constructed for. A window arriving with
    /// any other count re-mints, because libopus cannot be told a new one.
    minted_for_source_channels: u32,
    /// Output buffer size, from the stream count the layout resolved to.
    highest_packet_bytes: usize,
}

/// Encodes 20 ms audio windows into Opus media frames.
#[derive(Default)]
pub struct AudioWindowToEncodedPacketEncoder {
    config: OpusEncoderConfig,
    minted: Option<MintedOpusEncoder>,
    /// The entry a mint derived that the port does not carry yet.
    stream_facts_owed_to_the_port: Option<OpusEncoderPortStreamFacts>,
    notices: Vec<OpusCodecNotice>,
    packets_encoded: u64,
    /// The channel count whose layout Opus cannot place, once refused.
    ///
    /// Keyed on the count rather than latched run-wide, because the window
    /// stage flushes and re-anchors when a source changes its count, so a run
    /// that briefly carried nine channels and returned to stereo must encode
    /// again.
    channel_count_already_refused: Option<u32>,
    /// Latched once a mint has failed for a reason no later window can
    /// change — a bitrate libopus rejects, or libopus declining an encoder for
    /// a layout it had agreed. Every later window is discarded, so the refusal
    /// is said once instead of per dispatch.
    mint_already_failed: bool,
}

impl AudioWindowToEncodedPacketEncoder {
    /// An encoder that mints its libopus instance from the first window it is handed.
    pub fn new(config: OpusEncoderConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Encode one window into one Opus media frame stamped at its first
    /// sample, or hand back `None` when this encoder has already refused its
    /// source and is discarding what follows.
    pub fn encode_one_window(
        &mut self,
        window: &AudioWindowToEncode<'_>,
    ) -> Result<Option<Vec<u8>>> {
        if self.mint_already_failed || self.channel_count_already_refused == Some(window.channels) {
            return Ok(None);
        }
        let interleaved_samples = read_window_as_interleaved_f32(window)?;
        let mut media_frame =
            hang_legacy_container_media_frame_bytes(window.first_sample_timestamp_ns, &[])
                .map_err(|refusal| {
                    OpusCodecRefusal(format!("{OPUS_ENCODER_PROCESSOR_NAME}: {refusal}"))
                })?;

        if let Some(stale) = self
            .minted
            .take_if(|minted| minted.minted_for_source_channels != window.channels)
        {
            self.notices.push(OpusCodecNotice::info(format!(
                "{OPUS_ENCODER_PROCESSOR_NAME}: the source's channel count changed from {} to {} \
                 — re-minting the encoder",
                stale.minted_for_source_channels, window.channels
            )));
        }

        let minted = match &mut self.minted {
            Some(minted) => minted,
            empty_slot => {
                // Resolved before the mint so the two failures stay
                // distinguishable: a count Opus cannot place is this window's
                // problem, everything after it is the run's.
                let layout = match OpusStreamLayoutForSourceChannelCount::resolve(window.channels) {
                    Ok(layout) => layout,
                    Err(refusal) => {
                        self.channel_count_already_refused = Some(window.channels);
                        return Err(OpusCodecRefusal(format!(
                            "{OPUS_ENCODER_PROCESSOR_NAME}: this window cannot be encoded, and \
                             windows are discarded until the source's channel count changes — \
                             {refusal}"
                        )));
                    }
                };
                let minted_and_its_entry = mint_encoder_for_window(&self.config, window, layout)
                    .and_then(|(minted, pre_skip, notice)| {
                        Ok((minted, stream_facts_for(window.channels, pre_skip)?, notice))
                    });
                match minted_and_its_entry {
                    Ok((minted, stream_facts, notice)) => {
                        self.notices.push(notice);
                        self.stream_facts_owed_to_the_port = Some(stream_facts);
                        empty_slot.insert(minted)
                    }
                    Err(mint_failure) => {
                        self.mint_already_failed = true;
                        return Err(OpusCodecRefusal(format!(
                            "{mint_failure}; every later window is discarded"
                        )));
                    }
                }
            }
        };

        let opus_packet_bytes = minted
            .instance
            .encode_interleaved_f32(&interleaved_samples, minted.highest_packet_bytes)?;
        media_frame.extend_from_slice(&opus_packet_bytes);
        self.packets_encoded += 1;
        Ok(Some(media_frame))
    }

    /// The stream facts the port does not carry yet, owed by the last mint.
    pub fn take_stream_facts_owed_to_the_port(&mut self) -> Option<OpusEncoderPortStreamFacts> {
        self.stream_facts_owed_to_the_port.take()
    }

    /// What the encoder did since the last call that an operator reads.
    pub fn take_notices(&mut self) -> Vec<OpusCodecNotice> {
        std::mem::take(&mut self.notices)
    }

    /// How many packets this encoder has encoded, for the teardown line.
    pub fn packets_encoded(&self) -> u64 {
        self.packets_encoded
    }
}

/// The stream facts an encoder of `channels` with this lookahead publishes.
fn stream_facts_for(channels: u32, pre_skip: u32) -> Result<OpusEncoderPortStreamFacts> {
    let codec_configuration_hex = OpusIdentificationHeader::hex_encoded_for_encoder(
        channels, pre_skip,
    )
    .map_err(|refusal| OpusCodecRefusal(format!("{OPUS_ENCODER_PROCESSOR_NAME}: {refusal}")))?;
    Ok(OpusEncoderPortStreamFacts {
        channel_count: channels,
        sample_rate: OPUS_SAMPLE_RATE_HZ,
        codec_configuration_hex,
    })
}

/// Read a window's payload as interleaved `f32`, refusing by name a block
/// that is not what the port's window contract promised rather than
/// reinterpreting its bytes.
fn read_window_as_interleaved_f32(window: &AudioWindowToEncode<'_>) -> Result<Vec<f32>> {
    if window.dtype != AudioSampleDtype::F32 {
        return Err(OpusCodecRefusal(format!(
            "{OPUS_ENCODER_PROCESSOR_NAME}: a window arrived as {:?}, but the port's \
             `audio_window` contract declares `f32` — the stage converts, so this is a block \
             that did not come through it",
            window.dtype
        )));
    }
    if window.sample_rate != OPUS_SAMPLE_RATE_HZ {
        return Err(OpusCodecRefusal(format!(
            "{OPUS_ENCODER_PROCESSOR_NAME}: a window arrived at {} Hz, but Opus codes at \
             {OPUS_SAMPLE_RATE_HZ} Hz and the port's `audio_window` contract resamples to it",
            window.sample_rate
        )));
    }
    let expected_scalars = window.sample_count as usize * window.channels as usize;
    let expected_bytes = expected_scalars * AudioSampleDtype::F32.bytes_per_sample();
    if window.interleaved_sample_bytes.len() != expected_bytes {
        return Err(OpusCodecRefusal(format!(
            "{OPUS_ENCODER_PROCESSOR_NAME}: a window carries {} payload bytes, but {} samples in \
             {} channels of `f32` is {expected_bytes} — refused rather than reshaped into a \
             plausible wrong answer",
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

/// Mint the instance a window's layout resolves to, with its lookahead and
/// the notice saying what was minted.
fn mint_encoder_for_window(
    config: &OpusEncoderConfig,
    window: &AudioWindowToEncode<'_>,
    layout: OpusStreamLayoutForSourceChannelCount,
) -> Result<(MintedOpusEncoder, u32, OpusCodecNotice)> {
    let application = config.application.as_libopus_application();
    let refused_by_libopus = |shape: &str, failure: opus::Error| {
        OpusCodecRefusal(format!(
            "{OPUS_ENCODER_PROCESSOR_NAME}: libopus refused a {} channel {shape}: {failure}",
            window.channels
        ))
    };

    let (mut instance, streams) = match layout {
        OpusStreamLayoutForSourceChannelCount::MappingFamilyZeroSingleStream { channels } => (
            MintedOpusEncoderInstance::SingleStream(
                opus::Encoder::new(OPUS_SAMPLE_RATE_HZ, channels, application)
                    .map_err(|failure| refused_by_libopus("encoder", failure))?,
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
                .map_err(|failure| refused_by_libopus("multistream encoder", failure))?,
            ),
            usize::from(multistream.streams),
        ),
    };

    instance.apply_settings(config)?;
    let pre_skip = instance.lookahead_samples()?;

    let notice = OpusCodecNotice::info(format!(
        "{OPUS_ENCODER_PROCESSOR_NAME}: minted the Opus encoder from the window's channel count — \
         source_channels={} channel_mapping_family={} streams={streams} pre_skip={pre_skip} \
         application={:?} bitrate_bps={:?}",
        window.channels,
        layout.channel_mapping_family(),
        config.application,
        config.bitrate_bps,
    ));

    Ok((
        MintedOpusEncoder {
            instance,
            minted_for_source_channels: window.channels,
            highest_packet_bytes: streams * HIGHEST_PACKET_BYTES_PER_OPUS_STREAM
                + MULTISTREAM_PACKET_FRAMING_HEADROOM_BYTES,
        },
        pre_skip,
        notice,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hang_legacy_container_media_frame::read_hang_legacy_container_media_frame;

    const WINDOW_SAMPLE_COUNT: u32 = 960;

    /// One 20 ms window of a 440 Hz tone's samples, in the shape the port's
    /// window contract hands `process()`.
    struct OwnedAudioWindow {
        interleaved_sample_bytes: Vec<u8>,
        sample_rate: u32,
        channels: u32,
        dtype: AudioSampleDtype,
        first_sample_timestamp_ns: i64,
    }

    impl OwnedAudioWindow {
        fn of(channels: u32, first_sample_index: u32) -> Self {
            let mut interleaved_sample_bytes = Vec::new();
            for sample in 0..WINDOW_SAMPLE_COUNT {
                let phase = (first_sample_index + sample) as f32 * 440.0 * std::f32::consts::TAU
                    / OPUS_SAMPLE_RATE_HZ as f32;
                for channel in 0..channels {
                    let scalar = 0.5 * (phase + channel as f32 * 0.1).sin();
                    interleaved_sample_bytes.extend_from_slice(&scalar.to_le_bytes());
                }
            }
            Self {
                interleaved_sample_bytes,
                sample_rate: OPUS_SAMPLE_RATE_HZ,
                channels,
                dtype: AudioSampleDtype::F32,
                first_sample_timestamp_ns: 1_000,
            }
        }

        fn borrowed(&self) -> AudioWindowToEncode<'_> {
            AudioWindowToEncode {
                interleaved_sample_bytes: &self.interleaved_sample_bytes,
                sample_rate: self.sample_rate,
                channels: self.channels,
                sample_count: WINDOW_SAMPLE_COUNT,
                dtype: self.dtype,
                first_sample_timestamp_ns: self.first_sample_timestamp_ns,
            }
        }
    }

    fn encode(
        encoder: &mut AudioWindowToEncodedPacketEncoder,
        window: &OwnedAudioWindow,
    ) -> Result<Option<Vec<u8>>> {
        encoder.encode_one_window(&window.borrowed())
    }

    #[test]
    fn the_application_spellings_are_the_three_libopus_has() {
        for (spelling, expected) in [
            ("audio", OpusEncoderApplication::Audio),
            ("voip", OpusEncoderApplication::Voip),
            ("lowdelay", OpusEncoderApplication::LowDelay),
        ] {
            assert_eq!(
                OpusEncoderApplication::from_config_spelling(spelling),
                Ok(expected)
            );
        }
        let refusal = OpusEncoderApplication::from_config_spelling("surround")
            .expect_err("a tuning libopus does not have is refused rather than defaulted")
            .to_string();
        assert!(refusal.contains("surround"), "{refusal}");
        assert_eq!(
            OpusEncoderApplication::default(),
            OpusEncoderApplication::Audio
        );
    }

    fn opus_identification_header_of(
        stream_facts: &OpusEncoderPortStreamFacts,
    ) -> OpusIdentificationHeader {
        OpusIdentificationHeader::parse_hex_encoded(&stream_facts.codec_configuration_hex)
            .expect("the OpusHead reads back")
    }

    #[test]
    fn a_window_leaves_as_a_media_frame_stamped_at_its_first_sample() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let media_frame = encode(&mut encoder, &OwnedAudioWindow::of(2, 0))
            .expect("encodes")
            .expect("publishes a media frame");
        let read = read_hang_legacy_container_media_frame(&media_frame).expect("a media frame");
        assert_eq!(read.timestamp_microseconds, 1);
        assert!(!read.codec_payload_bytes.is_empty());
    }

    #[test]
    fn a_window_stamped_before_zero_is_refused_before_it_mints_or_encodes() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();
        let mut stamped_before_zero = OwnedAudioWindow::of(2, 0);
        stamped_before_zero.first_sample_timestamp_ns = -1_000;

        let refusal = encode(&mut encoder, &stamped_before_zero)
            .expect_err("a timestamp the container cannot carry is refused")
            .to_string();

        assert!(refusal.contains("-1000"), "{refusal}");
        assert!(encoder.take_stream_facts_owed_to_the_port().is_none());
        assert_eq!(encoder.packets_encoded(), 0);
        assert!(encoder.minted.is_none());
    }

    #[test]
    fn the_encoder_mints_from_the_first_windows_channel_count_and_owes_its_entry() {
        for channels in 1..=HIGHEST_CHANNEL_COUNT_OPUS_PLACES {
            let mut encoder = AudioWindowToEncodedPacketEncoder::default();
            encode(&mut encoder, &OwnedAudioWindow::of(channels, 0))
                .expect("encodes")
                .expect("publishes a media frame");

            let stream_facts = encoder
                .take_stream_facts_owed_to_the_port()
                .expect("the first packet's mint owes the port its entry");
            assert_eq!(stream_facts.sample_rate, OPUS_SAMPLE_RATE_HZ);
            assert_eq!(stream_facts.channel_count, channels);
            assert_eq!(
                opus_identification_header_of(&stream_facts).channels,
                channels
            );
            assert_eq!(encoder.take_stream_facts_owed_to_the_port(), None);
            let notices = encoder.take_notices();
            assert_eq!(notices.len(), 1, "the mint is said once: {notices:?}");
            assert!(notices[0].message.contains("minted"), "{notices:?}");
        }
    }

    #[test]
    fn pre_skip_is_the_minted_encoders_own_lookahead_and_does_not_vary_with_the_channel_count() {
        let mut pre_skips = Vec::new();
        for channels in 1..=HIGHEST_CHANNEL_COUNT_OPUS_PLACES {
            let mut encoder = AudioWindowToEncodedPacketEncoder::default();
            encode(&mut encoder, &OwnedAudioWindow::of(channels, 0))
                .expect("encodes")
                .expect("publishes a media frame");
            let stream_facts = encoder.take_stream_facts_owed_to_the_port().unwrap();
            pre_skips.push(opus_identification_header_of(&stream_facts).pre_skip);
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
    fn a_window_whose_channel_count_changes_re_mints_and_owes_the_port_a_new_entry() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();

        let mut entries_owed = Vec::new();
        for (window_index, channels) in [2, 2, 6, 6, 1].into_iter().enumerate() {
            encode(
                &mut encoder,
                &OwnedAudioWindow::of(channels, window_index as u32 * WINDOW_SAMPLE_COUNT),
            )
            .expect("encodes across the re-mint")
            .expect("publishes a media frame");
            entries_owed.push(
                encoder
                    .take_stream_facts_owed_to_the_port()
                    .map(|stream_facts| stream_facts.channel_count),
            );
        }

        assert_eq!(
            entries_owed,
            vec![Some(2), None, Some(6), None, Some(1)],
            "each mint owes the entry naming its own count, and only a mint does"
        );
        assert_eq!(encoder.packets_encoded(), 5);
        let re_mints = encoder
            .take_notices()
            .into_iter()
            .filter(|notice| notice.message.contains("re-minting"))
            .count();
        assert_eq!(re_mints, 2);
    }

    #[test]
    fn a_channel_count_opus_cannot_place_is_refused_once_and_then_discarded() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();

        let refusal = encode(&mut encoder, &OwnedAudioWindow::of(9, 0))
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains('9'), "names the count: {refusal}");
        assert!(
            refusal.contains(OPUS_ENCODER_PROCESSOR_NAME),
            "names the processor: {refusal}"
        );

        // Latched: the alternative to discarding is the same refusal logged
        // per dispatch for the life of the run.
        for window_index in 1..4 {
            assert_eq!(
                encode(
                    &mut encoder,
                    &OwnedAudioWindow::of(9, window_index * WINDOW_SAMPLE_COUNT)
                )
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

    /// The window stage flushes and re-anchors when a source changes its
    /// channel count, so a run that briefly carried a count Opus cannot place
    /// and returned to a supported one must encode again.
    #[test]
    fn a_count_opus_cannot_place_does_not_silence_the_encoder_once_the_source_returns() {
        let mut encoder = AudioWindowToEncodedPacketEncoder::default();

        encode(&mut encoder, &OwnedAudioWindow::of(9, 0)).expect_err("nine channels are refused");
        assert_eq!(
            encode(&mut encoder, &OwnedAudioWindow::of(9, WINDOW_SAMPLE_COUNT))
                .expect("the same count is discarded rather than refused again"),
            None
        );

        encode(
            &mut encoder,
            &OwnedAudioWindow::of(2, 2 * WINDOW_SAMPLE_COUNT),
        )
        .expect("a supported count mints")
        .expect("and publishes");
        assert_eq!(
            encoder
                .take_stream_facts_owed_to_the_port()
                .map(|stream_facts| stream_facts.channel_count),
            Some(2)
        );
        assert_eq!(encoder.packets_encoded(), 1);

        assert_eq!(
            encode(
                &mut encoder,
                &OwnedAudioWindow::of(9, 3 * WINDOW_SAMPLE_COUNT)
            )
            .expect("still discarded"),
            None
        );
        encode(
            &mut encoder,
            &OwnedAudioWindow::of(2, 4 * WINDOW_SAMPLE_COUNT),
        )
        .expect("stereo still encodes")
        .expect("and publishes");
        assert_eq!(encoder.packets_encoded(), 2);
    }

    #[test]
    fn a_window_that_is_not_what_the_contract_promised_is_refused_rather_than_reinterpreted() {
        let mut short_payload = OwnedAudioWindow::of(2, 0);
        short_payload.interleaved_sample_bytes.truncate(64);
        let refusal = encode(
            &mut AudioWindowToEncodedPacketEncoder::default(),
            &short_payload,
        )
        .expect_err("refused")
        .to_string();
        assert!(
            refusal.contains("64"),
            "names the length it was handed: {refusal}"
        );

        let mut wrong_dtype = OwnedAudioWindow::of(2, 0);
        wrong_dtype.dtype = AudioSampleDtype::I16;
        assert!(
            encode(
                &mut AudioWindowToEncodedPacketEncoder::default(),
                &wrong_dtype
            )
            .is_err(),
            "an `i16` window is refused, not read as `f32` bytes"
        );

        let mut wrong_rate = OwnedAudioWindow::of(2, 0);
        wrong_rate.sample_rate = 44_100;
        let refusal = encode(
            &mut AudioWindowToEncodedPacketEncoder::default(),
            &wrong_rate,
        )
        .expect_err("refused")
        .to_string();
        assert!(
            refusal.contains("44100"),
            "names the rate it was handed: {refusal}"
        );
    }

    #[test]
    fn a_declared_bitrate_reaches_libopus_and_bounds_the_packet() {
        let at_bitrate = |bitrate_bps| {
            AudioWindowToEncodedPacketEncoder::new(OpusEncoderConfig {
                bitrate_bps: Some(bitrate_bps),
                application: OpusEncoderApplication::Audio,
            })
        };
        let mut at_low_rate = at_bitrate(24_000);
        let mut at_high_rate = at_bitrate(192_000);

        let mut low_rate_bytes = 0usize;
        let mut high_rate_bytes = 0usize;
        for window_index in 0..10u32 {
            let window = OwnedAudioWindow::of(2, window_index * WINDOW_SAMPLE_COUNT);
            low_rate_bytes += encode(&mut at_low_rate, &window)
                .expect("encodes")
                .expect("publishes")
                .len();
            high_rate_bytes += encode(&mut at_high_rate, &window)
                .expect("encodes")
                .expect("publishes")
                .len();
        }
        assert!(
            high_rate_bytes > low_rate_bytes * 2,
            "the declared bitrate reaches the encoder: 24 kbit/s gave {low_rate_bytes} bytes, \
             192 kbit/s gave {high_rate_bytes}"
        );
    }
}
