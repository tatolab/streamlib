// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use iceoryx2::prelude::*;

use crate::{
    ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody, GpuPoolSlotFrameDescriptorBody,
    ImageFrameDescriptorBody, PacketsUnitCodecNameDoesNotFit, PacketsUnitDescriptorBody,
};

/// Whether a message opens a group or continues the one before it, carried in
/// [`DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageGroupBoundary {
    /// The message opens a group — a point a relay or a late joiner can start from.
    StartsGroup,
    /// The message continues the group the message before it opened.
    ContinuesGroup,
}

/// The alignment every channel sample's payload starts at, offset 0 of the
/// sample's payload area: a cache line, and what a SIMD load or a DLPack
/// consumer asks of a CPU tensor.
pub const DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES: usize = 64;

/// One cache line of a channel sample's payload, the element type of every
/// channel data service's slice.
///
/// The slice is of these rather than of `u8` under iceoryx2's
/// `payload_alignment`, because iceoryx2 strides every slice element at its
/// aligned size, which for a 64-byte-aligned `u8` would be 64 bytes a byte. A
/// payload of `n` bytes takes `n.div_ceil(64)` lines, and the user header's
/// [`DataChannelSampleUserHeader::payload_byte_len`] says where it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ZeroCopySend)]
#[type_name("DataChannelPayloadCacheLine")]
#[repr(C)]
#[repr(align(64))]
pub struct DataChannelPayloadCacheLine {
    /// The line's bytes.
    pub bytes: [u8; DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES],
}

/// The cache lines a `payload_byte_len`-byte payload takes.
pub const fn data_channel_payload_cache_lines_for(payload_byte_len: usize) -> usize {
    payload_byte_len.div_ceil(DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES)
}

/// Every byte of `cache_lines`, in order.
pub fn data_channel_payload_bytes_of(cache_lines: &[DataChannelPayloadCacheLine]) -> &[u8] {
    // SAFETY: `DataChannelPayloadCacheLine` is `repr(C)` over a byte array with
    // no padding (its size is its alignment), so a slice of them is that many
    // contiguous initialized bytes.
    unsafe {
        std::slice::from_raw_parts(
            cache_lines.as_ptr().cast::<u8>(),
            std::mem::size_of_val(cache_lines),
        )
    }
}

/// The [`DataChannelSampleUserHeader::flags`] bit set when the message opens a
/// group — a point a relay or a late joiner can start from. Bits 1..31 are
/// reserved: a writer writes them zero and a reader ignores them.
pub const DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP: u32 = 1 << 0;

/// Bytes in a [`DataChannelPayloadDescriptor`]'s kind-specific body.
pub const DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES: usize = 60;

/// The iceoryx2 user header every channel data sample carries: the sample's
/// place in its publisher's sends, what its writer declared lost upstream, its
/// stamp, its payload's length and group flag, and what the payload is.
///
/// The payload area carries nothing but payload. The iceoryx2 type name is
/// pinned because the default one carries the module path, and every opener
/// of a data service must present the same name or be refused
/// `IncompatibleTypes`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("DataChannelSampleUserHeader")]
#[repr(C)]
pub struct DataChannelSampleUserHeader {
    /// Counted from zero per publisher; consumed by every send that may have
    /// delivered.
    pub sequence_number: u64,
    /// Messages the writer declared lost upstream of it since its previous send
    /// that may have delivered — a loss no link of this channel caused.
    pub messages_lost_upstream_before_this_one: u64,
    /// The publish stamp, monotonic nanoseconds.
    pub timestamp_ns: i64,
    /// Bytes of payload the writer wrote; a reader never reads past it or past
    /// the sample's own slice.
    pub payload_byte_len: u32,
    /// [`DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP`]; every other bit is reserved zero.
    pub flags: u32,
    /// What the payload is.
    pub payload_descriptor: DataChannelPayloadDescriptor,
}

impl DataChannelSampleUserHeader {
    /// A header for a `payload_byte_len`-byte message of `payload_descriptor`,
    /// stamped `timestamp_ns` and `group_boundary`, before its numbering is set.
    pub fn new(
        timestamp_ns: i64,
        payload_byte_len: u32,
        group_boundary: MessageGroupBoundary,
        payload_descriptor: DataChannelPayloadDescriptor,
    ) -> Self {
        let flags = match group_boundary {
            MessageGroupBoundary::StartsGroup => DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP,
            MessageGroupBoundary::ContinuesGroup => 0,
        };
        Self {
            sequence_number: 0,
            messages_lost_upstream_before_this_one: 0,
            timestamp_ns,
            payload_byte_len,
            flags,
            payload_descriptor,
        }
    }

    /// Whether the message opens a group, read from
    /// [`DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP`].
    pub fn group_boundary(&self) -> MessageGroupBoundary {
        if self.flags & DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP != 0 {
            MessageGroupBoundary::StartsGroup
        } else {
            MessageGroupBoundary::ContinuesGroup
        }
    }

    /// The payload this header stamps, out of the sample's `sample_payload`
    /// slice, refused where the stamped length runs past it.
    ///
    /// The stamped length is a `u32` off the wire, so it bounds the read rather
    /// than being trusted by it.
    pub fn payload_within<'sample>(
        &self,
        sample_payload: &'sample [u8],
    ) -> Result<&'sample [u8], DataChannelSamplePayloadRefusal> {
        sample_payload.get(..self.payload_byte_len as usize).ok_or(
            DataChannelSamplePayloadRefusal::StampedLengthPastTheSample {
                stamped_payload_byte_len: self.payload_byte_len,
                sample_payload_byte_len: sample_payload.len(),
            },
        )
    }
}

