// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The Image frame, Channels block, Packets unit and Records message a
//! payload descriptor carries, read and written in the engine's wire layout:
//! an Image frame's body is its GPU pool slot in bytes 0..40, then width,
//! height and texture format as little-endian `u32`s and its four H.273 code
//! points; a Channels block's is its start sample as a little-endian `u64`,
//! then sample rate, channel count, samples per channel and sample type as
//! little-endian `u32`s; a Packets unit's is its codec name, UTF-8 and
//! zero-padded to 32 bytes; a Records message's is all zero.

use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES, MODULE_HOST_TABLE_PAYLOAD_KIND_CHANNELS_BLOCK,
    MODULE_HOST_TABLE_PAYLOAD_KIND_GPU_POOL_SLOT_FRAME, MODULE_HOST_TABLE_PAYLOAD_KIND_IMAGE_FRAME,
    MODULE_HOST_TABLE_PAYLOAD_KIND_PACKETS_UNIT, MODULE_HOST_TABLE_PAYLOAD_KIND_RECORDS_MESSAGE,
    ModuleHostTablePayloadDescriptor,
};

const IMAGE_FRAME_GPU_POOL_SLOT_FRAME_BYTES: usize = 40;
const IMAGE_FRAME_WIDTH_OFFSET: usize = 40;
const IMAGE_FRAME_HEIGHT_OFFSET: usize = 44;
const IMAGE_FRAME_TEXTURE_FORMAT_OFFSET: usize = 48;
const IMAGE_FRAME_COLOUR_OFFSET: usize = 52;
const PACKETS_UNIT_CODEC_BYTES: usize = 32;
const CHANNELS_BLOCK_START_SAMPLE_OFFSET: usize = 0;
const CHANNELS_BLOCK_SAMPLE_RATE_OFFSET: usize = 8;
const CHANNELS_BLOCK_CHANNEL_COUNT_OFFSET: usize = 12;
const CHANNELS_BLOCK_SAMPLES_PER_CHANNEL_OFFSET: usize = 16;
const CHANNELS_BLOCK_SAMPLE_TYPE_OFFSET: usize = 20;

/// The H.273 code point that states nothing about an axis.
pub const MODULE_HOST_H273_UNSPECIFIED: u8 = 2;

/// An Image frame's colour as its four ITU-T H.273 code points; `2`
/// (unspecified) on an axis the frame does not state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostImageFrameH273Colour {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    /// `VideoFullRangeFlag`: 0 limited, 1 full, 2 unspecified.
    pub range: u8,
}

/// What an Image frame descriptor states besides its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostImageFrame {
    pub width: u32,
    pub height: u32,
    /// The engine's `TextureFormat`, as its `repr(u32)` value.
    pub texture_format: u32,
    pub colour: ModuleHostImageFrameH273Colour,
}

/// The sample type of a Channels block, by its wire value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ModuleHostChannelsBlockSampleType {
    F32 = 0,
    F64 = 1,
}

impl ModuleHostChannelsBlockSampleType {
    /// Bytes one sample of this type takes.
    pub fn sample_byte_len(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

/// What a Channels block descriptor states. Its payload is planar:
/// `channel_count` runs of `samples_per_channel` little-endian samples,
/// channel by channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostChannelsBlock {
    /// The index, counted from the stream's first sample, of the block's first sample.
    pub start_sample: u64,
    /// Samples per second, per channel.
    pub sample_rate: u32,
    pub channel_count: u32,
    pub samples_per_channel: u32,
    pub sample_type: ModuleHostChannelsBlockSampleType,
}

/// Why a payload descriptor is not the kind a reader takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleHostPayloadDescriptorRefusal {
    /// The message is described as another kind.
    AnotherKind { expected_kind: u32, kind: u32 },
    /// A codec name longer than 32 bytes, or holding a NUL.
    CodecNameDoesNotFit { codec: String },
    /// A Channels block of a sample type this SDK does not know.
    UnknownChannelsBlockSampleType { sample_type: u32 },
    /// A Channels block whose payload is not the length its descriptor states.
    ChannelsBlockPayloadLength {
        expected_byte_len: Option<usize>,
        byte_len: usize,
    },
}

