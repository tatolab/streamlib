// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The descriptor bodies of each kind of message: an Image frame, an Array
//! frame and its attribute table, a Channels block and a Packets unit; a
//! Records message has none. Each packs
//! little-endian into a [`crate::DataChannelPayloadDescriptor`] body at its
//! documented offsets; the tail past its used bytes is reserved zero.

use crate::{
    DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES, GPU_POOL_SLOT_FRAME_BODY_USED_BYTES,
    GpuPoolSlotFrameDescriptorBody,
};

/// Byte offset of [`ImageFrameDescriptorBody::width`] in a descriptor body.
pub const IMAGE_FRAME_BODY_WIDTH_OFFSET: usize = GPU_POOL_SLOT_FRAME_BODY_USED_BYTES;
/// Byte offset of [`ImageFrameDescriptorBody::height`] in a descriptor body.
pub const IMAGE_FRAME_BODY_HEIGHT_OFFSET: usize = 44;
/// Byte offset of [`ImageFrameDescriptorBody::format`] in a descriptor body.
pub const IMAGE_FRAME_BODY_FORMAT_OFFSET: usize = 48;
/// Byte offset of [`ImageFrameDescriptorBody::colour`] in a descriptor body.
pub const IMAGE_FRAME_BODY_COLOUR_OFFSET: usize = 52;
/// Bytes of a descriptor body an Image frame uses.
pub const IMAGE_FRAME_BODY_USED_BYTES: usize = 56;

/// An Image frame's colour as its four ITU-T H.273 code points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageFrameH273Colour {
    /// `ColourPrimaries`.
    pub primaries: u8,
    /// `TransferCharacteristics`.
    pub transfer: u8,
    /// `MatrixCoefficients`.
    pub matrix: u8,
    /// `VideoFullRangeFlag`: 0 limited, 1 full.
    pub range: u8,
}

impl ImageFrameH273Colour {
    /// A frame that states no colour: H.273 code point 2, unspecified, on
    /// every axis.
    pub const UNSPECIFIED: Self = Self {
        primaries: 2,
        transfer: 2,
        matrix: 2,
        range: 2,
    };
}

/// The body of an Image frame descriptor: the GPU pool slot the frame lives
/// in, then its size, its pixel format and its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageFrameDescriptorBody {
    /// The pool slot the frame lives in, in its first 40 bytes.
    pub slot: GpuPoolSlotFrameDescriptorBody,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The RHI `TextureFormat` the slot holds, as its `repr(u32)` value.
    pub format: u32,
    /// The frame's colour.
    pub colour: ImageFrameH273Colour,
}

impl ImageFrameDescriptorBody {
    /// This body as descriptor body bytes, the reserved tail zero.
    pub fn to_descriptor_body(&self) -> [u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES] {
        let mut body = self.slot.to_descriptor_body();
        body[IMAGE_FRAME_BODY_WIDTH_OFFSET..IMAGE_FRAME_BODY_HEIGHT_OFFSET]
            .copy_from_slice(&self.width.to_le_bytes());
        body[IMAGE_FRAME_BODY_HEIGHT_OFFSET..IMAGE_FRAME_BODY_FORMAT_OFFSET]
            .copy_from_slice(&self.height.to_le_bytes());
        body[IMAGE_FRAME_BODY_FORMAT_OFFSET..IMAGE_FRAME_BODY_COLOUR_OFFSET]
            .copy_from_slice(&self.format.to_le_bytes());
        body[IMAGE_FRAME_BODY_COLOUR_OFFSET..IMAGE_FRAME_BODY_USED_BYTES].copy_from_slice(&[
            self.colour.primaries,
            self.colour.transfer,
            self.colour.matrix,
            self.colour.range,
        ]);
        body
    }

