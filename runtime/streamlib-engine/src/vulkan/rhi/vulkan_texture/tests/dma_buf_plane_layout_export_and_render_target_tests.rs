// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! DMA-BUF textures on Linux: plane layout aspects, exporting and dropping fds, render targets and rings.

use super::*;

fn inode_of(fd: std::os::unix::io::RawFd) -> Option<u64> {
    let mut file_status = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, file_status.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(unsafe { file_status.assume_init() }.st_ino)
}

/// The device-side half of the lookup's fd contract. A 4 KB buffer's
/// DMA-BUF cannot back a 16 MB image allocation, so the driver refuses
/// at `vkAllocateMemory` — and closes the fd as it does (NVIDIA), which
/// is why the importer must not close it again: in a debug build a
/// second close of an `OwnedFd` is an IO-safety abort of this very
/// test binary. Nothing may be left open, and nothing closed twice.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_texture_import_the_driver_refuses_leaves_no_fd_behind_and_closes_none_twice() {
    use std::os::fd::{FromRawFd as _, OwnedFd};

    let Some(device) = a_device_that_exports_dma_buf_or_skip(
        "a_texture_import_the_driver_refuses_leaves_no_fd_behind_and_closes_none_twice",
    ) else {
        return;
    };

    let source =
        crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(&device, 4096)
            .expect("source buffer allocation failed");
    let fd = source.export_dma_buf_fd().expect("DMA-BUF export failed");
    let inode = inode_of(fd).expect("an exported DMA-BUF must stat");

    let live_imports_before = device.live_import_allocation_count();
    let result = HostVulkanTexture::from_dma_buf_fd(
        &device,
        unsafe { OwnedFd::from_raw_fd(fd) },
        64,
        64,
        TextureFormat::Rgba8Unorm,
        16 * 1024 * 1024,
    );
    assert!(
        result.is_err(),
        "a 4 KB DMA-BUF cannot back a 16 MB image; the driver accepted it and the \
         close-on-failure contract went unexercised"
    );
    drop(result);
    assert_eq!(
        device.live_import_allocation_count(),
        live_imports_before,
        "a refused import must not leave VkDeviceMemory live"
    );
    assert_ne!(
        inode_of(fd),
        Some(inode),
        "the refused import left the DMA-BUF fd open — no owner remains to close it"
    );
}

/// `DRM_FORMAT_MOD_LINEAR` is zero, so a modifier-tiled image is not
/// recognisable by its modifier's value. Keying the aspect choice on
/// that value routes a linear-modifier image down the COLOR branch and
/// trips `VUID-vkGetImageSubresourceLayout-tiling-09433`.
#[test]
fn a_linear_modifier_image_is_queried_through_the_memory_plane_aspect() {
    assert_eq!(
        dma_buf_plane_layout_query_aspect_mask(
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
            TextureFormat::Bgra8Unorm,
            0,
        )
        .expect("plane 0 of a single-plane modifier-tiled image"),
        vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
    );
}

#[test]
fn a_modifier_tiled_nv12_image_uses_the_memory_plane_aspect_not_the_format_plane_aspect() {
    assert_eq!(
        dma_buf_plane_layout_query_aspect_mask(
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
            TextureFormat::Nv12,
            1,
        )
        .expect("plane 1 of a modifier-tiled NV12 image"),
        vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
    );
}

/// The counterpart the modifier's value cannot express: an image
/// created without `VK_EXT_image_drm_format_modifier` also reports a
/// zero modifier, and must keep the COLOR aspect.
#[test]
fn a_linear_tiled_image_created_without_a_modifier_keeps_the_color_aspect() {
    assert_eq!(
        dma_buf_plane_layout_query_aspect_mask(
            vk::ImageTiling::LINEAR,
            TextureFormat::Bgra8Unorm,
            0,
        )
        .expect("plane 0 of a linear-tiled single-plane image"),
        vk::ImageAspectFlags::COLOR,
    );
}

