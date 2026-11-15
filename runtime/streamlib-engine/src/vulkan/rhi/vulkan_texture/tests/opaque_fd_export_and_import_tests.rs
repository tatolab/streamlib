// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! OPAQUE_FD export images on Linux: the round trip, cross-flavor refusals, and a consumer reading a held pool back.

use super::*;

/// `new_opaque_fd_export` allocates from the OPAQUE_FD image pool,
/// reports `is_opaque_fd_export() == true`, exports a valid kernel
/// fd, and the consumer-rhi import side accepts the same fd (alloc-
/// size round-trip). Cross-flavor export is rejected (calling
/// `export_opaque_fd_memory` on a DMA-BUF-allocated texture
/// produces an error).
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn opaque_fd_image_export_round_trip_and_cross_flavor_rejection() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_image_pool().is_none() {
        println!("Skipping - OPAQUE_FD image pool unavailable on this driver");
        return;
    }

    // Positive: OPAQUE_FD-allocated image exports an OPAQUE_FD fd.
    let desc = TextureDescriptor::new(128, 128, TextureFormat::Rgba8Unorm);
    let texture = HostVulkanTexture::new_opaque_fd_export(&device, &desc)
        .expect("new_opaque_fd_export failed");
    assert!(
        texture.is_opaque_fd_export(),
        "is_opaque_fd_export() must report true for OPAQUE_FD-allocated images"
    );
    assert_eq!(texture.width(), 128);
    assert_eq!(texture.height(), 128);
    assert_eq!(texture.format(), TextureFormat::Rgba8Unorm);
    let alloc_size = texture.vma_allocation_size();
    assert!(
        alloc_size >= (128 * 128 * 4) as vk::DeviceSize,
        "VMA allocation size {alloc_size} must cover at least 128x128x4 bytes"
    );
    let fd = texture
        .export_opaque_fd_memory()
        .expect("export_opaque_fd_memory failed");
    assert!(fd >= 0, "OPAQUE_FD fd must be non-negative");
    unsafe { libc::close(fd) };

    // Negative: DMA-BUF-allocated texture rejects OPAQUE_FD export.
    let dma_buf_desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm)
        .with_usage(TextureUsages::TEXTURE_BINDING);
    let dma_buf_tex =
        HostVulkanTexture::new(&device, &dma_buf_desc).expect("DMA-BUF texture creation failed");
    assert!(
        !dma_buf_tex.is_opaque_fd_export(),
        "is_opaque_fd_export() must be false for DMA-BUF-allocated images"
    );
    match dma_buf_tex.export_opaque_fd_memory() {
        Err(crate::core::Error::GpuError(msg)) => {
            assert!(
                msg.contains("not created with `new_opaque_fd_export`"),
                "error must call out the cross-flavor mismatch, got: {msg}"
            );
        }
        other => panic!("expected cross-flavor rejection on DMA-BUF texture, got {other:?}"),
    }
}

