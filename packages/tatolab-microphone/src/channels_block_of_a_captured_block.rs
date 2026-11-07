// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Channels block a captured block is published as: planar f32 samples,
//! an i16 capture stream widened to f32.

use tatolab_module_sdk::{
    ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat, ModuleHostChannelsBlock,
    ModuleHostChannelsBlockSampleType,
};

/// One captured block as a Channels block descriptor and its planar payload.
pub(crate) struct CapturedChannelsBlock {
    pub(crate) block: ModuleHostChannelsBlock,
    pub(crate) planar_payload: Vec<u8>,
}

/// The captured block's interleaved samples, in the capture stream's format,
/// as f32.
fn interleaved_f32_samples_of_captured_bytes(
    interleaved_sample_bytes: &[u8],
    sample_format: ModuleHostAudioSampleFormat,
) -> Vec<f32> {
    match sample_format {
        ModuleHostAudioSampleFormat::F32 => {
            let (samples, _) = interleaved_sample_bytes.as_chunks::<4>();
            samples
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect()
        }
        ModuleHostAudioSampleFormat::I16 => {
            let (samples, _) = interleaved_sample_bytes.as_chunks::<2>();
            samples
                .iter()
                .map(|bytes| f32::from(i16::from_le_bytes(*bytes)) / 32_768.0)
                .collect()
        }
    }
}

/// The block one capture of `sample_count` frames is published as, its first
/// sample `start_sample` samples into the stream.
pub(crate) fn channels_block_of_a_captured_block(
    interleaved_sample_bytes: &[u8],
    sample_count: u32,
    start_sample: u64,
    stream_format: ModuleHostAudioStreamFormat,
) -> CapturedChannelsBlock {
    let interleaved_samples = interleaved_f32_samples_of_captured_bytes(
        interleaved_sample_bytes,
        stream_format.sample_format,
    );
    CapturedChannelsBlock {
        block: ModuleHostChannelsBlock {
            start_sample,
            sample_rate: stream_format.sample_rate,
            channel_count: stream_format.channels,
            samples_per_channel: sample_count,
            sample_type: ModuleHostChannelsBlockSampleType::F32,
        },
        planar_payload: ModuleHostChannelsBlock::planar_f32_payload_of_interleaved_samples(
            &interleaved_samples,
            stream_format.channels,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_f32_capture_is_published_planar_with_its_stream_format() {
        let interleaved_sample_bytes: Vec<u8> = [0.25f32, -0.5, 0.75, 1.0]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let captured = channels_block_of_a_captured_block(
            &interleaved_sample_bytes,
            2,
            96_000,
            ModuleHostAudioStreamFormat {
                sample_rate: 48_000,
                channels: 2,
                sample_format: ModuleHostAudioSampleFormat::F32,
            },
        );
        assert_eq!(captured.block.start_sample, 96_000);
        assert_eq!(captured.block.sample_rate, 48_000);
        assert_eq!(captured.block.channel_count, 2);
        assert_eq!(captured.block.samples_per_channel, 2);
        assert_eq!(
            captured
                .block
                .interleaved_f32_samples_of_the_planar_payload(&captured.planar_payload)
                .unwrap(),
            [0.25, -0.5, 0.75, 1.0]
        );
    }

    #[test]
    fn an_i16_capture_is_widened_to_f32() {
        let interleaved_sample_bytes: Vec<u8> = [i16::MIN, 16_384]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let captured = channels_block_of_a_captured_block(
            &interleaved_sample_bytes,
            2,
            0,
            ModuleHostAudioStreamFormat {
                sample_rate: 44_100,
                channels: 1,
                sample_format: ModuleHostAudioSampleFormat::I16,
            },
        );
        assert_eq!(
            captured
                .block
                .interleaved_f32_samples_of_the_planar_payload(&captured.planar_payload)
                .unwrap(),
            [-1.0, 0.5]
        );
    }
}
