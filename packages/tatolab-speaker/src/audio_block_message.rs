// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio-block message a block to play arrives as.

use serde::Deserialize;
use tatolab_module_sdk::{ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat};

/// The audio-block wire map (`docs/plan/ARCHITECTURE.md` §Media I/O): a msgpack
/// named map, `samples` as `bin`, little-endian interleaved scalars read by
/// `dtype`. Keys this speaker does not play from are ignored.
#[derive(Deserialize)]
struct AudioBlockWireMap {
    #[serde(with = "serde_bytes")]
    samples: Vec<u8>,
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    dtype: String,
}

/// How a stream's scalar encoding is spelled on the wire.
fn wire_dtype_of(sample_format: ModuleHostAudioSampleFormat) -> &'static str {
    match sample_format {
        ModuleHostAudioSampleFormat::F32 => "f32",
        ModuleHostAudioSampleFormat::I16 => "i16",
    }
}

/// The interleaved samples one message carries, once it is known to be in the
/// format the playback stream plays; why not, otherwise.
pub(crate) fn interleaved_sample_bytes_to_play(
    payload: &[u8],
    playback_stream_format: ModuleHostAudioStreamFormat,
) -> Result<Vec<u8>, String> {
    let block: AudioBlockWireMap = rmp_serde::from_slice(payload)
        .map_err(|decode_failure| format!("not an audio-block message: {decode_failure}"))?;
    let playback_dtype = wire_dtype_of(playback_stream_format.sample_format);
    if block.sample_rate != playback_stream_format.sample_rate
        || block.channels != playback_stream_format.channels
        || block.dtype != playback_dtype
    {
        return Err(format!(
            "a block at {} Hz × {} ch {} reached a stream playing {} Hz × {} ch {}; the `audio` \
             port's window converts what a link carries, so this block bypassed it",
            block.sample_rate,
            block.channels,
            block.dtype,
            playback_stream_format.sample_rate,
            playback_stream_format.channels,
            playback_dtype
        ));
    }
    let expected_byte_count = block.sample_count as usize
        * block.channels as usize
        * playback_stream_format.sample_format.bytes_per_sample();
    if block.samples.len() != expected_byte_count {
        return Err(format!(
            "a block of {} per-channel samples carries {} bytes where {expected_byte_count} \
             were due",
            block.sample_count,
            block.samples.len()
        ));
    }
    Ok(block.samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYING_48_KHZ_STEREO_F32: ModuleHostAudioStreamFormat = ModuleHostAudioStreamFormat {
        sample_rate: 48_000,
        channels: 2,
        sample_format: ModuleHostAudioSampleFormat::F32,
    };

    #[derive(serde::Serialize)]
    struct WrittenAudioBlock<'samples> {
        #[serde(with = "serde_bytes")]
        samples: &'samples [u8],
        sample_rate: u32,
        channels: u32,
        sample_count: u32,
        dtype: &'static str,
        first_sample_timestamp_ns: i64,
    }

    fn an_audio_block_message(
        samples: &[u8],
        sample_rate: u32,
        channels: u32,
        sample_count: u32,
        dtype: &'static str,
    ) -> Vec<u8> {
        rmp_serde::to_vec_named(&WrittenAudioBlock {
            samples,
            sample_rate,
            channels,
            sample_count,
            dtype,
            first_sample_timestamp_ns: 9,
        })
        .unwrap()
    }

    #[test]
    fn a_block_in_the_streams_format_hands_over_its_samples_as_written() {
        let samples: Vec<u8> = [0.25f32, -0.5]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(
            interleaved_sample_bytes_to_play(
                &an_audio_block_message(&samples, 48_000, 2, 1, "f32"),
                PLAYING_48_KHZ_STEREO_F32
            ),
            Ok(samples)
        );
    }

    #[test]
    fn a_block_in_another_format_is_refused_naming_both_formats() {
        let refusal = interleaved_sample_bytes_to_play(
            &an_audio_block_message(&[0, 0], 16_000, 1, 1, "i16"),
            PLAYING_48_KHZ_STEREO_F32,
        )
        .unwrap_err();
        assert!(refusal.contains("16000 Hz × 1 ch i16"), "{refusal}");
        assert!(refusal.contains("48000 Hz × 2 ch f32"), "{refusal}");
    }

    #[test]
    fn a_block_whose_samples_disagree_with_its_count_is_refused() {
        let refusal = interleaved_sample_bytes_to_play(
            &an_audio_block_message(&[0; 4], 48_000, 2, 1, "f32"),
            PLAYING_48_KHZ_STEREO_F32,
        )
        .unwrap_err();
        assert!(refusal.contains("8 were due"), "{refusal}");
    }

    #[test]
    fn a_payload_that_is_not_an_audio_block_is_refused() {
        let refusal = interleaved_sample_bytes_to_play(b"not msgpack", PLAYING_48_KHZ_STEREO_F32)
            .unwrap_err();
        assert!(
            refusal.starts_with("not an audio-block message"),
            "{refusal}"
        );
    }
}