impl std::fmt::Display for ModuleHostPayloadDescriptorRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModuleHostPayloadDescriptorRefusal::AnotherKind {
                expected_kind,
                kind,
            } => write!(
                formatter,
                "the message is described as kind {kind}, and this reader takes kind \
                 {expected_kind} only"
            ),
            ModuleHostPayloadDescriptorRefusal::CodecNameDoesNotFit { codec } => write!(
                formatter,
                "codec {codec:?} is longer than the {PACKETS_UNIT_CODEC_BYTES} bytes a Packets \
                 unit names its codec in, or holds a NUL"
            ),
            ModuleHostPayloadDescriptorRefusal::UnknownChannelsBlockSampleType { sample_type } => {
                write!(
                    formatter,
                    "the Channels block's sample type {sample_type} is neither f32 (0) nor f64 (1)"
                )
            }
            ModuleHostPayloadDescriptorRefusal::ChannelsBlockPayloadLength {
                expected_byte_len,
                byte_len,
            } => match expected_byte_len {
                Some(expected_byte_len) => write!(
                    formatter,
                    "the Channels block's payload is {byte_len} bytes, and its descriptor states \
                     {expected_byte_len}"
                ),
                None => write!(
                    formatter,
                    "the Channels block's descriptor states a payload past this machine's memory, \
                     and it carries {byte_len} bytes"
                ),
            },
        }
    }
}

impl std::error::Error for ModuleHostPayloadDescriptorRefusal {}

fn refuse_unless_the_kind_is(
    payload_descriptor: &ModuleHostTablePayloadDescriptor,
    expected_kind: u32,
) -> Result<(), ModuleHostPayloadDescriptorRefusal> {
    if payload_descriptor.kind == expected_kind {
        return Ok(());
    }
    Err(ModuleHostPayloadDescriptorRefusal::AnotherKind {
        expected_kind,
        kind: payload_descriptor.kind,
    })
}

fn u32_at(body: &[u8; MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES], offset: usize) -> u32 {
    u32::from_le_bytes([
        body[offset],
        body[offset + 1],
        body[offset + 2],
        body[offset + 3],
    ])
}

/// The texture formats an Image frame names, each as its wire name and the
/// engine's `TextureFormat` `repr(u32)` value the descriptor carries.
const IMAGE_FRAME_TEXTURE_FORMATS: &[(&str, u32)] = &[
    ("rgba8_unorm", 0),
    ("rgba8_unorm_srgb", 1),
    ("bgra8_unorm", 2),
    ("bgra8_unorm_srgb", 3),
    ("rgba16_float", 4),
    ("rgba32_float", 5),
    ("nv12", 6),
];

impl ModuleHostImageFrame {
    /// The `texture_format` value of the format `wire_name` spells, as an
    /// output pool texture request names it; `None` for a name the engine has
    /// no format for.
    pub fn texture_format_of_the_wire_name(wire_name: &str) -> Option<u32> {
        IMAGE_FRAME_TEXTURE_FORMATS
            .iter()
            .find(|(name, _)| *name == wire_name)
            .map(|&(_, texture_format)| texture_format)
    }

