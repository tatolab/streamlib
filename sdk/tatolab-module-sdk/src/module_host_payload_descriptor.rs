// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Image frame and Packets unit a payload descriptor carries, read and
//! written in the engine's wire layout: an Image frame's body is its GPU pool
//! slot in bytes 0..40, then width, height and texture format as
//! little-endian `u32`s and its four H.273 code points; a Packets unit's is
//! its codec name, UTF-8 and zero-padded to 32 bytes.

use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_PAYLOAD_DESCRIPTOR_BODY_BYTES,
    MODULE_HOST_TABLE_PAYLOAD_KIND_GPU_POOL_SLOT_FRAME, MODULE_HOST_TABLE_PAYLOAD_KIND_IMAGE_FRAME,
    MODULE_HOST_TABLE_PAYLOAD_KIND_PACKETS_UNIT, ModuleHostTablePayloadDescriptor,
};

const IMAGE_FRAME_GPU_POOL_SLOT_FRAME_BYTES: usize = 40;
const IMAGE_FRAME_WIDTH_OFFSET: usize = 40;
const IMAGE_FRAME_HEIGHT_OFFSET: usize = 44;
const IMAGE_FRAME_TEXTURE_FORMAT_OFFSET: usize = 48;
const IMAGE_FRAME_COLOUR_OFFSET: usize = 52;
const PACKETS_UNIT_CODEC_BYTES: usize = 32;

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

/// Why a payload descriptor is not the kind a reader takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleHostPayloadDescriptorRefusal {
    /// The message is described as another kind.
    AnotherKind { expected_kind: u32, kind: u32 },
    /// A codec name longer than 32 bytes, or holding a NUL.
    CodecNameDoesNotFit { codec: String },
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
