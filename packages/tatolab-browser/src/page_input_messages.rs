// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The two messages a page input reads: an Image frame naming the pooled
//! frame to read back, and a Channels block whose samples reach the page as
//! interleaved little-endian `f32`.

use tatolab_module_sdk::{
    ModuleHostChannelsBlock, ModuleHostImageFrame, ModuleHostTablePayloadDescriptor,
};

/// The extent of an Image frame a video input reads back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageInputVideoFrameExtent {
    pub width: u32,
    pub height: u32,
}

/// One Channels block, its samples interleaved as little-endian `f32`.
#[derive(Debug, Clone, PartialEq)]
pub struct PageInputAudioBlock {
    pub sample_rate: u32,
    pub channel_count: u32,
    pub interleaved_samples_le_f32: Vec<u8>,
}

impl PageInputVideoFrameExtent {
    /// The extent the Image frame `payload_descriptor` states.
    pub fn of_the_image_frame(
        payload_descriptor: &ModuleHostTablePayloadDescriptor,
    ) -> Result<Self, String> {
        let image_frame = ModuleHostImageFrame::of_the_payload_descriptor(payload_descriptor)
            .map_err(|refusal| {
                format!("a video input's message is not an Image frame: {refusal}")
            })?;
        Ok(Self {
            width: image_frame.width,
            height: image_frame.height,
        })
    }
}

impl PageInputAudioBlock {
    /// The Channels block `payload_descriptor` states over its planar
    /// `payload`, interleaved and narrowed to `f32`.
    pub fn of_the_channels_block(
        payload_descriptor: &ModuleHostTablePayloadDescriptor,
        payload: &[u8],
    ) -> Result<Self, String> {
        let block = ModuleHostChannelsBlock::of_the_payload_descriptor(payload_descriptor)
            .map_err(|refusal| {
                format!("an audio input's message is not a Channels block: {refusal}")
            })?;
        let interleaved_samples = block
            .interleaved_f32_samples_of_the_planar_payload(payload)
            .map_err(|refusal| format!("an audio input's Channels block: {refusal}"))?;
        Ok(Self {
            sample_rate: block.sample_rate,
            channel_count: block.channel_count,
            interleaved_samples_le_f32: interleaved_samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tatolab_module_sdk::{
        ModuleHostChannelsBlockSampleType, ModuleHostImageFrameH273Colour,
        records_message_payload_descriptor,
    };

    /// The module host table's wire kind of a GPU pool slot frame.
    const GPU_POOL_SLOT_FRAME_PAYLOAD_KIND: u32 = 1;

    fn le_f32_bytes(samples: &[f32]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    #[test]
    fn a_video_input_reads_the_extent_its_image_frame_states() {
        let mut gpu_pool_slot_frame = ModuleHostTablePayloadDescriptor::bytes();
        gpu_pool_slot_frame.kind = GPU_POOL_SLOT_FRAME_PAYLOAD_KIND;
        let image_frame = ModuleHostImageFrame {
            width: 1920,
            height: 1080,
            texture_format: ModuleHostImageFrame::texture_format_of_the_wire_name("rgba8_unorm")
                .unwrap(),
            colour: ModuleHostImageFrameH273Colour {
                primaries: 1,
                transfer: 13,
                matrix: 0,
                range: 1,
            },
        }
        .payload_descriptor_in_the_slot(&gpu_pool_slot_frame)
        .unwrap();

        assert_eq!(
            PageInputVideoFrameExtent::of_the_image_frame(&image_frame).unwrap(),
            PageInputVideoFrameExtent {
                width: 1920,
                height: 1080,
            }
        );
        assert!(
            PageInputVideoFrameExtent::of_the_image_frame(&records_message_payload_descriptor())
                .is_err()
        );
    }

    #[test]
    fn a_planar_f32_block_reaches_the_page_interleaved() {
        let block = ModuleHostChannelsBlock {
            start_sample: 0,
            sample_rate: 48_000,
            channel_count: 2,
            samples_per_channel: 2,
            sample_type: ModuleHostChannelsBlockSampleType::F32,
        };
        let planar_payload = le_f32_bytes(&[0.25, 0.5, -0.25, -0.5]);

        let read = PageInputAudioBlock::of_the_channels_block(
            &block.payload_descriptor(),
            &planar_payload,
        )
        .unwrap();

        assert_eq!(read.sample_rate, 48_000);
        assert_eq!(read.channel_count, 2);
        assert_eq!(
            read.interleaved_samples_le_f32,
            le_f32_bytes(&[0.25, -0.25, 0.5, -0.5])
        );
    }

    #[test]
    fn an_f64_block_narrows_to_f32_and_a_short_block_is_refused() {
        let block = ModuleHostChannelsBlock {
            start_sample: 7,
            sample_rate: 16_000,
            channel_count: 1,
            samples_per_channel: 2,
            sample_type: ModuleHostChannelsBlockSampleType::F64,
        };
        let planar_payload: Vec<u8> = [-1.0f64, 0.5]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();

        let read = PageInputAudioBlock::of_the_channels_block(
            &block.payload_descriptor(),
            &planar_payload,
        )
        .unwrap();

        assert_eq!(read.interleaved_samples_le_f32, le_f32_bytes(&[-1.0, 0.5]));
        assert!(
            PageInputAudioBlock::of_the_channels_block(
                &block.payload_descriptor(),
                &planar_payload[..9]
            )
            .is_err()
        );
    }
}