    /// The wire name of the format `texture_format` names, the inverse of
    /// [`Self::texture_format_of_the_wire_name`].
    pub fn wire_name_of_the_texture_format(texture_format: u32) -> Option<&'static str> {
        IMAGE_FRAME_TEXTURE_FORMATS
            .iter()
            .find(|(_, value)| *value == texture_format)
            .map(|&(name, _)| name)
    }

    /// The Image frame `payload_descriptor` states, refusing another kind.
    pub fn of_the_payload_descriptor(
        payload_descriptor: &ModuleHostTablePayloadDescriptor,
    ) -> Result<Self, ModuleHostPayloadDescriptorRefusal> {
        refuse_unless_the_kind_is(
            payload_descriptor,
            MODULE_HOST_TABLE_PAYLOAD_KIND_IMAGE_FRAME,
        )?;
        let body = &payload_descriptor.body;
        let colour = &body[IMAGE_FRAME_COLOUR_OFFSET..IMAGE_FRAME_COLOUR_OFFSET + 4];
        Ok(Self {
            width: u32_at(body, IMAGE_FRAME_WIDTH_OFFSET),
            height: u32_at(body, IMAGE_FRAME_HEIGHT_OFFSET),
            texture_format: u32_at(body, IMAGE_FRAME_TEXTURE_FORMAT_OFFSET),
            colour: ModuleHostImageFrameH273Colour {
                primaries: colour[0],
                transfer: colour[1],
                matrix: colour[2],
                range: colour[3],
            },
        })
    }

    /// The descriptor of this frame in the slot `gpu_pool_slot_frame` names,
    /// as [`crate::ModuleHostSurface::gpu_pool_slot_frame`] answers it.
    pub fn payload_descriptor_in_the_slot(
        &self,
        gpu_pool_slot_frame: &ModuleHostTablePayloadDescriptor,
    ) -> Result<ModuleHostTablePayloadDescriptor, ModuleHostPayloadDescriptorRefusal> {
        refuse_unless_the_kind_is(
            gpu_pool_slot_frame,
            MODULE_HOST_TABLE_PAYLOAD_KIND_GPU_POOL_SLOT_FRAME,
        )?;
        let mut body = [0u8; MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        body[..IMAGE_FRAME_GPU_POOL_SLOT_FRAME_BYTES]
            .copy_from_slice(&gpu_pool_slot_frame.body[..IMAGE_FRAME_GPU_POOL_SLOT_FRAME_BYTES]);
        body[IMAGE_FRAME_WIDTH_OFFSET..IMAGE_FRAME_HEIGHT_OFFSET]
            .copy_from_slice(&self.width.to_le_bytes());
        body[IMAGE_FRAME_HEIGHT_OFFSET..IMAGE_FRAME_TEXTURE_FORMAT_OFFSET]
            .copy_from_slice(&self.height.to_le_bytes());
        body[IMAGE_FRAME_TEXTURE_FORMAT_OFFSET..IMAGE_FRAME_COLOUR_OFFSET]
            .copy_from_slice(&self.texture_format.to_le_bytes());
        body[IMAGE_FRAME_COLOUR_OFFSET..IMAGE_FRAME_COLOUR_OFFSET + 4].copy_from_slice(&[
            self.colour.primaries,
            self.colour.transfer,
            self.colour.matrix,
            self.colour.range,
        ]);
        Ok(ModuleHostTablePayloadDescriptor {
            kind: MODULE_HOST_TABLE_PAYLOAD_KIND_IMAGE_FRAME,
            body,
        })
    }
}

impl ModuleHostChannelsBlock {
    /// The Channels block `payload_descriptor` states, refusing another kind.
    pub fn of_the_payload_descriptor(
        payload_descriptor: &ModuleHostTablePayloadDescriptor,
    ) -> Result<Self, ModuleHostPayloadDescriptorRefusal> {
        refuse_unless_the_kind_is(
            payload_descriptor,
            MODULE_HOST_TABLE_PAYLOAD_KIND_CHANNELS_BLOCK,
        )?;
        let body = &payload_descriptor.body;
        let mut start_sample = [0u8; 8];
        start_sample.copy_from_slice(
            &body[CHANNELS_BLOCK_START_SAMPLE_OFFSET..CHANNELS_BLOCK_SAMPLE_RATE_OFFSET],
        );
        let sample_type = match u32_at(body, CHANNELS_BLOCK_SAMPLE_TYPE_OFFSET) {
            0 => ModuleHostChannelsBlockSampleType::F32,
            1 => ModuleHostChannelsBlockSampleType::F64,
            sample_type => {
                return Err(
                    ModuleHostPayloadDescriptorRefusal::UnknownChannelsBlockSampleType {
                        sample_type,
                    },
                );
            }
        };
        Ok(Self {
            start_sample: u64::from_le_bytes(start_sample),
            sample_rate: u32_at(body, CHANNELS_BLOCK_SAMPLE_RATE_OFFSET),
            channel_count: u32_at(body, CHANNELS_BLOCK_CHANNEL_COUNT_OFFSET),
            samples_per_channel: u32_at(body, CHANNELS_BLOCK_SAMPLES_PER_CHANNEL_OFFSET),
            sample_type,
        })
    }