/// Why a channel sample holds no payload a reader may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataChannelSamplePayloadRefusal {
    /// The user header stamps a longer payload than the sample carries.
    StampedLengthPastTheSample {
        stamped_payload_byte_len: u32,
        sample_payload_byte_len: usize,
    },
    /// The descriptor names a kind this reader does not know.
    UnknownPayloadDescriptorKind { kind: u32 },
}

impl std::fmt::Display for DataChannelSamplePayloadRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StampedLengthPastTheSample {
                stamped_payload_byte_len,
                sample_payload_byte_len,
            } => write!(
                f,
                "the sample's user header declares a {stamped_payload_byte_len}-byte payload but \
                 the sample carries only {sample_payload_byte_len} bytes"
            ),
            Self::UnknownPayloadDescriptorKind { kind } => write!(
                f,
                "the sample's payload descriptor names kind {kind}, which this reader does not \
                 know"
            ),
        }
    }
}

impl std::error::Error for DataChannelSamplePayloadRefusal {}

/// The kinds a [`DataChannelPayloadDescriptor`] names, by their wire value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DataChannelPayloadDescriptorKind {
    /// The payload is the sample's bytes; the body is zero.
    Bytes = 0,
    /// The payload names a slot of a GPU pool; the body is a
    /// [`GpuPoolSlotFrameDescriptorBody`].
    GpuPoolSlotFrame = 1,
    /// An Image frame; the body is an [`ImageFrameDescriptorBody`] and the
    /// payload is empty.
    ImageFrame = 2,
    /// A Channels block; the body is a [`ChannelsBlockDescriptorBody`] and
    /// the payload is its planar samples.
    ChannelsBlock = 3,
    /// A Packets unit; the body is a [`PacketsUnitDescriptorBody`] and the
    /// payload is the unit's bytes.
    PacketsUnit = 4,
    /// A Records message; the body is zero and the payload is the record,
    /// msgpack-encoded by the runtime.
    RecordsMessage = 5,
    /// An Array frame; the body is an [`ArrayFrameDescriptorBody`] and the
    /// payload starts with its attribute table, then each CPU attribute's data.
    ArrayFrame = 6,
}

impl DataChannelPayloadDescriptorKind {
    /// The kind `wire_kind` names, or `None` for one this build does not know.
    pub fn from_wire(wire_kind: u32) -> Option<Self> {
        match wire_kind {
            0 => Some(Self::Bytes),
            1 => Some(Self::GpuPoolSlotFrame),
            2 => Some(Self::ImageFrame),
            3 => Some(Self::ChannelsBlock),
            4 => Some(Self::PacketsUnit),
            5 => Some(Self::RecordsMessage),
            6 => Some(Self::ArrayFrame),
            _ => None,
        }
    }
}

/// What a channel sample's payload is: a kind and a fixed-size body the kind
/// gives a layout to. A reader refuses a kind it does not know, by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ZeroCopySend)]
#[type_name("DataChannelPayloadDescriptor")]
#[repr(C)]
pub struct DataChannelPayloadDescriptor {
    /// A [`DataChannelPayloadDescriptorKind`] wire value.
    pub kind: u32,
    /// The kind's body, little-endian; reserved bytes are zero.
    pub body: [u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
}

impl DataChannelPayloadDescriptor {
    /// The descriptor of a payload that is the sample's bytes.
    pub const fn bytes() -> Self {
        Self {
            kind: DataChannelPayloadDescriptorKind::Bytes as u32,
            body: [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
        }
    }

    /// The descriptor of a payload naming one GPU pool slot.
    pub fn gpu_pool_slot_frame(body: GpuPoolSlotFrameDescriptorBody) -> Self {
        Self {
            kind: DataChannelPayloadDescriptorKind::GpuPoolSlotFrame as u32,
            body: body.to_descriptor_body(),
        }
    }

    /// The descriptor of an Image frame.
    pub fn image_frame(body: ImageFrameDescriptorBody) -> Self {
        Self {
            kind: DataChannelPayloadDescriptorKind::ImageFrame as u32,
            body: body.to_descriptor_body(),
        }
    }

    /// The descriptor of a Channels block.
    pub fn channels_block(body: ChannelsBlockDescriptorBody) -> Self {
        Self {
            kind: DataChannelPayloadDescriptorKind::ChannelsBlock as u32,
            body: body.to_descriptor_body(),
        }
    }

    /// The descriptor of a Packets unit, refused when its codec name does not fit.
    pub fn packets_unit(
        body: &PacketsUnitDescriptorBody,
    ) -> Result<Self, PacketsUnitCodecNameDoesNotFit> {
        Ok(Self {
            kind: DataChannelPayloadDescriptorKind::PacketsUnit as u32,
            body: body.to_descriptor_body()?,
        })
    }

    /// The descriptor of a Records message.
    pub const fn records_message() -> Self {
        Self {
            kind: DataChannelPayloadDescriptorKind::RecordsMessage as u32,
            body: [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
        }
    }

    /// The descriptor of an Array frame.
    pub fn array_frame(body: ArrayFrameDescriptorBody) -> Self {
        Self {
            kind: DataChannelPayloadDescriptorKind::ArrayFrame as u32,
            body: body.to_descriptor_body(),
        }
    }

    /// The kind this descriptor names, refused by kind when this build does not
    /// know it.
    pub fn known_kind(
        &self,
    ) -> Result<DataChannelPayloadDescriptorKind, DataChannelSamplePayloadRefusal> {
        DataChannelPayloadDescriptorKind::from_wire(self.kind).ok_or(
            DataChannelSamplePayloadRefusal::UnknownPayloadDescriptorKind { kind: self.kind },
        )
    }
}

impl Default for DataChannelPayloadDescriptor {
    fn default() -> Self {
        Self::bytes()
    }
}

#[cfg(test)]
mod tests;