    /// The body `descriptor_body` packs.
    pub fn from_descriptor_body(
        descriptor_body: &[u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    ) -> Self {
        let u32_at = |offset: usize| {
            let mut field = [0u8; 4];
            field.copy_from_slice(&descriptor_body[offset..offset + 4]);
            u32::from_le_bytes(field)
        };
        let colour = &descriptor_body[IMAGE_FRAME_BODY_COLOUR_OFFSET..IMAGE_FRAME_BODY_USED_BYTES];
        Self {
            slot: GpuPoolSlotFrameDescriptorBody::from_descriptor_body(descriptor_body),
            width: u32_at(IMAGE_FRAME_BODY_WIDTH_OFFSET),
            height: u32_at(IMAGE_FRAME_BODY_HEIGHT_OFFSET),
            format: u32_at(IMAGE_FRAME_BODY_FORMAT_OFFSET),
            colour: ImageFrameH273Colour {
                primaries: colour[0],
                transfer: colour[1],
                matrix: colour[2],
                range: colour[3],
            },
        }
    }
}

/// Byte offset of [`ChannelsBlockDescriptorBody::start_sample`] in a descriptor body.
pub const CHANNELS_BLOCK_BODY_START_SAMPLE_OFFSET: usize = 0;
/// Byte offset of [`ChannelsBlockDescriptorBody::sample_rate`] in a descriptor body.
pub const CHANNELS_BLOCK_BODY_SAMPLE_RATE_OFFSET: usize = 8;
/// Byte offset of [`ChannelsBlockDescriptorBody::channel_count`] in a descriptor body.
pub const CHANNELS_BLOCK_BODY_CHANNEL_COUNT_OFFSET: usize = 12;
/// Byte offset of [`ChannelsBlockDescriptorBody::samples_per_channel`] in a descriptor body.
pub const CHANNELS_BLOCK_BODY_SAMPLES_PER_CHANNEL_OFFSET: usize = 16;
/// Byte offset of [`ChannelsBlockDescriptorBody::sample_type`] in a descriptor body.
pub const CHANNELS_BLOCK_BODY_SAMPLE_TYPE_OFFSET: usize = 20;
/// Bytes of a descriptor body a Channels block uses.
pub const CHANNELS_BLOCK_BODY_USED_BYTES: usize = 24;

/// The sample type of a Channels block, by its wire value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ChannelsBlockSampleType {
    F32 = 0,
    F64 = 1,
}

impl ChannelsBlockSampleType {
    /// The sample type `wire_value` names, or `None` for one this build does not know.
    pub fn from_wire(wire_value: u32) -> Option<Self> {
        match wire_value {
            0 => Some(Self::F32),
            1 => Some(Self::F64),
            _ => None,
        }
    }

    /// Bytes one sample of this type takes.
    pub fn sample_byte_len(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    /// The dtype an Array attribute holding these samples states: `float32` or `float64`.
    pub fn array_dtype_wire_name(self) -> &'static str {
        match self {
            Self::F32 => "float32",
            Self::F64 => "float64",
        }
    }

    /// The sample type an Array attribute's dtype `wire_name` holds, or `None`
    /// for a dtype no Channels block carries.
    pub fn from_array_dtype_wire_name(wire_name: &str) -> Option<Self> {
        [Self::F32, Self::F64]
            .into_iter()
            .find(|sample_type| sample_type.array_dtype_wire_name() == wire_name)
    }
}

/// The body of a Channels block descriptor. The payload is planar:
/// `channel_count` runs of `samples_per_channel` samples, channel by channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelsBlockDescriptorBody {
    /// The index, counted from the stream's first sample, of this block's first sample.
    pub start_sample: u64,
    /// Samples per second, per channel.
    pub sample_rate: u32,
    /// How many channels the payload holds.
    pub channel_count: u32,
    /// How many samples each channel's run holds.
    pub samples_per_channel: u32,
    /// A [`ChannelsBlockSampleType`] wire value.
    pub sample_type: u32,
}

