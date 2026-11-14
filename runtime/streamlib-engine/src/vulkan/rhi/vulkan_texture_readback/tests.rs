// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::TextureDescriptor;

fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(d) => Some(d),
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            None
        }
    }
}

/// Allocate a host-side BGRA8 texture with TRANSFER_SRC + STORAGE
/// usage and fill plane 0 with a deterministic pattern via a
/// pre-recorded copy from a HOST_VISIBLE staging buffer.
fn make_filled_texture(
    device: &Arc<HostVulkanDevice>,
    width: u32,
    height: u32,
    format: TextureFormat,
    pattern: impl Fn(u32, u32) -> [u8; 4],
) -> Texture {
    use crate::core::rhi::PixelFormat;
    use crate::vulkan::rhi::HostVulkanBuffer;

    let bpp = format.bytes_per_pixel();
    // Allocate staging via the export-capable pool — fine for fill:
    // we only need HOST_VISIBLE + TRANSFER_SRC.
    let _pix_format = match format {
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => PixelFormat::Bgra32,
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => PixelFormat::Bgra32,
        other => panic!("test fixture only supports 8-bit RGBA/BGRA, got {other:?}"),
    };
    let staging = HostVulkanBuffer::new(device, (width as u64) * (height as u64) * (bpp as u64))
        .expect("staging");
    unsafe {
        let mut p = staging.mapped_ptr();
        for y in 0..height {
            for x in 0..width {
                let px = pattern(x, y);
                std::ptr::copy_nonoverlapping(px.as_ptr(), p, 4);
                p = p.add(4);
            }
        }
    }

    // Allocate the destination texture via the device's RHI helper —
    // a `STORAGE_BINDING | COPY_SRC | COPY_DST` 2D image. We can't
    // use `acquire_render_target_dma_buf_image` here because tests
    // don't have a `GpuContext`; we go straight to `HostVulkanTexture`.
    let desc = TextureDescriptor {
        width,
        height,
        format,
        usage: crate::core::rhi::TextureUsages::COPY_SRC
            | crate::core::rhi::TextureUsages::COPY_DST
            | crate::core::rhi::TextureUsages::STORAGE_BINDING,
        label: Some("readback-test-texture"),
    };
    let host_tex = crate::vulkan::rhi::HostVulkanTexture::new(device, &desc).expect("texture");
    let texture = <Texture as crate::host_rhi::HostTextureExt>::from_vulkan(host_tex);

    // Record + submit a one-shot copy from staging → texture.
    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();
    let pool = unsafe {
        dev.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(qf)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                .build(),
            None,
        )
    }
    .expect("pool");
    let cmd = unsafe {
        dev.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1)
                .build(),
        )
    }
    .expect("cmd")[0];
    unsafe {
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
        .expect("begin");

        // UNDEFINED → TRANSFER_DST_OPTIMAL.
        let to_dst = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(qf)
            .dst_queue_family_index(qf)
            .image(texture.vulkan_inner().image().expect("vk image"))
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1)
                    .build(),
            )
            .build();
        let bs = [to_dst];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&bs)
            .build();
        dev.cmd_pipeline_barrier2(cmd, &dep);

        let copy = vk::BufferImageCopy::builder()
            .buffer_offset(0)
            .buffer_row_length(0)
            .buffer_image_height(0)
            .image_subresource(
                vk::ImageSubresourceLayers::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1)
                    .build(),
            )
            .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .image_extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .build();
        let regions = [copy];
        dev.cmd_copy_buffer_to_image(
            cmd,
            staging.buffer(),
            texture.vulkan_inner().image().expect("vk image"),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &regions,
        );

        // TRANSFER_DST_OPTIMAL → GENERAL.
        let to_general = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(qf)
            .dst_queue_family_index(qf)
            .image(texture.vulkan_inner().image().expect("vk image"))
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1)
                    .build(),
            )
            .build();
        let bs2 = [to_general];
        let dep2 = vk::DependencyInfo::builder()
            .image_memory_barriers(&bs2)
            .build();
        dev.cmd_pipeline_barrier2(cmd, &dep2);

        dev.end_command_buffer(cmd).expect("end");
        let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build()];
        let submits = [vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build()];
        device
            .submit_to_queue(queue, &submits, vk::Fence::null())
            .expect("submit fill");
        dev.queue_wait_idle(queue).expect("wait idle");
        dev.destroy_command_pool(pool, None);
    }

    texture
}

