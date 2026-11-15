// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::*;

#[test]
fn the_gpu_pool_slot_frame_body_packs_every_field_at_its_documented_offset() {
    assert_eq!(
        [
            GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_IMAGE_LAYOUT_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_USED_BYTES,
        ],
        [0, 16, 20, 28, 36, 40]
    );
    let frame = GpuPoolSlotFrameDescriptorBody {
        pool: *b"0123456789abcdef",
        slot: 0x0403_0201,
        generation: 0x0c0b_0a09_0807_0605,
        timeline_value: u64::MAX - 1,
        image_layout: 1_000_001_002,
    };
    let descriptor = DataChannelPayloadDescriptor::gpu_pool_slot_frame(frame);
    assert_eq!(
        descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::GpuPoolSlotFrame)
    );
    assert_eq!(&descriptor.body[0..16], b"0123456789abcdef");
    assert_eq!(&descriptor.body[16..20], &[1, 2, 3, 4]);
    assert_eq!(&descriptor.body[20..28], &[5, 6, 7, 8, 9, 10, 11, 12]);
    assert_eq!(&descriptor.body[28..36], &(u64::MAX - 1).to_le_bytes());
    assert_eq!(&descriptor.body[36..40], &1_000_001_002i32.to_le_bytes());
    assert!(descriptor.body[40..].iter().all(|&reserved| reserved == 0));
    assert_eq!(
        GpuPoolSlotFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        frame
    );
}