impl ChannelsBlockDescriptorBody {
    /// This body as descriptor body bytes, the reserved tail zero.
    pub fn to_descriptor_body(&self) -> [u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES] {
        let mut body = [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        body[CHANNELS_BLOCK_BODY_START_SAMPLE_OFFSET..CHANNELS_BLOCK_BODY_SAMPLE_RATE_OFFSET]
            .copy_from_slice(&self.start_sample.to_le_bytes());
        for (offset, value) in [
            (CHANNELS_BLOCK_BODY_SAMPLE_RATE_OFFSET, self.sample_rate),
            (CHANNELS_BLOCK_BODY_CHANNEL_COUNT_OFFSET, self.channel_count),
            (
                CHANNELS_BLOCK_BODY_SAMPLES_PER_CHANNEL_OFFSET,
                self.samples_per_channel,
            ),
            (CHANNELS_BLOCK_BODY_SAMPLE_TYPE_OFFSET, self.sample_type),
        ] {
            body[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        body
    }

    /// The body `descriptor_body` packs.
    pub fn from_descriptor_body(
        descriptor_body: &[u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    ) -> Self {
        let u32_at = |offset: usize| {
            let mut field = [0u8; 4];
            field.copy_from_slice(&descriptor_body[offset..offset + 4]);
            u32::from_le_bytes(field)
        };
        let mut start_sample = [0u8; 8];
        start_sample.copy_from_slice(
            &descriptor_body
                [CHANNELS_BLOCK_BODY_START_SAMPLE_OFFSET..CHANNELS_BLOCK_BODY_SAMPLE_RATE_OFFSET],
        );
        Self {
            start_sample: u64::from_le_bytes(start_sample),
            sample_rate: u32_at(CHANNELS_BLOCK_BODY_SAMPLE_RATE_OFFSET),
            channel_count: u32_at(CHANNELS_BLOCK_BODY_CHANNEL_COUNT_OFFSET),
            samples_per_channel: u32_at(CHANNELS_BLOCK_BODY_SAMPLES_PER_CHANNEL_OFFSET),
            sample_type: u32_at(CHANNELS_BLOCK_BODY_SAMPLE_TYPE_OFFSET),
        }
    }

    /// The payload bytes this block's planar samples take, or `None` past `usize`.
    pub fn planar_payload_byte_len(&self) -> Option<usize> {
        let sample_type = ChannelsBlockSampleType::from_wire(self.sample_type)?;
        (self.channel_count as usize)
            .checked_mul(self.samples_per_channel as usize)?
            .checked_mul(sample_type.sample_byte_len())
    }
}

/// Bytes of a Packets unit's descriptor body its codec name takes, UTF-8 and
/// zero-padded; a unit with no codec leaves them zero.
pub const PACKETS_UNIT_BODY_CODEC_BYTES: usize = 32;

/// The body of a Packets unit descriptor: the codec its bytes are encoded
/// with, or none for a plain byte stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacketsUnitDescriptorBody {
    /// The codec name, at most [`PACKETS_UNIT_BODY_CODEC_BYTES`] bytes of
    /// UTF-8; empty for a plain byte stream.
    pub codec: String,
}

/// Why a codec name cannot ride a Packets unit's descriptor body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacketsUnitCodecNameDoesNotFit {
    pub codec: String,
}

impl std::fmt::Display for PacketsUnitCodecNameDoesNotFit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "codec {:?} is longer than the {PACKETS_UNIT_BODY_CODEC_BYTES} bytes a Packets unit \
             names its codec in, or holds a NUL",
            self.codec
        )
    }
}

impl std::error::Error for PacketsUnitCodecNameDoesNotFit {}

