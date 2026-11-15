// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn the_engine_device_requests_no_video_queue_family() {
    let family_of = |queue_flags: vk::QueueFlags| vk::QueueFamilyProperties {
        queue_flags,
        queue_count: 1,
        ..Default::default()
    };
    let queue_families_of_a_discrete_gpu_with_video_engines = [
        family_of(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER),
        family_of(vk::QueueFlags::TRANSFER),
        family_of(vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER),
        family_of(vk::QueueFlags::TRANSFER | vk::QueueFlags::VIDEO_DECODE_KHR),
        family_of(vk::QueueFlags::TRANSFER | vk::QueueFlags::VIDEO_ENCODE_KHR),
    ];

    let selection = super::HostVulkanQueueFamilySelection::from_queue_families(
        &queue_families_of_a_discrete_gpu_with_video_engines,
    )
    .expect("a graphics family is present");

    assert_eq!(selection.distinct_queue_family_indices(), vec![0, 1, 2]);
    for family_index in selection.distinct_queue_family_indices() {
        assert!(
            !queue_families_of_a_discrete_gpu_with_video_engines[family_index as usize]
                .queue_flags
                .intersects(vk::QueueFlags::VIDEO_DECODE_KHR | vk::QueueFlags::VIDEO_ENCODE_KHR),
            "family {family_index} is a video family"
        );
    }
}

#[test]
fn a_device_whose_only_transfer_only_family_codes_video_transfers_on_graphics() {
    let family_of = |queue_flags: vk::QueueFlags| vk::QueueFamilyProperties {
        queue_flags,
        queue_count: 1,
        ..Default::default()
    };
    let selection = super::HostVulkanQueueFamilySelection::from_queue_families(&[
        family_of(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER),
        family_of(vk::QueueFlags::TRANSFER | vk::QueueFlags::VIDEO_DECODE_KHR),
    ])
    .expect("a graphics family is present");

    assert_eq!(selection.distinct_queue_family_indices(), vec![0]);
}
