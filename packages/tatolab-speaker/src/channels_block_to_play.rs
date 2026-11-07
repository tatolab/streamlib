// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Channels block a window of samples to play arrives as.

use tatolab_module_sdk::{
    ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat, ModuleHostChannelsBlock,
    ModuleHostTablePayloadDescriptor,
};

/// The interleaved sample bytes, in the playback stream's format, one message
/// carries once it is known to be a Channels block at the stream's rate and
/// channel count; why not, otherwise.
pub(crate) fn interleaved_sample_bytes_to_play(
    payload_descriptor: &ModuleHostTablePayloadDescriptor,
    planar_payload: &[u8],
    playback_stream_format: ModuleHostAudioStreamFormat,
) -> Result<Vec<u8>, String> {
    let block = ModuleHostChannelsBlock::of_the_payload_descriptor(payload_descriptor)
        .map_err(|refusal| format!("not a Channels block: {refusal}"))?;
    if block.sample_rate != playback_stream_format.sample_rate
        || block.channel_count != playback_stream_format.channels
    {
        return Err(format!(
            "a block at {} Hz × {} ch reached a stream playing {} Hz × {} ch; the `audio` \
             port's window converts what a link carries, so this block bypassed it",
            block.sample_rate,
            block.channel_count,
            playback_stream_format.sample_rate,
            playback_stream_format.channels
        ));
    }
    let interleaved_samples = block
        .interleaved_f32_samples_of_the_planar_payload(planar_payload)
        .map_err(|refusal| refusal.to_string())?;
    Ok(match playback_stream_format.sample_format {
        ModuleHostAudioSampleFormat::F32 => interleaved_samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect(),
        ModuleHostAudioSampleFormat::I16 => interleaved_samples
            .iter()
            .flat_map(|sample| {
                ((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).to_le_bytes()
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use tatolab_module_sdk::{ModuleHostChannelsBlockSampleType, packets_unit_payload_descriptor};

    use super::*;

    const PLAYING_48_KHZ_STEREO_F32: ModuleHostAudioStreamFormat = ModuleHostAudioStreamFormat {
        sample_rate: 48_000,
        channels: 2,
        sample_format: ModuleHostAudioSampleFormat::F32,
    };

    fn a_channels_block(
        sample_rate: u32,
        channel_count: u32,
        samples_per_channel: u32,
    ) -> ModuleHostTablePayloadDescriptor {
        ModuleHostChannelsBlock {
            start_sample: 0,
            sample_rate,
            channel_count,
            samples_per_channel,
            sample_type: ModuleHostChannelsBlockSampleType::F32,
        }
        .payload_descriptor()
    }

    #[test]
    fn a_block_in_the_streams_format_hands_over_its_samples_interleaved() {
        let planar_payload =
            ModuleHostChannelsBlock::planar_f32_payload_of_interleaved_samples(&[0.25, -0.5], 2);
        let expected: Vec<u8> = [0.25f32, -0.5]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(
            interleaved_sample_bytes_to_play(
                &a_channels_block(48_000, 2, 1),
                &planar_payload,
                PLAYING_48_KHZ_STEREO_F32
            ),
            Ok(expected)
        );
    }

    #[test]
    fn a_block_in_another_format_is_refused_naming_both_formats() {
        let refusal = interleaved_sample_bytes_to_play(
            &a_channels_block(16_000, 1, 1),
            &[0; 4],
            PLAYING_48_KHZ_STEREO_F32,
        )
        .unwrap_err();
        assert!(refusal.contains("16000 Hz × 1 ch"), "{refusal}");
        assert!(refusal.contains("48000 Hz × 2 ch"), "{refusal}");
    }

    #[test]
    fn a_block_whose_payload_disagrees_with_its_descriptor_is_refused() {
        let refusal = interleaved_sample_bytes_to_play(
            &a_channels_block(48_000, 2, 1),
            &[0; 4],
            PLAYING_48_KHZ_STEREO_F32,
        )
        .unwrap_err();
        assert!(refusal.contains("states 8"), "{refusal}");
    }

    #[test]
    fn a_message_that_is_not_a_channels_block_is_refused() {
        let refusal = interleaved_sample_bytes_to_play(
            &packets_unit_payload_descriptor("opus").unwrap(),
            &[],
            PLAYING_48_KHZ_STEREO_F32,
        )
        .unwrap_err();
        assert!(refusal.starts_with("not a Channels block"), "{refusal}");
    }
}