impl PacketsUnitDescriptorBody {
    /// This body as descriptor body bytes, refused when the codec name does not fit.
    pub fn to_descriptor_body(
        &self,
    ) -> Result<[u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES], PacketsUnitCodecNameDoesNotFit>
    {
        let codec = self.codec.as_bytes();
        if codec.len() > PACKETS_UNIT_BODY_CODEC_BYTES || codec.contains(&0) {
            return Err(PacketsUnitCodecNameDoesNotFit {
                codec: self.codec.clone(),
            });
        }
        let mut body = [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        body[..codec.len()].copy_from_slice(codec);
        Ok(body)
    }

    /// The body `descriptor_body` packs; codec bytes that are not UTF-8 read lossily.
    pub fn from_descriptor_body(
        descriptor_body: &[u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    ) -> Self {
        let codec = &descriptor_body[..PACKETS_UNIT_BODY_CODEC_BYTES];
        let codec_len = codec
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(codec.len());
        Self {
            codec: String::from_utf8_lossy(&codec[..codec_len]).into_owned(),
        }
    }
}

/// The body of an Array frame descriptor: how many rows the attribute table
/// at the start of the payload holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArrayFrameDescriptorBody {
    /// Rows in the attribute table.
    pub attribute_count: u32,
}

impl ArrayFrameDescriptorBody {
    /// This body as descriptor body bytes, the reserved tail zero.
    pub fn to_descriptor_body(&self) -> [u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES] {
        let mut body = [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        body[..4].copy_from_slice(&self.attribute_count.to_le_bytes());
        body
    }

    /// The body `descriptor_body` packs.
    pub fn from_descriptor_body(
        descriptor_body: &[u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    ) -> Self {
        let mut attribute_count = [0u8; 4];
        attribute_count.copy_from_slice(&descriptor_body[..4]);
        Self {
            attribute_count: u32::from_le_bytes(attribute_count),
        }
    }
}

/// Bytes an attribute table row gives its name, UTF-8 and zero-padded.
pub const ARRAY_FRAME_ATTRIBUTE_NAME_BYTES: usize = 32;
/// Bytes an attribute table row gives its element type's wire name, zero-padded.
pub const ARRAY_FRAME_ATTRIBUTE_DTYPE_BYTES: usize = 16;
/// The most dimensions one attribute has.
pub const ARRAY_FRAME_ATTRIBUTE_MAX_DIMENSIONS: usize = 8;
/// Byte offset of [`ArrayFrameAttributeRow::dtype`] in a row.
pub const ARRAY_FRAME_ATTRIBUTE_ROW_DTYPE_OFFSET: usize = 32;
/// Byte offset of the dimension count in a row.
pub const ARRAY_FRAME_ATTRIBUTE_ROW_DIMENSION_COUNT_OFFSET: usize = 48;
/// Byte offset of the residency code in a row: 0 CPU, 1 GPU pool slot.
pub const ARRAY_FRAME_ATTRIBUTE_ROW_RESIDENCY_OFFSET: usize = 52;
/// Byte offset of the shape, eight `u64` dimensions, unused ones zero.
pub const ARRAY_FRAME_ATTRIBUTE_ROW_SHAPE_OFFSET: usize = 56;
/// Byte offset of a CPU attribute's data offset into the payload.
pub const ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_OFFSET_OFFSET: usize = 120;
/// Byte offset of a CPU attribute's data length.
pub const ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_BYTE_LEN_OFFSET: usize = 128;
/// Byte offset of a GPU attribute's pool slot reference, a
/// [`GpuPoolSlotFrameDescriptorBody`]'s 40 bytes.
pub const ARRAY_FRAME_ATTRIBUTE_ROW_GPU_POOL_SLOT_OFFSET: usize = 136;
/// Bytes one attribute table row takes.
pub const ARRAY_FRAME_ATTRIBUTE_ROW_BYTES: usize = 176;
/// The alignment of each CPU attribute's data within the payload.
pub const ARRAY_FRAME_CPU_DATA_ALIGNMENT_BYTES: usize = 64;

/// Where one Array attribute's data lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayFrameAttributeResidency {
    /// In the payload, `byte_len` bytes from `payload_byte_offset`.
    Cpu {
        /// Offset from the payload's start.
        payload_byte_offset: u64,
        /// Length of the attribute's data.
        byte_len: u64,
    },
    /// In a GPU pool slot, by reference.
    GpuPoolSlot(GpuPoolSlotFrameDescriptorBody),
}

/// One row of an Array frame's attribute table: an attribute's name, element
/// type and shape, and where its data lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayFrameAttributeRow {
    /// The attribute's name.
    pub name: String,
    /// The element type's wire name, such as `float32`.
    pub dtype: String,
    /// The dimensions, outermost first.
    pub shape: Vec<u64>,
    /// Where the data lives.
    pub residency: ArrayFrameAttributeResidency,
}

/// Why an attribute table cannot be packed or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayFrameAttributeTableRefusal {
    /// What is wrong, naming the attribute.
    pub reason: String,
}

impl std::fmt::Display for ArrayFrameAttributeTableRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for ArrayFrameAttributeTableRefusal {}

fn zero_padded_utf8_into(
    row: &mut [u8],
    text: &str,
    field: &str,
    attribute_name: &str,
) -> Result<(), ArrayFrameAttributeTableRefusal> {
    let text_bytes = text.as_bytes();
    if text_bytes.is_empty() || text_bytes.len() > row.len() || text_bytes.contains(&0) {
        return Err(ArrayFrameAttributeTableRefusal {
            reason: format!(
                "Array attribute {attribute_name:?} has {field} {text:?}, which is not 1 to {} \
                 bytes of UTF-8 without a NUL",
                row.len()
            ),
        });
    }
    row[..text_bytes.len()].copy_from_slice(text_bytes);
    Ok(())
}