#[test]
fn a_linear_tiled_nv12_image_keeps_the_format_plane_aspects() {
    let aspects: Vec<vk::ImageAspectFlags> = (0..2)
        .map(|plane_index| {
            dma_buf_plane_layout_query_aspect_mask(
                vk::ImageTiling::LINEAR,
                TextureFormat::Nv12,
                plane_index,
            )
            .expect("both planes of a linear-tiled NV12 image")
        })
        .collect();
    assert_eq!(
        aspects,
        vec![vk::ImageAspectFlags::PLANE_0, vk::ImageAspectFlags::PLANE_1],
    );
}

/// OPTIMAL layouts are driver-opaque. The caller refuses them before it
/// reaches this helper, but the helper is independently reachable, so it
/// must not answer COLOR for a tiling it cannot describe.
#[test]
fn an_optimal_tiled_image_has_no_queryable_plane_aspect() {
    assert!(
        dma_buf_plane_layout_query_aspect_mask(
            vk::ImageTiling::OPTIMAL,
            TextureFormat::Bgra8Unorm,
            0,
        )
        .is_err(),
    );
}

#[test]
fn a_plane_index_past_what_the_image_exposes_is_refused() {
    for (tiling, format, plane_index) in [
        (
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
            TextureFormat::Bgra8Unorm,
            4,
        ),
        (vk::ImageTiling::LINEAR, TextureFormat::Nv12, 2),
        (vk::ImageTiling::LINEAR, TextureFormat::Bgra8Unorm, 1),
    ] {
        assert!(
            dma_buf_plane_layout_query_aspect_mask(tiling, format, plane_index).is_err(),
            "plane {plane_index} of a {tiling:?} {format:?} image must be refused, \
             not silently answered with another plane's aspect"
        );
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_dma_buf_export() {
    let Some(device) = a_device_that_exports_dma_buf_or_skip("test_dma_buf_export") else {
        return;
    };

    let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm);
    let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");

    let fd = texture.export_dma_buf_fd().expect("DMA-BUF export failed");
    assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");

    println!("DMA-BUF exported: fd={fd}");
    unsafe { libc::close(fd) };
}

/// A DMA-BUF-exportable texture on the rig's device, or `None` when
/// there is no Vulkan device to build one on.
fn dma_buf_exportable_texture_or_skip() -> Option<HostVulkanTexture> {
    let device = a_device_that_exports_dma_buf_or_skip("dma_buf_exportable_texture_or_skip")?;
    let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm);
    Some(HostVulkanTexture::new(&device, &desc).expect("texture creation failed"))
}

/// Every DMA-BUF export mints its own fd and surrenders it — the rule
/// the OPAQUE_FD exporters already follow. A memoized fd hands two
/// owners the same descriptor: the caller that closes after its
/// hand-off, and the texture that closes again at drop.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn every_dma_buf_export_mints_a_fresh_fd_the_caller_owns() {
    let Some(texture) = dma_buf_exportable_texture_or_skip() else {
        return;
    };

    let first_export = texture
        .export_dma_buf_fd()
        .expect("first DMA-BUF export failed");
    let second_export = texture
        .export_dma_buf_fd()
        .expect("second DMA-BUF export failed");
    assert_ne!(
        first_export, second_export,
        "each export must mint its own fd; the same number twice is one \
         descriptor with two owners"
    );

    drop(texture);

    for exported_fd in [first_export, second_export] {
        let survived = unsafe { libc::fcntl(exported_fd, libc::F_GETFD) } != -1;
        if survived {
            unsafe { libc::close(exported_fd) };
        }
        assert!(
            survived,
            "fd {exported_fd} was closed by the texture that exported it; an \
             exporter surrenders every fd it mints"
        );
    }
}

