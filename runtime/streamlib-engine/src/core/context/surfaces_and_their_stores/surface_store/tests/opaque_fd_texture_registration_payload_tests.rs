// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
use crate::vulkan::rhi::{HostVulkanDevice, HostVulkanTexture};
use streamlib_consumer_rhi::{ConsumerVulkanDevice, ConsumerVulkanTexture};

const VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT: u64 = 0x10;

/// A render target of a format CUDA cannot map — what a graphics kernel
/// draws into on a device without DMA-BUF — registers the usage it was
/// created with, and a consumer device rebuilds the image from that
/// registration alone.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_render_target_registers_its_usage_and_imports_from_the_registration() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Skipping — no Vulkan device: {e}");
            return;
        }
    };
    if device.opaque_fd_image_pool().is_none() {
        println!(
            "Skipping — OPAQUE_FD image pool not created on {}",
            device.name()
        );
        return;
    }
    let consumer_device = match ConsumerVulkanDevice::new_on_the_device_the_runtime_named(
        device.physical_device_uuid(),
    ) {
        Ok(d) => std::sync::Arc::new(d),
        Err(e) => {
            println!("Skipping — ConsumerVulkanDevice unavailable: {e}");
            return;
        }
    };
    let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm).with_usage(
        TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::COPY_DST,
    );
    let texture = HostVulkanTexture::new_opaque_fd_export(&device, &desc)
        .expect("a bgra8 render target allocates as OPAQUE_FD");

    let payload = opaque_fd_texture_registration_payload(
        &texture,
        "surface-under-test",
        "runtime-under-test",
        false,
        false,
        streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
    )
    .expect("an OPAQUE_FD texture states its export contract");

    assert_eq!(payload["handle_type"], SURFACE_HANDLE_TYPE_OPAQUE_FD);
    assert_eq!(payload["format"], TextureFormat::Bgra8Unorm.wire_name());
    let registered_usage = payload["vk_image_usage"]
        .as_u64()
        .expect("the registration states vk_image_usage");
    assert_ne!(
        registered_usage & VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
        0,
        "a render target must register COLOR_ATTACHMENT, got {registered_usage:#x}"
    );

    let fd = texture
        .export_opaque_fd_memory()
        .expect("the render target exports its memory");
    let imported = ConsumerVulkanTexture::from_opaque_fd(
        &consumer_device,
        fd,
        64,
        64,
        TextureFormat::Bgra8Unorm,
        u32::try_from(registered_usage).expect("VkImageUsageFlags is 32 bits"),
        payload["vk_image_allocation_size"]
            .as_u64()
            .expect("the registration states its allocation size"),
        stated_vk_memory_type_index(&payload)
            .expect("the registration states its memory type index"),
    );
    let imported = match imported {
        Ok(imported) => imported,
        Err(failure) => {
            unsafe { libc::close(fd) };
            panic!("the registration must rebuild the image on the consumer: {failure}");
        }
    };
    assert_eq!(
        u64::from(
            streamlib_consumer_rhi::VulkanTextureLike::vk_image_usage_flags(&imported).bits()
        ),
        registered_usage
    );
}

/// The import binds the memory type the registration states and no
/// other: a stated index the rebuilt image cannot bind is refused by
/// name, before any memory is imported, leaving the fd the caller's.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn an_opaque_fd_texture_import_refuses_a_stated_memory_type_the_image_cannot_bind() {
    use crate::vulkan::rhi::VulkanTextureLike as _;
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Skipping — no Vulkan device: {e}");
            return;
        }
    };
    if device.opaque_fd_image_pool().is_none() {
        println!(
            "Skipping — OPAQUE_FD image pool not created on {}",
            device.name()
        );
        return;
    }
    let consumer_device = match ConsumerVulkanDevice::new_on_the_device_the_runtime_named(
        device.physical_device_uuid(),
    ) {
        Ok(d) => std::sync::Arc::new(d),
        Err(e) => {
            println!("Skipping — ConsumerVulkanDevice unavailable: {e}");
            return;
        }
    };
    let desc = TextureDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
        .with_usage(TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING);
    let texture = HostVulkanTexture::new_opaque_fd_export(&device, &desc)
        .expect("an rgba8 render target allocates as OPAQUE_FD");
    let fd = texture
        .export_opaque_fd_memory()
        .expect("the render target exports its memory");

    // VK_MAX_MEMORY_TYPES is 32, so index 32 names no memory type on any
    // device.
    let refusal = ConsumerVulkanTexture::from_opaque_fd(
        &consumer_device,
        fd,
        64,
        64,
        TextureFormat::Rgba8Unorm,
        texture.vk_image_usage_flags().bits(),
        texture.vma_allocation_size(),
        32,
    )
    .err()
    .map(|refusal| refusal.to_string());
    let the_caller_still_owns_the_fd = unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1;
    unsafe { libc::close(fd) };

    let refusal = refusal.expect("a memory type the image cannot bind must be refused");
    assert!(
        refusal.contains("ConsumerVulkanTexture::from_opaque_fd")
            && refusal.contains("memory type index 32"),
        "the refusal names the import and the stated index: {refusal}"
    );
    assert!(
        the_caller_still_owns_the_fd,
        "a refused import leaves the fd open and the caller's"
    );
}
