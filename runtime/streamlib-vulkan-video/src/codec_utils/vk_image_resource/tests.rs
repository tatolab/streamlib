// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn test_ycbcr_vk_format_info_nv12() {
    let info = ycbcr_vk_format_info(vk::Format::G8_B8R8_2PLANE_420_UNORM);
    assert!(info.is_some());
    let info = info.unwrap();
    assert_eq!(
        info.planes_layout.layout,
        YcbcrPlanesLayout::SemiPlanarCbCrInterleaved
    );
    assert_eq!(info.planes_layout.bpp, YcbcrBpp::Bpp8);
    assert!(info.planes_layout.secondary_plane_subsampled_x);
    assert!(info.planes_layout.secondary_plane_subsampled_y);
    assert_eq!(info.planes_layout.number_of_extra_planes, 1);
    assert_eq!(info.vk_plane_format[0], vk::Format::R8_UNORM);
    assert_eq!(info.vk_plane_format[1], vk::Format::R8G8_UNORM);
}

#[test]
fn test_ycbcr_vk_format_info_p010() {
    let info = ycbcr_vk_format_info(vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16);
    assert!(info.is_some());
    let info = info.unwrap();
    assert_eq!(info.planes_layout.bpp, YcbcrBpp::Bpp10);
    assert_eq!(info.planes_layout.number_of_extra_planes, 1);
}

#[test]
fn test_ycbcr_vk_format_info_3plane() {
    let info = ycbcr_vk_format_info(vk::Format::G8_B8_R8_3PLANE_420_UNORM);
    assert!(info.is_some());
    let info = info.unwrap();
    assert_eq!(info.planes_layout.number_of_extra_planes, 2);
    assert_eq!(
        info.planes_layout.layout,
        YcbcrPlanesLayout::PlanarCbCrBlockJoined
    );
}

#[test]
fn test_ycbcr_vk_format_info_non_ycbcr() {
    assert!(ycbcr_vk_format_info(vk::Format::R8G8B8A8_UNORM).is_none());
    assert!(ycbcr_vk_format_info(vk::Format::B8G8R8A8_SRGB).is_none());
    assert!(ycbcr_vk_format_info(vk::Format::UNDEFINED).is_none());
}

#[test]
fn test_is_16bit_detection() {
    // 8-bit format should not be 16-bit
    let nv12 = ycbcr_vk_format_info(vk::Format::G8_B8R8_2PLANE_420_UNORM).unwrap();
    assert_eq!(nv12.planes_layout.bpp, YcbcrBpp::Bpp8);

    // 10-bit format should report as 16-bit (non-8bpp treated as 16bpp)
    let p010 = ycbcr_vk_format_info(vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16).unwrap();
    assert_ne!(p010.planes_layout.bpp, YcbcrBpp::Bpp8);
}

