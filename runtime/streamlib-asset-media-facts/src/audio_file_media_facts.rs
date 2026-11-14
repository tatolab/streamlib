// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Audio files: the container probed, never decoded, for its first track's duration, sample
//! rate, channel count and codec.

use std::fs::File;
use std::path::Path;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetAudioFactsOnTheWire, AssetMediaFactsOnTheWire,
};
use symphonia::core::codecs::{
    CODEC_TYPE_AAC, CODEC_TYPE_ALAC, CODEC_TYPE_EAC3, CODEC_TYPE_FLAC, CODEC_TYPE_MP1,
    CODEC_TYPE_MP2, CODEC_TYPE_MP3, CODEC_TYPE_OPUS, CODEC_TYPE_VORBIS, CodecParameters, CodecType,
};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::{AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable};

/// Symphonia numbers every PCM codec from `0x100` and every ADPCM codec from `0x200`.
const SYMPHONIA_PCM_CODEC_TYPES: std::ops::Range<u32> = 0x100..0x200;
const SYMPHONIA_ADPCM_CODEC_TYPES: std::ops::Range<u32> = 0x200..0x300;

fn audio_codec_of(codec: CodecType) -> Option<&'static str> {
    Some(match codec {
        CODEC_TYPE_AAC => "aac",
        CODEC_TYPE_MP3 => "mp3",
        CODEC_TYPE_MP2 => "mp2",
        CODEC_TYPE_MP1 => "mp1",
        CODEC_TYPE_OPUS => "opus",
        CODEC_TYPE_VORBIS => "vorbis",
        CODEC_TYPE_FLAC => "flac",
        CODEC_TYPE_ALAC => "alac",
        CODEC_TYPE_EAC3 => "eac3",
        other => match symphonia_codec_number_of(other)? {
            number if SYMPHONIA_PCM_CODEC_TYPES.contains(&number) => "pcm",
            number if SYMPHONIA_ADPCM_CODEC_TYPES.contains(&number) => "adpcm",
            _ => return None,
        },
    })
}

/// A codec's number, which symphonia keeps private and displays as `0x`-prefixed hex.
fn symphonia_codec_number_of(codec: CodecType) -> Option<u32> {
    u32::from_str_radix(codec.to_string().strip_prefix("0x")?, 16).ok()
}

fn duration_ms_of(codec_parameters: &CodecParameters) -> Option<u64> {
    let frame_count = codec_parameters.n_frames?;
    let (seconds_per_tick_numerator, seconds_per_tick_denominator) =
        match codec_parameters.time_base {
            Some(time_base) => (time_base.numer, time_base.denom),
            None => (1, codec_parameters.sample_rate?),
        };
    if seconds_per_tick_denominator == 0 {
        return None;
    }
    u64::try_from(
        u128::from(frame_count) * u128::from(seconds_per_tick_numerator) * 1_000
            / u128::from(seconds_per_tick_denominator),
    )
    .ok()
}

pub(crate) fn read_an_audio_file(
    audio_file_path: &Path,
    extension: &str,
) -> Result<AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable> {
    let file = File::open(audio_file_path).map_err(|why| {
        AssetMediaFactsUnreadable::because(format!("the file could not be opened: {why}"))
    })?;
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|why| {
            AssetMediaFactsUnreadable::because(format!("the audio container did not read: {why}"))
        })?;
    let Some(track) = probed.format.default_track() else {
        return Err(AssetMediaFactsUnreadable::because(
            "the audio container holds no track",
        ));
    };
    let codec_parameters = &track.codec_params;
    Ok(AssetMediaFactsReadFromTheFile {
        media_facts: AssetMediaFactsOnTheWire {
            duration_ms: duration_ms_of(codec_parameters),
            audio: Some(AssetAudioFactsOnTheWire {
                sample_rate_hz: codec_parameters.sample_rate,
                channel_count: codec_parameters
                    .channels
                    .map(|channels| channels.count() as u32),
                codec: audio_codec_of(codec_parameters.codec).map(str::to_owned),
            }),
            ..AssetMediaFactsOnTheWire::default()
        },
        preview_picture: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetMediaFactsReadersOfThisRuntime, read_the_media_facts_of_an_asset_file};

    /// A 16-bit PCM WAV of `seconds` of silence.
    fn a_wav_of_silence(sample_rate_hz: u32, channel_count: u16, seconds: u32) -> Vec<u8> {
        let block_align = channel_count * 2;
        let data_len = sample_rate_hz * u32::from(block_align) * seconds;
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channel_count.to_le_bytes());
        wav.extend_from_slice(&sample_rate_hz.to_le_bytes());
        wav.extend_from_slice(&(sample_rate_hz * u32::from(block_align)).to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.resize(wav.len() + data_len as usize, 0);
        wav
    }

    #[test]
    fn a_wav_reads_its_duration_sample_rate_channels_and_codec() {
        let directory = tempfile::tempdir().unwrap();
        let wav = directory.path().join("tone.wav");
        std::fs::write(&wav, a_wav_of_silence(48_000, 2, 3)).unwrap();
        let read = read_the_media_facts_of_an_asset_file(
            &wav,
            &AssetMediaFactsReadersOfThisRuntime::default(),
        )
        .unwrap();
        assert_eq!(read.media_facts.duration_ms, Some(3_000));
        assert_eq!(
            read.media_facts.audio,
            Some(AssetAudioFactsOnTheWire {
                sample_rate_hz: Some(48_000),
                channel_count: Some(2),
                codec: Some("pcm".to_owned()),
            })
        );
        assert_eq!(read.media_facts.video, None);
        assert_eq!(read.preview_picture, None);
    }

    #[test]
    fn a_file_that_is_no_audio_is_unreadable() {
        let directory = tempfile::tempdir().unwrap();
        let mp3 = directory.path().join("noise.mp3");
        std::fs::write(&mp3, b"not an mp3 at all").unwrap();
        let refused = read_the_media_facts_of_an_asset_file(
            &mp3,
            &AssetMediaFactsReadersOfThisRuntime::default(),
        )
        .unwrap_err();
        assert!(
            refused.0.starts_with("the audio container did not read"),
            "{refused}"
        );
    }
}