    /// The descriptor of this block.
    pub fn payload_descriptor(&self) -> ModuleHostTablePayloadDescriptor {
        let mut body = [0u8; MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        body[CHANNELS_BLOCK_START_SAMPLE_OFFSET..CHANNELS_BLOCK_SAMPLE_RATE_OFFSET]
            .copy_from_slice(&self.start_sample.to_le_bytes());
        for (offset, value) in [
            (CHANNELS_BLOCK_SAMPLE_RATE_OFFSET, self.sample_rate),
            (CHANNELS_BLOCK_CHANNEL_COUNT_OFFSET, self.channel_count),
            (
                CHANNELS_BLOCK_SAMPLES_PER_CHANNEL_OFFSET,
                self.samples_per_channel,
            ),
            (CHANNELS_BLOCK_SAMPLE_TYPE_OFFSET, self.sample_type as u32),
        ] {
            body[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        ModuleHostTablePayloadDescriptor {
            kind: MODULE_HOST_TABLE_PAYLOAD_KIND_CHANNELS_BLOCK,
            body,
        }
    }

    /// The bytes this block's planar payload takes, or `None` past `usize`.
    pub fn planar_payload_byte_len(&self) -> Option<usize> {
        (self.channel_count as usize)
            .checked_mul(self.samples_per_channel as usize)?
            .checked_mul(self.sample_type.sample_byte_len())
    }

    /// The planar f32 payload of `interleaved_samples`, frames of
    /// `channel_count` samples each, as a block of this layout carries it.
    pub fn planar_f32_payload_of_interleaved_samples(
        interleaved_samples: &[f32],
        channel_count: u32,
    ) -> Vec<u8> {
        let channel_count = channel_count.max(1) as usize;
        let samples_per_channel = interleaved_samples.len() / channel_count;
        let mut planar_payload = Vec::with_capacity(samples_per_channel * channel_count * 4);
        for channel in 0..channel_count {
            for frame in 0..samples_per_channel {
                planar_payload.extend_from_slice(
                    &interleaved_samples[frame * channel_count + channel].to_le_bytes(),
                );
            }
        }
        planar_payload
    }

    /// This block's samples, interleaved frame by frame and widened or
    /// narrowed to f32, read from its planar `payload`; refused when the
    /// payload is not the length the descriptor states.
    pub fn interleaved_f32_samples_of_the_planar_payload(
        &self,
        payload: &[u8],
    ) -> Result<Vec<f32>, ModuleHostPayloadDescriptorRefusal> {
        let expected_byte_len = self.planar_payload_byte_len();
        if expected_byte_len != Some(payload.len()) {
            return Err(
                ModuleHostPayloadDescriptorRefusal::ChannelsBlockPayloadLength {
                    expected_byte_len,
                    byte_len: payload.len(),
                },
            );
        }
        let channel_count = self.channel_count as usize;
        let samples_per_channel = self.samples_per_channel as usize;
        let sample_byte_len = self.sample_type.sample_byte_len();
        let sample_at = |channel: usize, frame: usize| -> f32 {
            let offset = (channel * samples_per_channel + frame) * sample_byte_len;
            let bytes = &payload[offset..offset + sample_byte_len];
            match self.sample_type {
                ModuleHostChannelsBlockSampleType::F32 => {
                    f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                }
                ModuleHostChannelsBlockSampleType::F64 => f64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]) as f32,
            }
        };
        let mut interleaved_samples = Vec::with_capacity(channel_count * samples_per_channel);
        for frame in 0..samples_per_channel {
            for channel in 0..channel_count {
                interleaved_samples.push(sample_at(channel, frame));
            }
        }
        Ok(interleaved_samples)
    }
}

/// The descriptor of a Records message, whose payload is one msgpack map.
pub fn records_message_payload_descriptor() -> ModuleHostTablePayloadDescriptor {
    ModuleHostTablePayloadDescriptor {
        kind: MODULE_HOST_TABLE_PAYLOAD_KIND_RECORDS_MESSAGE,
        body: [0u8; MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    }
}

/// The descriptor of a Packets unit coded as `codec`; empty for a plain byte
/// stream.
pub fn packets_unit_payload_descriptor(
    codec: &str,
) -> Result<ModuleHostTablePayloadDescriptor, ModuleHostPayloadDescriptorRefusal> {
    let codec_bytes = codec.as_bytes();
    if codec_bytes.len() > PACKETS_UNIT_CODEC_BYTES || codec_bytes.contains(&0) {
        return Err(ModuleHostPayloadDescriptorRefusal::CodecNameDoesNotFit {
            codec: codec.to_string(),
        });
    }
    let mut body = [0u8; MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES];
    body[..codec_bytes.len()].copy_from_slice(codec_bytes);
    Ok(ModuleHostTablePayloadDescriptor {
        kind: MODULE_HOST_TABLE_PAYLOAD_KIND_PACKETS_UNIT,
        body,
    })
}

/// The codec a Packets unit's descriptor names, refusing another kind; codec
/// bytes that are not UTF-8 read lossily.
pub fn codec_of_a_packets_unit(
    payload_descriptor: &ModuleHostTablePayloadDescriptor,
) -> Result<String, ModuleHostPayloadDescriptorRefusal> {
    refuse_unless_the_kind_is(
        payload_descriptor,
        MODULE_HOST_TABLE_PAYLOAD_KIND_PACKETS_UNIT,
    )?;
    let codec = &payload_descriptor.body[..PACKETS_UNIT_CODEC_BYTES];
    let codec_len = codec
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(codec.len());
    Ok(String::from_utf8_lossy(&codec[..codec_len]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_host_table_abi::MODULE_HOST_TABLE_PAYLOAD_KIND_BYTES;

    #[test]
    fn an_image_frame_written_in_a_slot_reads_back_with_the_slot_kept() {
        let mut slot_body = [0u8; MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        slot_body[..40].copy_from_slice(&[7u8; 40]);
        let slot = ModuleHostTablePayloadDescriptor {
            kind: MODULE_HOST_TABLE_PAYLOAD_KIND_GPU_POOL_SLOT_FRAME,
            body: slot_body,
        };
        let frame = ModuleHostImageFrame {
            width: 1920,
            height: 1080,
            texture_format: 3,
            colour: ModuleHostImageFrameH273Colour {
                primaries: 1,
                transfer: 13,
                matrix: 1,
                range: 1,
            },
        };
        let written = frame.payload_descriptor_in_the_slot(&slot).unwrap();
        assert_eq!(written.kind, MODULE_HOST_TABLE_PAYLOAD_KIND_IMAGE_FRAME);
        assert_eq!(&written.body[..40], &[7u8; 40]);
        assert_eq!(&written.body[56..], &[0u8; 4]);
        assert_eq!(
            ModuleHostImageFrame::of_the_payload_descriptor(&written).unwrap(),
            frame
        );
    }

    #[test]
    fn a_channels_block_reads_back_with_every_field_at_its_offset() {
        let block = ModuleHostChannelsBlock {
            start_sample: 0x0102_0304_0506_0708,
            sample_rate: 48_000,
            channel_count: 2,
            samples_per_channel: 960,
            sample_type: ModuleHostChannelsBlockSampleType::F64,
        };
        let descriptor = block.payload_descriptor();
        assert_eq!(
            descriptor.kind,
            MODULE_HOST_TABLE_PAYLOAD_KIND_CHANNELS_BLOCK
        );
        assert_eq!(
            &descriptor.body[..8],
            &0x0102_0304_0506_0708u64.to_le_bytes()
        );
        assert_eq!(&descriptor.body[20..24], &1u32.to_le_bytes());
        assert_eq!(&descriptor.body[24..], &[0u8; 36]);
        assert_eq!(
            ModuleHostChannelsBlock::of_the_payload_descriptor(&descriptor).unwrap(),
            block
        );
        assert_eq!(block.planar_payload_byte_len(), Some(2 * 960 * 8));
    }

    #[test]
    fn a_channels_block_of_an_unknown_sample_type_is_refused_by_its_value() {
        let mut descriptor = ModuleHostChannelsBlock {
            start_sample: 0,
            sample_rate: 48_000,
            channel_count: 1,
            samples_per_channel: 1,
            sample_type: ModuleHostChannelsBlockSampleType::F32,
        }
        .payload_descriptor();
        descriptor.body[20] = 9;
        assert_eq!(
            ModuleHostChannelsBlock::of_the_payload_descriptor(&descriptor).unwrap_err(),
            ModuleHostPayloadDescriptorRefusal::UnknownChannelsBlockSampleType { sample_type: 9 }
        );
    }

    #[test]
    fn interleaved_samples_round_trip_through_a_planar_payload() {
        let interleaved_samples = [0.0f32, 1.0, 0.25, -1.0, 0.5, 0.75];
        let planar_payload = ModuleHostChannelsBlock::planar_f32_payload_of_interleaved_samples(
            &interleaved_samples,
            2,
        );
        let first_channel: Vec<f32> = planar_payload[..12]
            .chunks(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();
        assert_eq!(first_channel, [0.0, 0.25, 0.5]);
        let block = ModuleHostChannelsBlock {
            start_sample: 0,
            sample_rate: 48_000,
            channel_count: 2,
            samples_per_channel: 3,
            sample_type: ModuleHostChannelsBlockSampleType::F32,
        };
        assert_eq!(
            block
                .interleaved_f32_samples_of_the_planar_payload(&planar_payload)
                .unwrap(),
            interleaved_samples
        );
        assert!(matches!(
            block.interleaved_f32_samples_of_the_planar_payload(&planar_payload[..8]),
            Err(ModuleHostPayloadDescriptorRefusal::ChannelsBlockPayloadLength { .. })
        ));
    }

    #[test]
    fn an_f64_block_reads_as_f32_samples() {
        let planar_payload: Vec<u8> = [0.5f64, -0.25]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let block = ModuleHostChannelsBlock {
            start_sample: 0,
            sample_rate: 48_000,
            channel_count: 1,
            samples_per_channel: 2,
            sample_type: ModuleHostChannelsBlockSampleType::F64,
        };
        assert_eq!(
            block
                .interleaved_f32_samples_of_the_planar_payload(&planar_payload)
                .unwrap(),
            [0.5, -0.25]
        );
    }

    #[test]
    fn a_texture_format_reads_back_through_its_wire_name() {
        for &(wire_name, texture_format) in IMAGE_FRAME_TEXTURE_FORMATS {
            assert_eq!(
                ModuleHostImageFrame::texture_format_of_the_wire_name(wire_name),
                Some(texture_format)
            );
            assert_eq!(
                ModuleHostImageFrame::wire_name_of_the_texture_format(texture_format),
                Some(wire_name)
            );
        }
        assert_eq!(
            ModuleHostImageFrame::texture_format_of_the_wire_name("r8"),
            None
        );
    }

    #[test]
    fn a_packets_unit_names_its_codec_and_refuses_one_that_does_not_fit() {
        let descriptor = packets_unit_payload_descriptor("h265").unwrap();
        assert_eq!(codec_of_a_packets_unit(&descriptor).unwrap(), "h265");
        assert!(packets_unit_payload_descriptor(&"x".repeat(33)).is_err());
    }

    #[test]
    fn a_reader_refuses_another_kind_naming_both() {
        let refusal =
            codec_of_a_packets_unit(&ModuleHostTablePayloadDescriptor::bytes()).unwrap_err();
        assert_eq!(
            refusal,
            ModuleHostPayloadDescriptorRefusal::AnotherKind {
                expected_kind: MODULE_HOST_TABLE_PAYLOAD_KIND_PACKETS_UNIT,
                kind: MODULE_HOST_TABLE_PAYLOAD_KIND_BYTES,
            }
        );
    }
}
