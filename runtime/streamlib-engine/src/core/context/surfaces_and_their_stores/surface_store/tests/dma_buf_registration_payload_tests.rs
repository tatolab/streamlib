// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
use crate::core::test_support::rig_only_capability_skip::{
    RigOnlyCapability, skip_because_this_machine_lacks_naming_the_refusal,
    skip_unless_this_machine_has,
};
use crate::vulkan::rhi::{HostVulkanDevice, HostVulkanTexture};

/// The wire half of #1915: a surface whose driver-chosen modifier is
/// `DRM_FORMAT_MOD_LINEAR` publishes a zero modifier, which reads exactly
/// like the zero published by an image that never went through
/// `VK_EXT_image_drm_format_modifier`. Only the tiling beside it tells a
/// consumer which one it is holding, so the payload must carry the
/// texture's recorded tiling rather than assume one.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_linear_modifier_surface_publishes_the_tiling_that_reads_its_modifier() {
    const DRM_FORMAT_MOD_LINEAR: u64 = 0;
    const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: i64 = 1_000_158_000;

    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Skipping — no Vulkan device: {e}");
            return;
        }
    };
    if skip_unless_this_machine_has(
        RigOnlyCapability::DmaBufModifiers,
        device.dma_buf_image_pool_tiled().is_some(),
    ) {
        return;
    }
    let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm).with_usage(
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
    );
    let texture = match HostVulkanTexture::new_render_target_dma_buf(
        &device,
        &desc,
        &[DRM_FORMAT_MOD_LINEAR],
    ) {
        Ok(t) => t,
        Err(e) => {
            skip_because_this_machine_lacks_naming_the_refusal(
                RigOnlyCapability::DmaBufModifiers,
                format!("the driver refused DRM_FORMAT_MOD_LINEAR: {e}"),
            );
            return;
        }
    };

    let payload = dma_buf_texture_registration_payload(
        &texture,
        "surface-under-test",
        "runtime-under-test",
        false,
        false,
        streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
    );

    assert_eq!(
        payload.get("drm_format_modifier").and_then(|v| v.as_u64()),
        Some(DRM_FORMAT_MOD_LINEAR),
        "the driver chose LINEAR, whose modifier value is zero"
    );
    assert_eq!(
        payload.get("vk_image_tiling").and_then(|v| v.as_i64()),
        Some(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT),
        "without this, the zero modifier above is indistinguishable on the \
         wire from an image that never carried a modifier at all"
    );
}
