// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::TiledDmaBufTextureOfALookupReply;
use streamlib_consumer_rhi::VulkanImageUsage;

#[test]
fn a_tiled_texture_reply_names_its_plane_its_allocation_and_its_usage() {
    let reply = serde_json::json!({
        "resource_type": "texture",
        "drm_format_modifier": 0x0300_0000_0060_6014u64,
        "plane_sizes": [8_388_608u64],
        "plane_offsets": [0u64],
        "plane_strides": [7680u64],
        "vk_image_usage": 0x05,
        "vk_image_allocation_size": 8_294_400u64,
    });
    assert_eq!(
        TiledDmaBufTextureOfALookupReply::of(&reply).unwrap(),
        TiledDmaBufTextureOfALookupReply {
            plane_offset_bytes: 0,
            plane_stride_bytes: 7680,
            allocation_byte_size: 8_388_608,
            usage: VulkanImageUsage(0x05),
        }
    );
}

#[test]
fn a_tiled_texture_reply_without_one_plane_stride_is_refused() {
    let reply = serde_json::json!({
        "drm_format_modifier": 0x0300_0000_0060_6014u64,
        "plane_sizes": [8_388_608u64],
        "plane_offsets": [0u64],
        "plane_strides": [],
    });
    let refusal = TiledDmaBufTextureOfALookupReply::of(&reply)
        .expect_err("a tiled image needs its plane's stride");
    assert!(refusal.to_string().contains("plane_strides"), "{refusal}");
}