/// Positive: round-trip a known pattern through the readback
/// primitive on a 32x32 texture.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn submit_then_wait_returns_expected_bytes() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let width = 32u32;
    let height = 32u32;
    let pattern = |x: u32, y: u32| {
        [
            ((x.wrapping_mul(7)) & 0xFF) as u8,
            ((y.wrapping_mul(11)) & 0xFF) as u8,
            (((x ^ y).wrapping_mul(13)) & 0xFF) as u8,
            0xFF,
        ]
    };
    let texture = make_filled_texture(&device, width, height, TextureFormat::Bgra8Unorm, pattern);

    let readback = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-positive",
            format: TextureFormat::Bgra8Unorm,
            width,
            height,
        },
    )
    .expect("readback");

    let ticket = readback
        .submit(&texture, TextureSourceLayout::General)
        .expect("submit");
    let bytes = readback.wait_and_read(ticket, u64::MAX).expect("wait");
    for y in 0..height {
        for x in 0..width {
            let off = ((y * width + x) * 4) as usize;
            assert_eq!(
                &bytes[off..off + 4],
                &pattern(x, y),
                "mismatch at ({x},{y})"
            );
        }
    }
}

/// Positive: multiple submits sequentially on a single handle. After
/// each wait, the next submit must succeed and the bytes reflect
/// the (re-filled) texture.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn multiple_sequential_submits_work() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let width = 16u32;
    let height = 16u32;
    let readback = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-sequential",
            format: TextureFormat::Bgra8Unorm,
            width,
            height,
        },
    )
    .expect("readback");

    for run in 0..3u32 {
        let pattern = move |x: u32, y: u32| {
            [
                ((x + run * 5) & 0xFF) as u8,
                ((y + run * 7) & 0xFF) as u8,
                ((run * 11) & 0xFF) as u8,
                0xFF,
            ]
        };
        let texture =
            make_filled_texture(&device, width, height, TextureFormat::Bgra8Unorm, pattern);
        let ticket = readback
            .submit(&texture, TextureSourceLayout::General)
            .expect("submit");
        let bytes = readback.wait_and_read(ticket, u64::MAX).expect("wait");
        for y in 0..height {
            for x in 0..width {
                let off = ((y * width + x) * 4) as usize;
                assert_eq!(
                    &bytes[off..off + 4],
                    &pattern(x, y),
                    "run {run}: mismatch at ({x},{y})"
                );
            }
        }
    }
}

/// Negative: descriptor / texture mismatch must error at submit time.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn rejects_descriptor_mismatch() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let texture = make_filled_texture(&device, 32, 32, TextureFormat::Bgra8Unorm, |_, _| {
        [0, 0, 0, 0xFF]
    });

    // Wrong width.
    let readback = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-wrong-width",
            format: TextureFormat::Bgra8Unorm,
            width: 64,
            height: 32,
        },
    )
    .expect("readback");
    let err = readback
        .submit(&texture, TextureSourceLayout::General)
        .expect_err("expected mismatch");
    assert!(matches!(
        err,
        TextureReadbackError::DescriptorMismatch { .. }
    ));

    // Wrong format.
    let readback = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-wrong-format",
            format: TextureFormat::Rgba16Float,
            width: 32,
            height: 32,
        },
    )
    .expect("readback");
    let err = readback
        .submit(&texture, TextureSourceLayout::General)
        .expect_err("expected mismatch");
    assert!(matches!(
        err,
        TextureReadbackError::DescriptorMismatch { .. }
    ));
}

/// Negative: a second submit before the first ticket is waited
/// returns InFlight.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn second_submit_before_wait_errors_in_flight() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let texture = make_filled_texture(&device, 32, 32, TextureFormat::Bgra8Unorm, |_, _| {
        [1, 2, 3, 4]
    });
    let readback = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-in-flight",
            format: TextureFormat::Bgra8Unorm,
            width: 32,
            height: 32,
        },
    )
    .expect("readback");
    let ticket = readback
        .submit(&texture, TextureSourceLayout::General)
        .expect("first");
    let err = readback
        .submit(&texture, TextureSourceLayout::General)
        .expect_err("expected in-flight");
    assert!(matches!(err, TextureReadbackError::InFlight { .. }));
    // Drain so Drop doesn't have to wait on a half-recorded submit.
    let _ = readback.wait_and_read(ticket, u64::MAX).expect("drain");
}

