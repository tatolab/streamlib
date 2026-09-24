// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MoltenVK contract for a `VkImage` over an IOSurface, shared by the
//! engine that allocates one and the helper that imports one.
//!
//! MoltenVK binds the surface when the image is created — there is no
//! attach after the fact — and checks only the extent and the element size
//! against the format's block size. The image's storage is the surface's own
//! linear rows whatever tiling it declares, and it still takes a memory
//! binding, which must be device-local and not host-visible: MoltenVK backs a
//! host-visible binding with a private `MTLBuffer` of its own for every image.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::TextureFormat;

/// Why `iosurface` cannot back a `format` image, or `None` when its single
/// plane's element size is the format's block size.
pub fn refusal_of_an_iosurface_for_an_image_of_format(
    iosurface: &objc2_io_surface::IOSurfaceRef,
    format: TextureFormat,
) -> Option<String> {
    (format.plane_count() != 1
        || iosurface.bytes_per_element() != format.bytes_per_pixel() as usize)
        .then(|| {
            format!(
                "the IOSurface's {}-byte elements are not {format:?}'s single {}-byte plane",
                iosurface.bytes_per_element(),
                format.bytes_per_pixel()
            )
        })
}

/// Create a `width`x`height` `OPTIMAL` image over `iosurface` through
/// `VkImportMetalIOSurfaceInfoEXT`, unbound.
///
/// # Safety
///
/// `device` must have `VK_EXT_metal_objects` enabled, and `iosurface` must
/// outlive the returned image.
pub unsafe fn create_image_over_iosurface(
    device: &vulkanalia::Device,
    iosurface: &objc2_io_surface::IOSurfaceRef,
    width: u32,
    height: u32,
    format: vk::Format,
    usage_flags: vk::ImageUsageFlags,
) -> vulkanalia::VkResult<vk::Image> {
    let mut import_iosurface_info = vk::ImportMetalIOSurfaceInfoEXT::builder()
        .io_surface(std::ptr::from_ref(iosurface).cast_mut().cast())
        .build();
    let image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(format)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(usage_flags)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut import_iosurface_info);
    // SAFETY: the create info and its chained import outlive the call; the
    // caller keeps the surface alive for the image's life.
    unsafe { device.create_image(&image_info, None) }
}

/// The first memory type in `memory_type_bits` that is device-local and not
/// host-visible — the binding an image over an IOSurface takes. Resolves to
/// type 0 on MoltenVK and Apple Silicon, but is never assumed to.
pub fn device_local_memory_type_that_is_not_host_visible(
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    memory_type_bits: u32,
) -> Option<u32> {
    (0..memory_properties.memory_type_count).find(|&index| {
        let flags = memory_properties.memory_types[index as usize].property_flags;
        memory_type_bits & (1 << index) != 0
            && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            && !flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_properties_with_types(
        property_flags: &[vk::MemoryPropertyFlags],
    ) -> vk::PhysicalDeviceMemoryProperties {
        let mut memory_properties = vk::PhysicalDeviceMemoryProperties {
            memory_type_count: property_flags.len() as u32,
            ..Default::default()
        };
        for (memory_type, flags) in memory_properties
            .memory_types
            .iter_mut()
            .zip(property_flags)
        {
            memory_type.property_flags = *flags;
        }
        memory_properties
    }

    /// MoltenVK on Apple Silicon lists a shared, host-visible type beside the
    /// private one; the binding takes the private type even listed second,
    /// and refuses rather than fall back when only a host-visible one fits.
    #[test]
    fn the_binding_takes_device_local_memory_that_is_not_host_visible() {
        let shared = vk::MemoryPropertyFlags::DEVICE_LOCAL
            | vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT;
        let memory_properties =
            memory_properties_with_types(&[shared, vk::MemoryPropertyFlags::DEVICE_LOCAL]);
        assert_eq!(
            device_local_memory_type_that_is_not_host_visible(&memory_properties, 0b11),
            Some(1)
        );
        assert_eq!(
            device_local_memory_type_that_is_not_host_visible(&memory_properties, 0b01),
            None
        );
    }
}