/// Record `record` into a one-shot command buffer on `queue`, submit it and
/// wait for its fence.
fn submit_one_shot_commands_and_wait(
    device: &vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    record: impl FnOnce(vk::CommandBuffer),
) {
    // SAFETY: every handle is created and destroyed here, on `device`, and
    // the fence wait retires the submission before the pool is destroyed.
    unsafe {
        let command_pool = device
            .create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(queue_family_index)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
            .expect("a command pool");
        let command_buffer = device
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
            .expect("a command buffer")[0];
        device
            .begin_command_buffer(
                command_buffer,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .expect("the command buffer begins");
        record(command_buffer);
        device
            .end_command_buffer(command_buffer)
            .expect("the command buffer ends");
        let fence = device
            .create_fence(&vk::FenceCreateInfo::default(), None)
            .expect("a fence");
        let command_buffers = [command_buffer];
        let submit_info = vk::SubmitInfo::builder().command_buffers(&command_buffers);
        device
            .queue_submit(queue, &[submit_info], fence)
            .expect("the commands submit");
        device
            .wait_for_fences(&[fence], true, u64::MAX)
            .expect("the commands complete");
        device.destroy_fence(fence, None);
        device.destroy_command_pool(command_pool, None);
    }
}

/// A barrier of the whole colour image from `old_layout` to `new_layout`.
fn whole_colour_image_barrier(
    image: vk::Image,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
) -> vk::ImageMemoryBarrier {
    vk::ImageMemoryBarrier::builder()
        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
        .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build()
}

/// A node output pool's worth of 1080p OPAQUE_FD images, each cleared to
/// its own colour and imported onto a consumer device that holds every
/// import at once — as a module's per-slot import cache does — reads back
/// on the consumer as the exporter wrote it, never as zeros.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn every_opaque_fd_image_of_a_held_pool_reads_back_on_a_consumer_as_the_exporter_wrote_it() {
    use std::os::fd::{FromRawFd as _, OwnedFd};

    use crate::vulkan::rhi::HostVulkanBuffer;
    use streamlib_consumer_rhi::{
        ConsumerVulkanBuffer, ConsumerVulkanDevice, ConsumerVulkanTexture,
    };

    const WIDTH: u32 = 1920;
    const HEIGHT: u32 = 1080;
    const POOL_SLOTS: u8 = 32;
    const READBACK_BYTE_SIZE: u64 = WIDTH as u64 * HEIGHT as u64 * 4;

    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_image_pool().is_none() {
        println!("Skipping - OPAQUE_FD image pool unavailable on this driver");
        return;
    }
    let consumer_device = match ConsumerVulkanDevice::new_on_the_device_the_runtime_named(
        device.physical_device_uuid(),
    ) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            println!("Skipping - ConsumerVulkanDevice unavailable: {e}");
            return;
        }
    };

    let readback_exported_by_the_host =
        HostVulkanBuffer::new_opaque_fd_export(&device, READBACK_BYTE_SIZE)
            .expect("a host-visible readback buffer the host exports");
    let readback_fd = readback_exported_by_the_host
        .export_opaque_fd_memory()
        .expect("the readback buffer's fd");
    let readback_on_the_consumer =
        ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
            &consumer_device,
            // SAFETY: a fresh fd the export just returned; nothing else holds it.
            unsafe { OwnedFd::from_raw_fd(readback_fd) },
            READBACK_BYTE_SIZE as vk::DeviceSize,
            readback_exported_by_the_host
                .vma_allocation_memory_type_index()
                .expect("the readback buffer states its memory type index"),
        )
        .expect("the readback buffer imports onto the consumer");

    let descriptor = TextureDescriptor::new(WIDTH, HEIGHT, TextureFormat::Rgba8Unorm).with_usage(
        TextureUsages::TEXTURE_BINDING
            | TextureUsages::STORAGE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::COPY_DST,
    );
    let slot_colour = |slot: u8| [slot.wrapping_mul(7).wrapping_add(40), 200, 255 - slot, 255];
    let mut held_pool = Vec::new();
    for slot in 0..POOL_SLOTS {
        let exported = HostVulkanTexture::new_opaque_fd_export(&device, &descriptor)
            .expect("an OPAQUE_FD export image");
        let exported_image = exported.image().expect("an allocated image");
        let [red, green, blue, alpha] = slot_colour(slot).map(|channel| f32::from(channel) / 255.0);
        submit_one_shot_commands_and_wait(
            device.device(),
            device.queue(),
            device.queue_family_index(),
            |command_buffer| unsafe {
                device.device().cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[] as &[vk::MemoryBarrier],
                    &[] as &[vk::BufferMemoryBarrier],
                    &[whole_colour_image_barrier(
                        exported_image,
                        vk::ImageLayout::UNDEFINED,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    )],
                );
                device.device().cmd_clear_color_image(
                    command_buffer,
                    exported_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue {
                        float32: [red, green, blue, alpha],
                    },
                    &[vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1)
                        .build()],
                );
                device.device().cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[] as &[vk::MemoryBarrier],
                    &[] as &[vk::BufferMemoryBarrier],
                    &[whole_colour_image_barrier(
                        exported_image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    )],
                );
            },
        );
        let memory_fd = exported
            .export_opaque_fd_memory()
            .expect("the image's OPAQUE_FD");
        let imported = ConsumerVulkanTexture::from_opaque_fd(
            &consumer_device,
            memory_fd,
            WIDTH,
            HEIGHT,
            TextureFormat::Rgba8Unorm,
            exported.vk_image_meta.vk_image_usage_flags.bits(),
            exported.vma_allocation_size(),
            exported
                .vma_allocation_memory_type_index()
                .expect("the image states its memory type index"),
        )
        .unwrap_or_else(|failure| panic!("slot {slot} imports onto the consumer: {failure}"));
        held_pool.push((slot, exported, imported));
    }

    let mut slots_read_back_wrong = Vec::new();
    for (slot, _exported, imported) in &held_pool {
        imported
            .acquire_from_foreign_layout(
                streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            )
            .expect("the consumer acquires the image");
        let consumer = consumer_device.device();
        submit_one_shot_commands_and_wait(
            consumer,
            consumer_device.queue(),
            consumer_device.queue_family_index(),
            |command_buffer| unsafe {
                consumer.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[] as &[vk::MemoryBarrier],
                    &[] as &[vk::BufferMemoryBarrier],
                    &[whole_colour_image_barrier(
                        imported.image(),
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    )],
                );
                consumer.cmd_copy_image_to_buffer(
                    command_buffer,
                    imported.image(),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    readback_on_the_consumer.buffer(),
                    &[vk::BufferImageCopy::builder()
                        .image_subresource(
                            vk::ImageSubresourceLayers::builder()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .layer_count(1)
                                .build(),
                        )
                        .image_extent(vk::Extent3D {
                            width: WIDTH,
                            height: HEIGHT,
                            depth: 1,
                        })
                        .build()],
                );
                consumer.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[] as &[vk::MemoryBarrier],
                    &[] as &[vk::BufferMemoryBarrier],
                    &[whole_colour_image_barrier(
                        imported.image(),
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    )],
                );
            },
        );
        // SAFETY: the copy above completed, and the readback buffer is the
        // host's own host-visible coherent export of the same memory.
        let read_back = unsafe {
            std::slice::from_raw_parts(
                readback_exported_by_the_host.mapped_ptr(),
                READBACK_BYTE_SIZE as usize,
            )
        };
        let expected = slot_colour(*slot);
        let (pixels_read_back, _) = read_back.as_chunks::<4>();
        let pixels_read_back_wrong = pixels_read_back
            .iter()
            .filter(|pixel| **pixel != expected)
            .count();
        if pixels_read_back_wrong > 0 {
            slots_read_back_wrong.push(format!(
                "slot {slot}: {pixels_read_back_wrong} of {} pixels differ from {expected:?}, \
                 first pixel {:?}",
                WIDTH * HEIGHT,
                &read_back[..4]
            ));
        }
    }
    assert!(
        slots_read_back_wrong.is_empty(),
        "every held import must read back the exporter's pixels: {slots_read_back_wrong:#?}"
    );
}