/// The fd teardown corruption, reproduced without the wire: the
/// registration path closes the fd it was handed, an unrelated owner
/// lands on that descriptor number, and the texture's own drop closes
/// it a second time — out from under whoever holds it now.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dropping_a_texture_never_closes_an_fd_its_export_surrendered() {
    let Some(texture) = dma_buf_exportable_texture_or_skip() else {
        return;
    };

    let exported_fd = texture.export_dma_buf_fd().expect("DMA-BUF export failed");

    // Opened while `exported_fd` is still held, so the kernel's
    // lowest-free rule cannot hand back the same number.
    let unrelated_owner_fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
    assert!(
        unrelated_owner_fd >= 0,
        "could not open the unrelated owner"
    );
    assert_ne!(
        unrelated_owner_fd, exported_fd,
        "the unrelated owner must be a descriptor of its own"
    );

    // One `dup2` is both halves of the hand-off: it closes `exported_fd`
    // exactly as the receiver of an SCM_RIGHTS send does, and seats the
    // unrelated owner on that number atomically — no window in which
    // another thread could claim it.
    assert_eq!(
        unsafe { libc::dup2(unrelated_owner_fd, exported_fd) },
        exported_fd,
        "could not seat an unrelated owner on the surrendered number"
    );

    drop(texture);

    let survived = unsafe { libc::fcntl(exported_fd, libc::F_GETFD) } != -1;
    unsafe {
        if survived {
            libc::close(exported_fd);
        }
        libc::close(unrelated_owner_fd);
    }
    assert!(
        survived,
        "the dropped texture closed fd {exported_fd}, which by then belonged to \
         an unrelated owner — the double-close that corrupts bystander \
         subsystems at teardown"
    );
}

/// Round-trip test for the render-target DMA-BUF path:
/// 1. Pull RT-capable modifiers for ARGB8888 from the device's EGL probe.
/// 2. Skip if none — vivid CI / headless boxes have no modifiers.
/// 3. Allocate a 1920x1080 BGRA render-target VkImage with the candidate list.
/// 4. Assert the driver-chosen modifier is in the candidate list.
/// 5. Export the DMA-BUF fd and read back the per-plane layout.
/// 6. Assert plane[0].row_pitch >= 1920 * 4 (BGRA stride is at least
///    pixel-tight, possibly aligned up by tiling).
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_render_target_dma_buf_round_trip() {
    use crate::vulkan::rhi::drm_modifier_probe::fourcc;

    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Skipping — no Vulkan device: {e}");
            return;
        }
    };
    let table = device.drm_modifier_table();
    let modifiers = table.rt_modifiers(fourcc::DRM_FORMAT_ARGB8888);
    if skip_unless_this_machine_has(
        RigOnlyCapability::DmaBufModifiers,
        !modifiers.is_empty() && device.dma_buf_image_pool_tiled().is_some(),
    ) {
        return;
    }

    let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Bgra8Unorm).with_usage(
        TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC,
    );
    let texture = HostVulkanTexture::new_render_target_dma_buf(&device, &desc, modifiers)
        .expect("RT DMA-BUF allocation must succeed when modifiers exist");

    assert!(texture.image().is_some());
    let chosen = texture.chosen_drm_format_modifier();
    assert!(
        modifiers.contains(&chosen),
        "driver picked modifier 0x{:016x} not in candidate list {:?}",
        chosen,
        modifiers
    );
    // Modifier 0 is DRM_FORMAT_MOD_LINEAR; the EGL probe's RT-capable
    // list is supposed to be tiled-only.
    assert_ne!(
        chosen, 0,
        "RT-capable modifier must not be DRM_FORMAT_MOD_LINEAR"
    );

    let layout = texture
        .dma_buf_plane_layout()
        .expect("plane layout must be queryable");
    assert_eq!(layout.len(), 1, "BGRA is single-plane");
    let (offset, row_pitch) = layout[0];
    assert!(
        row_pitch >= 1920 * 4,
        "row_pitch {row_pitch} must be at least pixel-tight 1920*4"
    );
    println!(
        "RT DMA-BUF: chosen modifier=0x{:016x}, plane[0]: offset={}, row_pitch={}",
        chosen, offset, row_pitch
    );

    let fd = texture
        .export_dma_buf_fd()
        .expect("DMA-BUF export must succeed");
    assert!(fd >= 0, "DMA-BUF fd must be non-negative");
}