fn zero_padded_utf8_of(field: &[u8]) -> String {
    let len = field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..len]).into_owned()
}

fn u32_le_at(bytes: &[u8], offset: usize) -> u32 {
    let mut field = [0u8; 4];
    field.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(field)
}

fn u64_le_at(bytes: &[u8], offset: usize) -> u64 {
    let mut field = [0u8; 8];
    field.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(field)
}

impl ArrayFrameAttributeRow {
    /// This row as its [`ARRAY_FRAME_ATTRIBUTE_ROW_BYTES`] bytes, refused when
    /// the name or dtype does not fit or the shape has too many dimensions.
    pub fn to_row_bytes(
        &self,
    ) -> Result<[u8; ARRAY_FRAME_ATTRIBUTE_ROW_BYTES], ArrayFrameAttributeTableRefusal> {
        let mut row = [0u8; ARRAY_FRAME_ATTRIBUTE_ROW_BYTES];
        zero_padded_utf8_into(
            &mut row[..ARRAY_FRAME_ATTRIBUTE_NAME_BYTES],
            &self.name,
            "name",
            &self.name,
        )?;
        zero_padded_utf8_into(
            &mut row[ARRAY_FRAME_ATTRIBUTE_ROW_DTYPE_OFFSET
                ..ARRAY_FRAME_ATTRIBUTE_ROW_DIMENSION_COUNT_OFFSET],
            &self.dtype,
            "dtype",
            &self.name,
        )?;
        if self.shape.is_empty() || self.shape.len() > ARRAY_FRAME_ATTRIBUTE_MAX_DIMENSIONS {
            return Err(ArrayFrameAttributeTableRefusal {
                reason: format!(
                    "Array attribute {:?} has {} dimensions; an attribute has 1 to \
                     {ARRAY_FRAME_ATTRIBUTE_MAX_DIMENSIONS}",
                    self.name,
                    self.shape.len()
                ),
            });
        }
        row[ARRAY_FRAME_ATTRIBUTE_ROW_DIMENSION_COUNT_OFFSET
            ..ARRAY_FRAME_ATTRIBUTE_ROW_RESIDENCY_OFFSET]
            .copy_from_slice(&(self.shape.len() as u32).to_le_bytes());
        for (dimension_index, dimension) in self.shape.iter().enumerate() {
            let offset = ARRAY_FRAME_ATTRIBUTE_ROW_SHAPE_OFFSET + dimension_index * 8;
            row[offset..offset + 8].copy_from_slice(&dimension.to_le_bytes());
        }
        let residency_code: u32 = match self.residency {
            ArrayFrameAttributeResidency::Cpu {
                payload_byte_offset,
                byte_len,
            } => {
                row[ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_OFFSET_OFFSET
                    ..ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_BYTE_LEN_OFFSET]
                    .copy_from_slice(&payload_byte_offset.to_le_bytes());
                row[ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_BYTE_LEN_OFFSET
                    ..ARRAY_FRAME_ATTRIBUTE_ROW_GPU_POOL_SLOT_OFFSET]
                    .copy_from_slice(&byte_len.to_le_bytes());
                0
            }
            ArrayFrameAttributeResidency::GpuPoolSlot(slot) => {
                row[ARRAY_FRAME_ATTRIBUTE_ROW_GPU_POOL_SLOT_OFFSET
                    ..ARRAY_FRAME_ATTRIBUTE_ROW_GPU_POOL_SLOT_OFFSET
                        + GPU_POOL_SLOT_FRAME_BODY_USED_BYTES]
                    .copy_from_slice(
                        &slot.to_descriptor_body()[..GPU_POOL_SLOT_FRAME_BODY_USED_BYTES],
                    );
                1
            }
        };
        row[ARRAY_FRAME_ATTRIBUTE_ROW_RESIDENCY_OFFSET..ARRAY_FRAME_ATTRIBUTE_ROW_SHAPE_OFFSET]
            .copy_from_slice(&residency_code.to_le_bytes());
        Ok(row)
    }