/// The device query agrees with the allocation: every request the device
/// reports exportable as OPAQUE_FD allocates, and one it does not — an
/// sRGB storage image, which many devices cannot create at all — is
/// refused naming the device before any image is created.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn opaque_fd_export_follows_what_the_device_reports_for_the_format_and_usage() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_image_pool().is_none() {
        println!("Skipping - OPAQUE_FD image pool unavailable on this driver");
        return;
    }
    let device_name = device.name();
    let render_target_usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;

    assert!(
        !HostVulkanTexture::device_exports_an_opaque_fd_image_of_format_and_usage(
            &device,
            TextureFormat::Nv12,
            TextureUsages::TEXTURE_BINDING,
        ),
        "{device_name}: a multi-plane format never crosses as one OPAQUE_FD block"
    );
    for (format, usage) in [
        (TextureFormat::Rgba8Unorm, TextureUsages::TEXTURE_BINDING),
        (TextureFormat::Rgba8Unorm, render_target_usage),
        (TextureFormat::Bgra8Unorm, render_target_usage),
    ] {
        assert!(
            HostVulkanTexture::device_exports_an_opaque_fd_image_of_format_and_usage(
                &device, format, usage,
            ),
            "{device_name}: reports {format:?} with {usage:?} not exportable as OPAQUE_FD"
        );
    }

    let srgb_storage_usage = TextureUsages::STORAGE_BINDING;
    let device_reports_srgb_storage_exportable =
        HostVulkanTexture::device_exports_an_opaque_fd_image_of_format_and_usage(
            &device,
            TextureFormat::Rgba8UnormSrgb,
            srgb_storage_usage,
        );
    let allocated = HostVulkanTexture::new_opaque_fd_export(
        &device,
        &TextureDescriptor::new(64, 64, TextureFormat::Rgba8UnormSrgb)
            .with_usage(srgb_storage_usage),
    );
    match (device_reports_srgb_storage_exportable, allocated) {
        (true, Ok(_)) => {}
        (false, Err(crate::core::Error::NotSupported(refusal))) => {
            assert!(
                refusal.contains(&device_name),
                "the refusal names the device: {refusal}"
            );
            assert!(
                refusal.contains(TextureFormat::Rgba8UnormSrgb.wire_name())
                    && refusal.contains("storage_binding")
                    && refusal.contains("OPAQUE_FD"),
                "the refusal names the format and the flavour: {refusal}"
            );
        }
        (reported, allocated) => panic!(
            "{device_name}: reported sRGB storage exportable = {reported}, but the \
             allocation answered {:?}",
            allocated.map(|_| "Ok")
        ),
    }
}

