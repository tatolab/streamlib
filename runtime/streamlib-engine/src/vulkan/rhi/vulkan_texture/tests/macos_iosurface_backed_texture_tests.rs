// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! IOSurface-backed textures on macOS: uploads land at the surface's stride, every single-plane format is taken, and planar formats are refused.

use super::*;

/// The bytes an IOSurface-backed image's surface holds, row by row at
/// the surface's own stride, trimmed to the image's width.
fn packed_rows_of_the_backing_iosurface(texture: &HostVulkanTexture) -> Vec<u8> {
    let iosurface = texture
        .backing_iosurface()
        .expect("an IOSurface-backed image keeps its surface");
    crate::apple::iosurface::with_iosurface_rows_tightly_packed_for_reading(
        iosurface,
        (texture.width() * texture.format().bytes_per_pixel()) as usize,
        texture.height() as usize,
        <[u8]>::to_vec,
    )
    .expect("the surface's rows read out")
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn an_upload_into_an_iosurface_backed_image_lands_in_the_surface_at_its_stride() {
    let Ok(device) = HostVulkanDevice::new() else {
        println!("Skipping - no Vulkan device available");
        return;
    };
    // An odd width, so a surface that pads its rows reads wrong at the
    // image's packed stride.
    let (width, height) = (37, 5);
    let texture = HostVulkanTexture::new_iosurface_backed(
        &device,
        &TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm).with_usage(
            TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::STORAGE_BINDING,
        ),
    )
    .expect("an IOSurface-backed image");
    assert_eq!(texture.vk_image_tiling(), vk::ImageTiling::OPTIMAL);
    assert_ne!(texture.vk_memory_binding().0, vk::DeviceMemory::null());

    let pattern: Vec<u8> = (0..(width * height * 4) as usize)
        .map(|index| (index.wrapping_mul(31).wrapping_add(7)) as u8)
        .collect();
    assert_ne!(
        packed_rows_of_the_backing_iosurface(&texture),
        pattern,
        "the surface already held the pattern before the engine wrote it"
    );

    let staging = crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
        &device,
        pattern.len() as u64,
    )
    .expect("a staging buffer");
    unsafe {
        std::ptr::copy_nonoverlapping(pattern.as_ptr(), staging.mapped_ptr(), pattern.len());
        let _final_texture_layout = device
            .upload_buffer_to_image(staging.buffer(), &texture, width, height)
            .expect("the upload");
    }
    assert_eq!(packed_rows_of_the_backing_iosurface(&texture), pattern);
}

/// MoltenVK checks an IOSurface's element size against the format's block
/// size, so the 8- and 16-byte float formats take a surface of their own
/// element size — and an upload lands in it as it does for 4 bytes.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn every_single_plane_format_takes_an_iosurface_backed_image() {
    let Ok(device) = HostVulkanDevice::new() else {
        println!("Skipping - no Vulkan device available");
        return;
    };
    let (width, height) = (19, 3);
    for format in [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba8UnormSrgb,
        TextureFormat::Bgra8Unorm,
        TextureFormat::Bgra8UnormSrgb,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba32Float,
    ] {
        let texture = HostVulkanTexture::new_iosurface_backed(
            &device,
            &TextureDescriptor::new(width, height, format).with_usage(
                TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST
                    | TextureUsages::TEXTURE_BINDING
                    | TextureUsages::STORAGE_BINDING,
            ),
        )
        .unwrap_or_else(|refusal| panic!("{format:?} over an IOSurface: {refusal}"));
        let pattern: Vec<u8> = (0..(width * height * format.bytes_per_pixel()) as usize)
            .map(|index| (index.wrapping_mul(13).wrapping_add(5)) as u8)
            .collect();
        let staging = crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
            &device,
            pattern.len() as u64,
        )
        .expect("a staging buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(pattern.as_ptr(), staging.mapped_ptr(), pattern.len());
            let _final_texture_layout = device
                .upload_buffer_to_image(staging.buffer(), &texture, width, height)
                .expect("the upload");
        }
        assert_eq!(
            packed_rows_of_the_backing_iosurface(&texture),
            pattern,
            "{format:?}"
        );
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_planar_format_is_refused_an_iosurface_backed_image_by_name() {
    let Ok(device) = HostVulkanDevice::new() else {
        println!("Skipping - no Vulkan device available");
        return;
    };
    let refused = HostVulkanTexture::new_iosurface_backed(
        &device,
        &TextureDescriptor::new(64, 64, TextureFormat::Nv12),
    )
    .err()
    .expect("NV12 has two planes");
    assert!(refused.to_string().contains("planar"), "{refused}");
}
