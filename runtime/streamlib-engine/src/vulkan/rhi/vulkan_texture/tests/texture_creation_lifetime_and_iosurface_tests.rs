// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Creating textures, their lifetime and image views, and IOSurface-backed textures on macOS.

use super::*;

/// The bytes an IOSurface-backed image's surface holds, row by row at
/// the surface's own stride, trimmed to the image's width.
#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_pool_texture_creation_1920x1080_bgra8() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
    let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");

    assert!(texture.image().is_some());
    assert_eq!(texture.width(), 1920);
    assert_eq!(texture.height(), 1080);
    assert_eq!(texture.format(), TextureFormat::Bgra8Unorm);

    println!(
        "Pool texture created: {}x{} {:?}",
        texture.width(),
        texture.height(),
        texture.format(),
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_texture_drop_frees_memory() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
    let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");
    drop(texture);

    println!("Texture drop completed without panic");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_multiple_textures_coexist() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);

    let t0 = HostVulkanTexture::new(&device, &desc).expect("texture 0 failed");
    let t1 = HostVulkanTexture::new(&device, &desc).expect("texture 1 failed");
    let t2 = HostVulkanTexture::new(&device, &desc).expect("texture 2 failed");
    let t3 = HostVulkanTexture::new(&device, &desc).expect("texture 3 failed");

    assert!(t0.image().is_some());
    assert!(t1.image().is_some());
    assert!(t2.image().is_some());
    assert!(t3.image().is_some());

    println!("4 textures coexist");

    drop(t0);
    drop(t1);
    drop(t2);
    drop(t3);

    println!("All dropped successfully");
}

/// Validates the camera-display allocation pattern after the fix:
/// 1. Camera: HOST_VISIBLE pixel buffers via raw exportable allocation (DMA-BUF)
/// 2. Camera: DEVICE_LOCAL compute output image via VMA (no export)
/// 3. Display: DEVICE_LOCAL camera textures via raw dedicated allocation (no export)
///
/// The original bug: VMA's global pTypeExternalMemoryHandleTypes made ALL block
/// allocations DMA-BUF exportable. On NVIDIA, after creating a swapchain, the
/// driver rejected additional DMA-BUF exportable DEVICE_LOCAL block allocations.
///
/// The fix: remove global export config from VMA. Exportable allocations (pixel
/// buffers, textures for IPC) use raw vkAllocateMemory with VkExportMemoryAllocateInfo.
/// Internal allocations (display camera textures) use raw vkAllocateMemory with
/// dedicated allocation + multi-type fallback (no export flags).
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_camera_display_allocation_pattern() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let allocator = device.allocator();
    let vk_device = device.device();
    let width = 1920u32;
    let height = 1080u32;

    // Step 1: Camera pixel buffers via HostVulkanBuffer (raw exportable allocation)
    use crate::vulkan::rhi::HostVulkanBuffer;
    let mut pixel_buffers = Vec::new();
    for i in 0..4 {
        let buf = HostVulkanBuffer::new(&device, (width as u64) * (height as u64) * 4_u64)
            .unwrap_or_else(|e| panic!("pixel buffer [{i}] creation failed: {e}"));
        assert!(!buf.mapped_ptr().is_null());
        pixel_buffers.push(buf);
    }
    println!(
        "Step 1: {} pixel buffers created (raw exportable)",
        pixel_buffers.len()
    );

    // Step 2: Camera compute output image via VMA (no export, no dedicated)
    let compute_img_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .build();

    let compute_alloc_opts = vma::AllocationOptions {
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };

    let (compute_img, compute_alloc) =
        unsafe { allocator.create_image(compute_img_info, &compute_alloc_opts) }
            .expect("compute output image creation failed");
    println!("Step 2: compute output image created (VMA, DEVICE_LOCAL)");

    // Step 3: Display camera textures via raw dedicated allocation (no export)
    // This was the allocation that failed before the fix.
    let mut camera_textures: Vec<(vk::Image, vk::DeviceMemory, vk::ImageView)> = Vec::new();
    for i in 0..4 {
        let img_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .build();

        let image = unsafe { vk_device.create_image(&img_info, None) }
            .unwrap_or_else(|e| panic!("camera image [{i}] creation failed: {e}"));

        let mem_reqs = unsafe { vk_device.get_image_memory_requirements(image) };

        // Try each compatible memory type with dedicated allocation
        let mut memory = None;
        for type_idx in 0..32u32 {
            if (mem_reqs.memory_type_bits & (1 << type_idx)) == 0 {
                continue;
            }
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::builder()
                .image(image)
                .build();
            let alloc_info = vk::MemoryAllocateInfo::builder()
                .allocation_size(mem_reqs.size)
                .memory_type_index(type_idx)
                .push_next(&mut dedicated)
                .build();
            if let Ok(mem) = unsafe { vk_device.allocate_memory(&alloc_info, None) } {
                memory = Some(mem);
                break;
            }
        }

        let memory = memory.unwrap_or_else(|| {
            unsafe { vk_device.destroy_image(image, None) };
            panic!("camera texture [{i}] memory allocation failed — all memory types rejected");
        });

        unsafe { vk_device.bind_image_memory(image, memory, 0) }
            .unwrap_or_else(|e| panic!("camera texture [{i}] bind failed: {e}"));

        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image)
            .view_type(vk::ImageViewType::_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .build();

        let image_view = unsafe { vk_device.create_image_view(&view_info, None) }
            .unwrap_or_else(|e| panic!("camera texture view [{i}] failed: {e}"));

        camera_textures.push((image, memory, image_view));
    }
    println!(
        "Step 3: {} camera textures created (raw dedicated, no export)",
        camera_textures.len()
    );

    // Cleanup
    unsafe {
        for (image, memory, view) in camera_textures {
            vk_device.destroy_image_view(view, None);
            vk_device.free_memory(memory, None);
            vk_device.destroy_image(image, None);
        }
        allocator.destroy_image(compute_img, compute_alloc);
    }
    drop(pixel_buffers);
    println!("All resources cleaned up successfully");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_various_formats() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let formats = [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm];

    for format in formats {
        let desc = TextureDescriptor::new(1920, 1080, format);
        let texture = HostVulkanTexture::new(&device, &desc)
            .unwrap_or_else(|e| panic!("Failed to create texture with {format:?}: {e}"));

        assert!(texture.image().is_some());
        assert_eq!(texture.format(), format);
        println!("Format {:?}: OK", format);
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_device_local_texture_creation() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Rgba8Unorm)
        .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING);
    let texture = HostVulkanTexture::new_device_local(&device, &desc)
        .expect("device-local texture creation failed");

    assert!(texture.image().is_some());
    assert_eq!(texture.width(), 1920);
    assert_eq!(texture.height(), 1080);
    assert_eq!(texture.format(), TextureFormat::Rgba8Unorm);

    println!(
        "Device-local texture created: {}x{}",
        texture.width(),
        texture.height()
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_lazy_image_view() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let desc = TextureDescriptor::new(640, 480, TextureFormat::Rgba8Unorm);
    let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");

    // First call creates the image view
    let view1 = texture.image_view().expect("image_view() failed");
    // Second call returns the cached view
    let view2 = texture.image_view().expect("cached image_view() failed");
    assert_eq!(
        view1, view2,
        "image_view() should return the same cached view"
    );

    println!("Lazy image view: created and cached successfully");
}