/// The rig half of the aspect-mask contract: allocating against
/// `DRM_FORMAT_MOD_LINEAR` yields a modifier-tiled image whose modifier
/// reads zero, and querying its plane layout must raise no validation
/// finding. Keying the aspect on the modifier's value instead of the
/// tiling trips `VUID-vkGetImageSubresourceLayout-tiling-09433` here.
/// NVIDIA advertises LINEAR as its only sampler-only ARGB8888
/// modifier, which is the shape real camera DMA-BUFs take on that
/// driver.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_linear_modifier_plane_layout_query_raises_no_validation_finding() {
    const DRM_FORMAT_MOD_LINEAR: u64 = 0;

    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Skipping — no Vulkan device: {e}");
            return;
        }
    };
    let counts_before = device.validation_layer_message_counts();
    if counts_before.is_none() {
        println!(
            "Skipping — no validation messenger installed. Re-run with \
             STREAMLIB_VULKAN_VALIDATION=1 and VK_LAYER_KHRONOS_validation present."
        );
        return;
    }
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
            // A driver may advertise LINEAR for EGL import yet refuse it
            // under `VkImageDrmFormatModifierListCreateInfoEXT` for this
            // usage set. The aspect-mask contract is what is under test,
            // not the allocator's modifier acceptance.
            skip_because_this_machine_lacks_naming_the_refusal(
                RigOnlyCapability::DmaBufModifiers,
                format!("the driver refused DRM_FORMAT_MOD_LINEAR: {e}"),
            );
            return;
        }
    };

    assert_eq!(
        texture.chosen_drm_format_modifier(),
        DRM_FORMAT_MOD_LINEAR,
        "the candidate list held only LINEAR, so the driver must have picked it"
    );
    assert_eq!(
        texture.vk_image_tiling(),
        vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
        "a LINEAR-modifier image is still modifier-tiled by creation — the \
         recorded tiling is what says so, because the modifier reads zero"
    );

    let layout = texture
        .dma_buf_plane_layout()
        .expect("plane layout of a linear-modifier image must be queryable");
    assert_eq!(layout.len(), 1, "BGRA is single-plane");

    assert_eq!(
        device.validation_layer_message_counts(),
        counts_before,
        "querying a linear-modifier image's plane layout must go through the \
         MEMORY_PLANE aspects, not COLOR"
    );
}

/// `new_render_target_dma_buf` with an empty modifier list must fail
/// loudly rather than silently fall back to LINEAR (which is sampler-
/// only on NVIDIA).
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_render_target_dma_buf_empty_modifiers_rejected() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Skipping — no Vulkan device: {e}");
            return;
        }
    };
    let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm)
        .with_usage(TextureUsages::RENDER_ATTACHMENT);
    let result = HostVulkanTexture::new_render_target_dma_buf(&device, &desc, &[]);
    let err = match result {
        Ok(_) => panic!("empty modifier list must reject, but allocation succeeded"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("empty modifier list") || msg.contains("EGL"),
        "error must explain the missing-EGL-modifier root cause: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_ring_texture_lifecycle() {
    let Some(device) = a_device_that_exports_dma_buf_or_skip("test_ring_texture_lifecycle") else {
        return;
    };

    let desc = TextureDescriptor::new(1920, 1080, TextureFormat::Rgba8Unorm)
        .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING);

    // Create 2 ring textures (matches RING_TEXTURE_COUNT)
    let t0 = HostVulkanTexture::new(&device, &desc).expect("ring texture 0 failed");
    let t1 = HostVulkanTexture::new(&device, &desc).expect("ring texture 1 failed");

    // Both should have valid images and image views
    assert!(t0.image().is_some());
    assert!(t1.image().is_some());
    let v0 = t0.image_view().expect("ring texture 0 image_view failed");
    let v1 = t1.image_view().expect("ring texture 1 image_view failed");
    assert_ne!(v0, v1, "ring textures should have different image views");

    // Both should be DMA-BUF exportable (created via new(), not new_device_local())
    let fd0 = t0
        .export_dma_buf_fd()
        .expect("ring texture 0 DMA-BUF export failed");
    let fd1 = t1
        .export_dma_buf_fd()
        .expect("ring texture 1 DMA-BUF export failed");
    assert!(fd0 >= 0);
    assert!(fd1 >= 0);
    assert_ne!(fd0, fd1);

    println!("Ring texture lifecycle: 2 textures created, image views cached, DMA-BUF exported");

    drop(t0);
    drop(t1);
    println!("Ring textures dropped cleanly");
}
