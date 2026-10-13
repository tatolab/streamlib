// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use crate::core::rhi::{PixelFormat, TextureDescriptor, TextureFormat, TextureUsages};
use crate::vulkan::rhi::{
    HostVulkanBuffer, HostVulkanDevice, HostVulkanTexture, VulkanTextureLike,
};

const FRAME_WIDTH: u32 = 1024;
const FRAME_HEIGHT: u32 = 1024;
const RGBA_BYTES_PER_PIXEL: u32 = 4;

// A device-gated test that finds no device passes trivially, so the skip has
// to reach the person reading the run — and stdout is the only channel a test
// harness surfaces.
#[allow(clippy::disallowed_macros)]
fn host_vulkan_device_on_moltenvk_or_skip() -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(device) => Some(device),
        Err(e) => {
            println!("Skipping - no Vulkan device on MoltenVK: {e}");
            None
        }
    }
}

fn frame_descriptor() -> TextureDescriptor {
    TextureDescriptor::new(FRAME_WIDTH, FRAME_HEIGHT, TextureFormat::Rgba8Unorm).with_usage(
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
    )
}

/// The working set's usage as the budget gate reads it.
fn working_set_usage_the_gate_reads(device: &HostVulkanDevice) -> u64 {
    let reading = device.metal_working_set_of_the_device().reading();
    reading.metal_current_allocated_bytes
        + reading.runtime_minted_iosurface_bytes_metal_does_not_count
}

fn metal_current_allocated_bytes(device: &HostVulkanDevice) -> u64 {
    device
        .metal_working_set_of_the_device()
        .metal_current_allocated_bytes()
}

/// Metal's own growth for a plain, VMA-allocated image of the frame's extent:
/// the reference that tells a doubled IOSurface delta apart from MoltenVK
/// backing a raw `vkAllocateMemory` with a Metal allocation of its own.
#[allow(clippy::disallowed_macros)]
fn print_what_a_plain_image_of_the_frames_extent_adds_to_metals_count(
    device: &Arc<HostVulkanDevice>,
) {
    let metal_before = metal_current_allocated_bytes(device);
    let plain_image = HostVulkanTexture::new_device_local(device, &frame_descriptor())
        .expect("a plain image of the frame's extent allocates");
    let metal_after = metal_current_allocated_bytes(device);
    println!(
        "plain VMA image {FRAME_WIDTH}x{FRAME_HEIGHT}: memory_requirements.size={} \
         Metal currentAllocatedSize delta={}",
        plain_image.vk_memory_size(),
        metal_after.saturating_sub(metal_before)
    );
}

fn assert_counted_once_in_the_working_set(
    gate_usage_delta: u64,
    iosurface_alloc_size: u64,
    what_was_minted: &str,
) {
    assert!(
        gate_usage_delta.saturating_mul(2) >= iosurface_alloc_size
            && gate_usage_delta.saturating_mul(2) <= iosurface_alloc_size.saturating_mul(3),
        "{what_was_minted}: the gate's working-set usage grew by {gate_usage_delta} bytes for a \
         {iosurface_alloc_size}-byte IOSurface, which is not once — flip the matching \
         METAL_COUNTS_… const with what this run measured"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[allow(clippy::disallowed_macros)]
fn an_iosurface_backed_vulkan_image_is_counted_once_in_the_metal_working_set_usage() {
    let Some(device) = host_vulkan_device_on_moltenvk_or_skip() else {
        return;
    };
    print_what_a_plain_image_of_the_frames_extent_adds_to_metals_count(&device);

    let gate_usage_before = working_set_usage_the_gate_reads(&device);
    let metal_before = metal_current_allocated_bytes(&device);
    let texture = HostVulkanTexture::new_iosurface_backed(&device, &frame_descriptor())
        .expect("an IOSurface-backed frame allocates");
    let gate_usage_delta =
        working_set_usage_the_gate_reads(&device).saturating_sub(gate_usage_before);
    let metal_delta = metal_current_allocated_bytes(&device).saturating_sub(metal_before);
    let iosurface_alloc_size = texture
        .backing_iosurface()
        .expect("the frame was minted over an IOSurface")
        .alloc_size() as u64;
    println!(
        "IOSurface-backed image {FRAME_WIDTH}x{FRAME_HEIGHT}: alloc_size={iosurface_alloc_size} \
         memory_requirements.size={} gate usage delta={gate_usage_delta} Metal \
         currentAllocatedSize delta={metal_delta}",
        texture.vk_memory_size()
    );
    assert_counted_once_in_the_working_set(
        gate_usage_delta,
        iosurface_alloc_size,
        "an IOSurface-backed Vulkan image",
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[allow(clippy::disallowed_macros)]
fn an_iosurface_backed_pixel_buffer_is_counted_once_in_the_metal_working_set_usage() {
    let Some(device) = host_vulkan_device_on_moltenvk_or_skip() else {
        return;
    };
    print_what_a_plain_image_of_the_frames_extent_adds_to_metals_count(&device);

    let gate_usage_before = working_set_usage_the_gate_reads(&device);
    let metal_before = metal_current_allocated_bytes(&device);
    let pixel_buffer = HostVulkanBuffer::new_iosurface_backed_pixel_buffer(
        &device,
        FRAME_WIDTH,
        FRAME_HEIGHT,
        RGBA_BYTES_PER_PIXEL,
        PixelFormat::Rgba32,
    )
    .expect("an IOSurface-backed pixel buffer allocates");
    let gate_usage_delta =
        working_set_usage_the_gate_reads(&device).saturating_sub(gate_usage_before);
    let metal_delta = metal_current_allocated_bytes(&device).saturating_sub(metal_before);
    let iosurface_alloc_size = pixel_buffer
        .backing_iosurface()
        .expect("the pixel buffer was minted over an IOSurface")
        .alloc_size() as u64;
    println!(
        "IOSurface-backed pixel buffer {FRAME_WIDTH}x{FRAME_HEIGHT}: \
         alloc_size={iosurface_alloc_size} buffer size={} gate usage delta={gate_usage_delta} \
         Metal currentAllocatedSize delta={metal_delta}",
        pixel_buffer.size()
    );
    assert_counted_once_in_the_working_set(
        gate_usage_delta,
        iosurface_alloc_size,
        "an IOSurface-backed host-pointer pixel buffer",
    );
}
