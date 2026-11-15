// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::vulkan::rhi::device::drm_modifier_probe;

/// The device number of `physical_device`'s DRM render node, or `None` when it
/// advertises no `VK_EXT_physical_device_drm` or has no render node.
pub(super) fn drm_render_node_number_of_the_physical_device(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    advertises_physical_device_drm: bool,
) -> Option<drm_modifier_probe::DrmRenderNodeNumber> {
    if !advertises_physical_device_drm {
        return None;
    }
    let mut drm_properties = vk::PhysicalDeviceDrmPropertiesEXT::builder().build();
    let mut properties2 = vk::PhysicalDeviceProperties2::builder()
        .push_next(&mut drm_properties)
        .build();
    unsafe { instance.get_physical_device_properties2(physical_device, &mut properties2) };
    (drm_properties.has_render == vk::TRUE).then_some(drm_modifier_probe::DrmRenderNodeNumber {
        major: drm_properties.render_major,
        minor: drm_properties.render_minor,
    })
}

/// Whether `physical_device` can export a timeline semaphore as an OPAQUE_FD.
pub(super) fn physical_device_exports_an_opaque_fd_timeline_semaphore(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> bool {
    let mut timeline_semaphore_type = vk::SemaphoreTypeCreateInfo::builder()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0)
        .build();
    let external_semaphore_info = vk::PhysicalDeviceExternalSemaphoreInfo::builder()
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
        .push_next(&mut timeline_semaphore_type)
        .build();
    let mut external_semaphore_properties = vk::ExternalSemaphoreProperties::default();
    unsafe {
        instance.get_physical_device_external_semaphore_properties(
            physical_device,
            &external_semaphore_info,
            &mut external_semaphore_properties,
        )
    };
    external_semaphore_properties
        .external_semaphore_features
        .contains(vk::ExternalSemaphoreFeatureFlags::EXPORTABLE)
}