    /// The row `row_bytes` packs, refused when its dimension count or
    /// residency code is out of range.
    pub fn from_row_bytes(
        row_bytes: &[u8; ARRAY_FRAME_ATTRIBUTE_ROW_BYTES],
    ) -> Result<Self, ArrayFrameAttributeTableRefusal> {
        let name = zero_padded_utf8_of(&row_bytes[..ARRAY_FRAME_ATTRIBUTE_NAME_BYTES]);
        let dimension_count =
            u32_le_at(row_bytes, ARRAY_FRAME_ATTRIBUTE_ROW_DIMENSION_COUNT_OFFSET) as usize;
        if dimension_count == 0 || dimension_count > ARRAY_FRAME_ATTRIBUTE_MAX_DIMENSIONS {
            return Err(ArrayFrameAttributeTableRefusal {
                reason: format!(
                    "Array attribute {name:?} states {dimension_count} dimensions; an attribute \
                     has 1 to {ARRAY_FRAME_ATTRIBUTE_MAX_DIMENSIONS}"
                ),
            });
        }
        let shape = (0..dimension_count)
            .map(|dimension_index| {
                u64_le_at(
                    row_bytes,
                    ARRAY_FRAME_ATTRIBUTE_ROW_SHAPE_OFFSET + dimension_index * 8,
                )
            })
            .collect();
        let residency = match u32_le_at(row_bytes, ARRAY_FRAME_ATTRIBUTE_ROW_RESIDENCY_OFFSET) {
            0 => ArrayFrameAttributeResidency::Cpu {
                payload_byte_offset: u64_le_at(
                    row_bytes,
                    ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_OFFSET_OFFSET,
                ),
                byte_len: u64_le_at(
                    row_bytes,
                    ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_BYTE_LEN_OFFSET,
                ),
            },
            1 => {
                let mut slot_body = [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES];
                slot_body[..GPU_POOL_SLOT_FRAME_BODY_USED_BYTES].copy_from_slice(
                    &row_bytes[ARRAY_FRAME_ATTRIBUTE_ROW_GPU_POOL_SLOT_OFFSET
                        ..ARRAY_FRAME_ATTRIBUTE_ROW_GPU_POOL_SLOT_OFFSET
                            + GPU_POOL_SLOT_FRAME_BODY_USED_BYTES],
                );
                ArrayFrameAttributeResidency::GpuPoolSlot(
                    GpuPoolSlotFrameDescriptorBody::from_descriptor_body(&slot_body),
                )
            }
            unknown => {
                return Err(ArrayFrameAttributeTableRefusal {
                    reason: format!(
                        "Array attribute {name:?} names residency code {unknown}, which is \
                         neither CPU (0) nor a GPU pool slot (1)"
                    ),
                });
            }
        };
        Ok(Self {
            name,
            dtype: zero_padded_utf8_of(
                &row_bytes[ARRAY_FRAME_ATTRIBUTE_ROW_DTYPE_OFFSET
                    ..ARRAY_FRAME_ATTRIBUTE_ROW_DIMENSION_COUNT_OFFSET],
            ),
            shape,
            residency,
        })
    }
}

/// The attribute table an Array payload starts with, `attribute_count` rows
/// read in order; refused when the payload is shorter than the table, or a
/// CPU row's data runs past the payload's end.
pub fn array_frame_attribute_table_of(
    body: ArrayFrameDescriptorBody,
    payload: &[u8],
) -> Result<Vec<ArrayFrameAttributeRow>, ArrayFrameAttributeTableRefusal> {
    let table_byte_len = (body.attribute_count as usize)
        .checked_mul(ARRAY_FRAME_ATTRIBUTE_ROW_BYTES)
        .filter(|table_byte_len| *table_byte_len <= payload.len())
        .ok_or_else(|| ArrayFrameAttributeTableRefusal {
            reason: format!(
                "an Array frame states {} attributes, whose table its {}-byte payload does not \
                 hold",
                body.attribute_count,
                payload.len()
            ),
        })?;
    payload[..table_byte_len]
        .as_chunks::<ARRAY_FRAME_ATTRIBUTE_ROW_BYTES>()
        .0
        .iter()
        .map(|row_bytes| {
            let row = ArrayFrameAttributeRow::from_row_bytes(row_bytes)?;
            if let ArrayFrameAttributeResidency::Cpu {
                payload_byte_offset,
                byte_len,
            } = row.residency
                && payload_byte_offset
                    .checked_add(byte_len)
                    .is_none_or(|end| end > payload.len() as u64)
            {
                return Err(ArrayFrameAttributeTableRefusal {
                    reason: format!(
                        "Array attribute {:?} states {byte_len} bytes at offset \
                         {payload_byte_offset}, past the end of its {}-byte payload",
                        row.name,
                        payload.len()
                    ),
                });
            }
            if let ArrayFrameAttributeResidency::Cpu {
                payload_byte_offset,
                byte_len,
            } = row.residency
                && byte_len != 0
                && payload_byte_offset < table_byte_len as u64
            {
                return Err(ArrayFrameAttributeTableRefusal {
                    reason: format!(
                        "Array attribute {:?} states data at offset {payload_byte_offset}, inside \
                         the {table_byte_len}-byte attribute table",
                        row.name
                    ),
                });
            }
            Ok(row)
        })
        .collect()
}