#[test]
fn test_is_compatible_logic() {
    // Test the pure logic of is_compatible by constructing two ImageCreateInfo structs.
    let base = vk::ImageCreateInfo {
        s_type: vk::StructureType::IMAGE_CREATE_INFO,
        next: std::ptr::null(),
        flags: vk::ImageCreateFlags::empty(),
        image_type: vk::ImageType::_2D,
        format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
        extent: vk::Extent3D {
            width: 1920,
            height: 1080,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: vk::SampleCountFlags::_1,
        tiling: vk::ImageTiling::OPTIMAL,
        usage: vk::ImageUsageFlags::SAMPLED,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        queue_family_index_count: 0,
        queue_family_indices: std::ptr::null(),
        initial_layout: vk::ImageLayout::UNDEFINED,
    };

    // Same or smaller should be compatible
    let smaller = vk::ImageCreateInfo {
        extent: vk::Extent3D {
            width: 1280,
            height: 720,
            depth: 1,
        },
        ..base
    };

    // Use a mock resource to test is_compatible
    let resource = VkImageResource::new_test_stub(base);

    assert!(resource.is_compatible(&smaller));
    assert!(resource.is_compatible(&base));

    // Larger width should NOT be compatible
    let larger = vk::ImageCreateInfo {
        extent: vk::Extent3D {
            width: 3840,
            height: 2160,
            depth: 1,
        },
        ..base
    };
    assert!(!resource.is_compatible(&larger));

    // Different format should NOT be compatible
    let diff_format = vk::ImageCreateInfo {
        format: vk::Format::R8G8B8A8_UNORM,
        ..base
    };
    assert!(!resource.is_compatible(&diff_format));

    // Different tiling should NOT be compatible
    let diff_tiling = vk::ImageCreateInfo {
        tiling: vk::ImageTiling::LINEAR,
        ..base
    };
    assert!(!resource.is_compatible(&diff_tiling));

    // Different image type should NOT be compatible
    let diff_type = vk::ImageCreateInfo {
        image_type: vk::ImageType::_3D,
        ..base
    };
    assert!(!resource.is_compatible(&diff_type));
}

#[test]
fn test_drm_format_modifier_detection() {
    // Test that uses_drm_format_modifier is set correctly
    let base_info = vk::ImageCreateInfo {
        s_type: vk::StructureType::IMAGE_CREATE_INFO,
        next: std::ptr::null(),
        flags: vk::ImageCreateFlags::empty(),
        image_type: vk::ImageType::_2D,
        format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
        extent: vk::Extent3D {
            width: 1920,
            height: 1080,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: vk::SampleCountFlags::_1,
        tiling: vk::ImageTiling::OPTIMAL,
        usage: vk::ImageUsageFlags::SAMPLED,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        queue_family_index_count: 0,
        queue_family_indices: std::ptr::null(),
        initial_layout: vk::ImageLayout::UNDEFINED,
    };

    // No DRM modifier
    let r1 = VkImageResource::new_inner(
        device_with_no_loaded_commands(),
        &base_info,
        vk::Image::null(),
        0,
        0,
        None,
        None,
        None,
        0,
        0,
    );
    assert!(!r1.uses_drm_format_modifier);

    // With DRM modifier value
    let r2 = VkImageResource::new_inner(
        device_with_no_loaded_commands(),
        &base_info,
        vk::Image::null(),
        0,
        0,
        None,
        None,
        None,
        1,
        2,
    );
    assert!(r2.uses_drm_format_modifier);
    assert_eq!(r2.drm_format_modifier, 1);
    assert_eq!(r2.memory_plane_count, 2);

    // With DRM tiling
    let drm_info = vk::ImageCreateInfo {
        tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
        ..base_info
    };
    let r3 = VkImageResource::new_inner(
        device_with_no_loaded_commands(),
        &drm_info,
        vk::Image::null(),
        0,
        0,
        None,
        None,
        None,
        0,
        0,
    );
    assert!(r3.uses_drm_format_modifier);
}

#[test]
fn test_plane_layout_bounds() {
    let mut resource = VkImageResource::new_test_stub(vk::ImageCreateInfo {
        s_type: vk::StructureType::IMAGE_CREATE_INFO,
        next: std::ptr::null(),
        flags: vk::ImageCreateFlags::empty(),
        image_type: vk::ImageType::_2D,
        format: vk::Format::R8G8B8A8_UNORM,
        extent: vk::Extent3D {
            width: 64,
            height: 64,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: vk::SampleCountFlags::_1,
        tiling: vk::ImageTiling::LINEAR,
        usage: vk::ImageUsageFlags::SAMPLED,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        queue_family_index_count: 0,
        queue_family_indices: std::ptr::null(),
        initial_layout: vk::ImageLayout::UNDEFINED,
    });
    resource.is_linear_image = true;

    // plane_index > 2 should return None
    assert!(resource.get_plane_layout(3).is_none());
    // Default zero layout should return None (size=0 and row_pitch=0)
    assert!(resource.get_plane_layout(0).is_none());

    // Non-linear should return None for get_subresource_layout
    let non_linear = VkImageResource::new_test_stub(vk::ImageCreateInfo::default());
    assert!(non_linear.get_subresource_layout().is_none());
    assert!(non_linear.get_plane_layout(0).is_none());
}

#[test]
fn test_memory_plane_layout_bounds() {
    let resource = VkImageResource {
        device: device_with_no_loaded_commands(),
        image: vk::Image::null(),
        image_create_info: vk::ImageCreateInfo {
            s_type: vk::StructureType::IMAGE_CREATE_INFO,
            next: std::ptr::null(),
            flags: vk::ImageCreateFlags::empty(),
            image_type: vk::ImageType::_2D,
            format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            extent: vk::Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::_1,
            tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
            usage: vk::ImageUsageFlags::SAMPLED,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            queue_family_index_count: 0,
            queue_family_indices: std::ptr::null(),
            initial_layout: vk::ImageLayout::UNDEFINED,
        },
        image_offset: 0,
        image_size: 0,
        device_memory: None,
        allocator: None,
        allocation: None,
        layouts: [vk::SubresourceLayout::default(); 3],
        memory_plane_layouts: [
            vk::SubresourceLayout {
                offset: 0,
                size: 1920 * 1080,
                row_pitch: 1920,
                array_pitch: 0,
                depth_pitch: 0,
            },
            vk::SubresourceLayout {
                offset: 1920 * 1080,
                size: 1920 * 540,
                row_pitch: 1920,
                array_pitch: 0,
                depth_pitch: 0,
            },
            vk::SubresourceLayout::default(),
            vk::SubresourceLayout::default(),
        ],
        drm_format_modifier: 1,
        memory_plane_count: 2,
        is_linear_image: false,
        is_16bit: false,
        is_subsampled_x: true,
        is_subsampled_y: true,
        uses_drm_format_modifier: true,
        owns_resources: false,
    };

    // Valid planes
    assert!(resource.get_memory_plane_layout(0).is_some());
    assert!(resource.get_memory_plane_layout(1).is_some());
    // Out of range
    assert!(resource.get_memory_plane_layout(2).is_none());
    assert!(resource.get_memory_plane_layout(4).is_none());

    // Verify layout values
    let l0 = resource.get_memory_plane_layout(0).unwrap();
    assert_eq!(l0.offset, 0);
    assert_eq!(l0.size, 1920 * 1080);
    let l1 = resource.get_memory_plane_layout(1).unwrap();
    assert_eq!(l1.offset, 1920 * 1080);
}