/// A multi-plane format is refused by name; every single-plane format
/// allocates. A CUDA-mappable format keeps exactly the usage CUDA's
/// texture- and surface-object imports bind; a render target, of any
/// format, carries `COLOR_ATTACHMENT` and still exports.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn opaque_fd_image_takes_every_single_plane_format_and_the_requested_usage() {
    use crate::vulkan::rhi::VulkanTextureLike as _;
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_image_pool().is_none() {
        println!("Skipping - OPAQUE_FD image pool unavailable on this driver");
        return;
    }

    match HostVulkanTexture::new_opaque_fd_export(
        &device,
        &TextureDescriptor::new(64, 64, TextureFormat::Nv12),
    ) {
        Err(crate::core::Error::Configuration(msg)) => {
            assert!(msg.contains("Nv12"), "refusal must name the format: {msg}");
            assert!(msg.contains("multi-plane"), "refusal must say why: {msg}");
        }
        Err(e) => panic!("Nv12 must be refused with a Configuration error; got {e}"),
        Ok(_) => panic!("Nv12 must be refused for OPAQUE_FD; got Ok"),
    }

    let cuda_usage_set = vk::ImageUsageFlags::TRANSFER_SRC
        | vk::ImageUsageFlags::TRANSFER_DST
        | vk::ImageUsageFlags::SAMPLED
        | vk::ImageUsageFlags::STORAGE;
    for cuda_mappable_format in [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba32Float,
    ] {
        let texture = HostVulkanTexture::new_opaque_fd_export(
            &device,
            &TextureDescriptor::new(64, 64, cuda_mappable_format),
        )
        .unwrap_or_else(|e| panic!("{cuda_mappable_format:?} must allocate, got: {e:?}"));
        assert!(texture.is_opaque_fd_export());
        assert_eq!(texture.vk_image_usage_flags(), cuda_usage_set);
    }

    for render_target_format in [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm] {
        let desc = TextureDescriptor::new(64, 64, render_target_format)
            .with_usage(TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING);
        let texture = HostVulkanTexture::new_opaque_fd_export(&device, &desc).unwrap_or_else(|e| {
            panic!("{render_target_format:?} render target must allocate, got: {e:?}")
        });
        assert!(texture.is_opaque_fd_export());
        assert!(
            texture
                .vk_image_usage_flags()
                .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT),
            "{render_target_format:?} render target must carry COLOR_ATTACHMENT"
        );
        let fd = texture
            .export_opaque_fd_memory()
            .expect("a render-target OPAQUE_FD image exports its memory");
        assert!(fd >= 0);
        unsafe { libc::close(fd) };
    }

    for sampled_only_format in [
        TextureFormat::Bgra8Unorm,
        TextureFormat::Bgra8UnormSrgb,
        TextureFormat::Rgba8UnormSrgb,
    ] {
        let texture = HostVulkanTexture::new_opaque_fd_export(
            &device,
            &TextureDescriptor::new(64, 64, sampled_only_format),
        )
        .unwrap_or_else(|e| panic!("{sampled_only_format:?} must allocate, got: {e:?}"));
        assert!(
            !texture
                .vk_image_usage_flags()
                .contains(vk::ImageUsageFlags::STORAGE),
            "{sampled_only_format:?} carries STORAGE only when asked for it"
        );
    }
}

/// Zero width/height is rejected — these aren't CUDA-specific
/// constraints but mirror the buffer-side input validation.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn opaque_fd_image_rejects_zero_dimensions() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_image_pool().is_none() {
        println!("Skipping - OPAQUE_FD image pool unavailable on this driver");
        return;
    }

    for (w, h) in [(0, 64), (64, 0), (0, 0)] {
        let desc = TextureDescriptor::new(w, h, TextureFormat::Rgba8Unorm);
        match HostVulkanTexture::new_opaque_fd_export(&device, &desc) {
            Err(crate::core::Error::Configuration(_)) => {}
            Err(e) => panic!(
                "dimensions {w}x{h} must be rejected with Configuration error; \
                 got {e}"
            ),
            Ok(_) => panic!("dimensions {w}x{h} must be rejected for OPAQUE_FD; got Ok"),
        }
    }
}