/// Negative: a ticket from one handle is rejected by another.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn foreign_ticket_rejected() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let texture = make_filled_texture(&device, 16, 16, TextureFormat::Bgra8Unorm, |_, _| {
        [0, 0, 0, 0xFF]
    });
    let rb1 = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-foreign-1",
            format: TextureFormat::Bgra8Unorm,
            width: 16,
            height: 16,
        },
    )
    .expect("rb1");
    let rb2 = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-foreign-2",
            format: TextureFormat::Bgra8Unorm,
            width: 16,
            height: 16,
        },
    )
    .expect("rb2");
    let ticket = rb1
        .submit(&texture, TextureSourceLayout::General)
        .expect("submit");
    // rb2 doesn't own this ticket — should reject.
    let err = rb2.try_read(ticket).expect_err("expected foreign");
    assert!(matches!(err, TextureReadbackError::ForeignTicket { .. }));
    let _ = rb1.wait_and_read(ticket, u64::MAX).expect("drain");
}

/// Negative: try_read with no in-flight submission errors NoSubmission.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn try_read_with_no_submission_errors_no_submission() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let readback = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-no-sub",
            format: TextureFormat::Bgra8Unorm,
            width: 8,
            height: 8,
        },
    )
    .expect("readback");
    let ticket = ReadbackTicket {
        handle_id: readback.handle_id,
        counter: 1,
    };
    let err = readback
        .try_read(ticket)
        .expect_err("expected no submission");
    assert!(matches!(err, TextureReadbackError::NoSubmission { .. }));
}

/// Multiple readback handles can be in flight concurrently.
/// Validates the design assertion that parallel readbacks are
/// achieved by holding N handles, not by parallelism on one.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn multiple_handles_can_be_in_flight_concurrently() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let texture = make_filled_texture(&device, 16, 16, TextureFormat::Bgra8Unorm, |x, y| {
        [(x as u8), (y as u8), (x as u8) ^ (y as u8), 0xFF]
    });
    let rb1 = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-parallel-1",
            format: TextureFormat::Bgra8Unorm,
            width: 16,
            height: 16,
        },
    )
    .expect("rb1");
    let rb2 = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-parallel-2",
            format: TextureFormat::Bgra8Unorm,
            width: 16,
            height: 16,
        },
    )
    .expect("rb2");
    let t1 = rb1
        .submit(&texture, TextureSourceLayout::General)
        .expect("submit 1");
    let t2 = rb2
        .submit(&texture, TextureSourceLayout::General)
        .expect("submit 2");
    // Both tickets must wait + read independently.
    let _ = rb1.wait_and_read(t1, u64::MAX).expect("read 1");
    let _ = rb2.wait_and_read(t2, u64::MAX).expect("read 2");
}

/// Drop on an idle handle must not panic.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn drop_on_idle_handle_no_panic() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let readback = VulkanTextureReadback::new(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-drop-idle",
            format: TextureFormat::Bgra8Unorm,
            width: 8,
            height: 8,
        },
    )
    .expect("readback");
    drop(readback);
}

/// Bytes-per-pixel for Bgra8Unorm and Rgba8Unorm: descriptor sizes
/// match the staging buffer and constructed handles report the
/// expected dimensions.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn descriptor_sizes_and_metadata_round_trip() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let descriptor = TextureReadbackDescriptor {
        label: "rt-meta",
        format: TextureFormat::Bgra8Unorm,
        width: 100,
        height: 50,
    };
    assert_eq!(descriptor.staging_size(), 100 * 50 * 4);
    let readback = VulkanTextureReadback::new(&device, &descriptor).expect("readback");
    assert_eq!(readback.format(), TextureFormat::Bgra8Unorm);
    assert_eq!(readback.width(), 100);
    assert_eq!(readback.height(), 50);
    assert_eq!(readback.staging_size(), 100 * 50 * 4);
    assert!(readback.handle_id() > 0);
}
