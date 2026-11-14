// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn test_semaphore_value_encoding() {
    // Verify that different sync types produce distinct upper nibbles.
    let v_decode = get_semaphore_value(SemSyncTypeIdx::Decode, 42);
    let v_filter = get_semaphore_value(SemSyncTypeIdx::Filter, 42);
    let v_display = get_semaphore_value(SemSyncTypeIdx::Display, 42);

    assert_ne!(v_decode, v_filter);
    assert_ne!(v_filter, v_display);

    // Lower 60 bits should encode the frame order.
    assert_eq!(v_decode & 0x0FFF_FFFF_FFFF_FFFF, 42);
    assert_eq!(v_filter & 0x0FFF_FFFF_FFFF_FFFF, 42);
    assert_eq!(v_display & 0x0FFF_FFFF_FFFF_FFFF, 42);

    // Same type + order should always produce the same value.
    assert_eq!(
        get_semaphore_value(SemSyncTypeIdx::Decode, 100),
        get_semaphore_value(SemSyncTypeIdx::Decode, 100),
    );
}

#[test]
fn test_decoded_frame_release_default() {
    let r = DecodedFrameRelease::default();
    assert_eq!(r.picture_index, 0);
    assert!(!r.has_consumer_signal_fence);
    assert!(!r.has_consumer_signal_semaphore);
}

#[test]
fn test_per_frame_decode_resources_lifecycle() {
    let mut res = PerFrameDecodeResources::default();
    assert!(res.is_available());

    res.add_ref();
    assert!(!res.is_available());

    res.release();
    assert!(res.is_available());
}

#[test]
fn test_per_frame_decode_resources_reset() {
    let mut res = PerFrameDecodeResources::default();
    res.pic_idx = 5;
    res.in_decode_queue = true;
    res.add_ref();

    res.reset();
    assert_eq!(res.pic_idx, -1);
    assert!(!res.in_decode_queue);
    assert!(res.is_available());
}

#[test]
fn test_image_exist_invalid() {
    let res = PerFrameDecodeResources::default();
    assert!(!res.image_exist(INVALID_IMAGE_TYPE_IDX));
    assert!(!res.image_exist(0));
    assert!(!res.image_exist(MAX_PER_FRAME_IMAGE_TYPES as u8));
}

#[test]
fn test_frame_synchronization_info_default() {
    let info = FrameSynchronizationInfo::default();
    assert_eq!(info.frame_complete_fence, vk::Fence::null());
    assert!(!info.has_frame_complete_signal_fence);
    assert!(!info.sync_on_frame_complete_fence);
}

#[test]
fn test_picture_resource_info_default() {
    let info = PictureResourceInfo::default();
    assert_eq!(info.image, vk::Image::null());
    assert_eq!(info.image_format, vk::Format::UNDEFINED);
    assert_eq!(info.current_image_layout, vk::ImageLayout::UNDEFINED);
}

#[test]
fn test_init_type_values() {
    assert_eq!(InitType::InvalidateImagesLayout as u8, 0);
    assert_eq!(InitType::RecreateImages as u8, 2);
    assert_eq!(InitType::IncreaseNumSlots as u8, 4);
}

#[test]
fn test_per_frame_image_set_default() {
    let set = PerFrameDecodeImageSet::default();
    assert_eq!(set.size(), 0);
    assert_eq!(set.frame_complete_semaphore, vk::Semaphore::null());
    assert_eq!(set.consumer_complete_semaphore, vk::Semaphore::null());
    assert_eq!(set.num_external_consumers, 0);
}

#[test]
fn test_get_image_set_new_layout_needs_recreation() {
    let mut res = PerFrameDecodeResources::default();
    // No images exist, so should return false (needs creation).
    assert!(!res.get_image_set_new_layout(0, vk::ImageLayout::GENERAL, None));
}

#[test]
fn test_invalidate_image_layout() {
    let mut res = PerFrameDecodeResources::default();
    res.invalidate_image_layout(0);
    // Verify we don't panic on valid index.
    res.invalidate_image_layout(MAX_PER_FRAME_IMAGE_TYPES as u8 - 1);
}

#[test]
fn test_vulkan_decoded_frame_default() {
    let frame = VulkanDecodedFrame::default();
    assert_eq!(frame.picture_index, 0);
    assert_eq!(frame.frame_complete_fence, vk::Fence::null());
    assert_eq!(frame.num_external_consumers, 0);
}

#[test]
fn test_sem_sync_type_idx_default() {
    let s = SemSyncTypeIdx::default();
    assert_eq!(s, SemSyncTypeIdx::Decode);
}