/// Where each CPU attribute's data starts in a payload whose table has
/// `attribute_count` rows, the data lengths given in row order, each start
/// aligned to [`ARRAY_FRAME_CPU_DATA_ALIGNMENT_BYTES`]; and the payload's
/// whole length.
pub fn array_frame_cpu_data_offsets(
    attribute_count: usize,
    cpu_data_byte_lens: &[u64],
) -> (Vec<u64>, u64) {
    let alignment = ARRAY_FRAME_CPU_DATA_ALIGNMENT_BYTES as u64;
    let mut next_offset = (attribute_count * ARRAY_FRAME_ATTRIBUTE_ROW_BYTES) as u64;
    let offsets = cpu_data_byte_lens
        .iter()
        .map(|byte_len| {
            let offset = next_offset.div_ceil(alignment) * alignment;
            next_offset = offset + byte_len;
            offset
        })
        .collect();
    (offsets, next_offset)
}

/// The most bytes one Records message's msgpack-encoded map may take.
pub const RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES: usize = 64 * 1024;

/// One CPU attribute an Array payload is packed from: its name, dtype wire
/// name, shape, and data bytes in row-major order.
#[derive(Clone, Copy, Debug)]
pub struct ArrayFrameCpuAttributeToPack<'data> {
    /// The attribute's name.
    pub name: &'data str,
    /// The element type's wire name, such as `float32`.
    pub dtype: &'data str,
    /// The dimensions, outermost first.
    pub shape: &'data [u64],
    /// The attribute's data.
    pub data: &'data [u8],
}

/// An Array frame's descriptor body and payload holding `attributes`, each
/// row in order and each attribute's data at its aligned offset; refused when
/// a row does not pack.
pub fn array_frame_payload_of_cpu_attributes(
    attributes: &[ArrayFrameCpuAttributeToPack<'_>],
) -> Result<(ArrayFrameDescriptorBody, Vec<u8>), ArrayFrameAttributeTableRefusal> {
    let data_byte_lens: Vec<u64> = attributes
        .iter()
        .map(|attribute| attribute.data.len() as u64)
        .collect();
    let (data_offsets, payload_byte_len) =
        array_frame_cpu_data_offsets(attributes.len(), &data_byte_lens);
    let mut payload = vec![0u8; payload_byte_len as usize];
    for (row_index, (attribute, data_offset)) in attributes.iter().zip(data_offsets).enumerate() {
        let row = ArrayFrameAttributeRow {
            name: attribute.name.to_string(),
            dtype: attribute.dtype.to_string(),
            shape: attribute.shape.to_vec(),
            residency: ArrayFrameAttributeResidency::Cpu {
                payload_byte_offset: data_offset,
                byte_len: attribute.data.len() as u64,
            },
        };
        let row_start = row_index * ARRAY_FRAME_ATTRIBUTE_ROW_BYTES;
        payload[row_start..row_start + ARRAY_FRAME_ATTRIBUTE_ROW_BYTES]
            .copy_from_slice(&row.to_row_bytes()?);
        let data_start = data_offset as usize;
        payload[data_start..data_start + attribute.data.len()].copy_from_slice(attribute.data);
    }
    Ok((
        ArrayFrameDescriptorBody {
            attribute_count: attributes.len() as u32,
        },
        payload,
    ))
}

#[cfg(test)]
mod tests;
