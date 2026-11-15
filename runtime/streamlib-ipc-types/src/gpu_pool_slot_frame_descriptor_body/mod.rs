// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES;

/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::pool`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET: usize = 0;
/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::slot`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET: usize = 16;
/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::generation`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET: usize = 20;
/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::timeline_value`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET: usize = 28;
/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::image_layout`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_IMAGE_LAYOUT_OFFSET: usize = 36;
/// Bytes of a descriptor body the GPU pool slot frame uses; the rest are
/// reserved zero.
pub const GPU_POOL_SLOT_FRAME_BODY_USED_BYTES: usize = 40;

/// The body of a [`DataChannelPayloadDescriptorKind::GpuPoolSlotFrame`](crate::DataChannelPayloadDescriptorKind::GpuPoolSlotFrame)
/// descriptor: which pool, which slot, which hand-out of it, the timeline
/// value its writes signal, and the image layout it is left in. Packed little-endian into the descriptor body at
/// the `GPU_POOL_SLOT_FRAME_BODY_*_OFFSET` offsets, because the body is a byte
/// array at a 4-byte offset and cannot hold an 8-byte-aligned field in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPoolSlotFrameDescriptorBody {
    /// The pool's UUID.
    pub pool: [u8; 16],
    /// The slot's index in the pool.
    pub slot: u32,
    /// Which hand-out of the slot the frame is.
    pub generation: u64,
    /// The timeline semaphore value the frame's writes signal.
    pub timeline_value: u64,
    /// The `VkImageLayout` the producer left the slot's image in, as its raw
    /// value; `VK_IMAGE_LAYOUT_UNDEFINED` (0) for a buffer slot.
    pub image_layout: i32,
}

impl GpuPoolSlotFrameDescriptorBody {
    /// This body as descriptor body bytes, the reserved tail zero.
    pub fn to_descriptor_body(&self) -> [u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES] {
        let mut body = [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        body[GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET..GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET]
            .copy_from_slice(&self.pool);
        body[GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET..GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET]
            .copy_from_slice(&self.slot.to_le_bytes());
        body[GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET
            ..GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET]
            .copy_from_slice(&self.generation.to_le_bytes());
        body[GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET
            ..GPU_POOL_SLOT_FRAME_BODY_IMAGE_LAYOUT_OFFSET]
            .copy_from_slice(&self.timeline_value.to_le_bytes());
        body[GPU_POOL_SLOT_FRAME_BODY_IMAGE_LAYOUT_OFFSET..GPU_POOL_SLOT_FRAME_BODY_USED_BYTES]
            .copy_from_slice(&self.image_layout.to_le_bytes());
        body
    }

    /// The body `descriptor_body` packs.
    pub fn from_descriptor_body(
        descriptor_body: &[u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    ) -> Self {
        let field = |start: usize, end: usize| &descriptor_body[start..end];
        let mut pool = [0u8; 16];
        pool.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET,
        ));
        let mut slot = [0u8; 4];
        slot.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET,
        ));
        let mut generation = [0u8; 8];
        generation.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET,
        ));
        let mut timeline_value = [0u8; 8];
        timeline_value.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_IMAGE_LAYOUT_OFFSET,
        ));
        let mut image_layout = [0u8; 4];
        image_layout.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_IMAGE_LAYOUT_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_USED_BYTES,
        ));
        Self {
            pool,
            slot: u32::from_le_bytes(slot),
            generation: u64::from_le_bytes(generation),
            timeline_value: u64::from_le_bytes(timeline_value),
            image_layout: i32::from_le_bytes(image_layout),
        }
    }
}

#[cfg(test)]
mod tests;
