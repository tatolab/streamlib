// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::PixelBuffer;
use crate::core::rhi::spirv_module_rewriting_for_tests::move_binding_to_another_descriptor_set_in_spirv_module;
use crate::vulkan::rhi::HostVulkanBuffer;

use super::pipeline_creation::pipeline_cache_file_path;
use super::spirv_validation::validate_against_spirv;
use crate::vulkan::rhi::kernels::vulkan_kernel_capability_refusal::VulkanSubgroupOperationSupport;
use std::path::PathBuf;

/// The device every dispatch test below runs on.
///
/// Under `hardware-tests` the rig is asserted to exist, so a device that
/// cannot be created is the failure — not a reason to pass. Skipping here is
/// what would let a driver producing no usable device still report a green
/// dispatch suite, which is exactly what the MoltenVK bring-up needed to see.
fn vulkan_device_for_dispatch_tests() -> Arc<HostVulkanDevice> {
    HostVulkanDevice::new().expect("the rig must produce a Vulkan device for the dispatch tests")
}

/// Allocate a HOST_VISIBLE storage buffer of `element_count * 4` bytes
/// usable as both an input and output of the test_blend kernel.
fn make_storage_buffer(device: &Arc<HostVulkanDevice>, element_count: u32) -> PixelBuffer {
    let vk_buf = HostVulkanBuffer::new(device, (element_count as u64) * 4)
        .expect("Failed to create storage buffer");
    PixelBuffer::from_host_vulkan_buffer(
        Arc::new(vk_buf),
        element_count,
        1,
        4,
        crate::core::rhi::PixelFormat::Bgra32,
    )
}

fn write_buffer_u32(buf: &PixelBuffer, values: &[u32]) {
    let ptr = buf.buffer_ref().inner.mapped_ptr() as *mut u32;
    unsafe {
        std::ptr::copy_nonoverlapping(values.as_ptr(), ptr, values.len());
    }
}

fn read_buffer_u32(buf: &PixelBuffer, len: usize) -> Vec<u32> {
    let ptr = buf.buffer_ref().inner.mapped_ptr() as *const u32;
    let mut out = vec![0u32; len];
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), len);
    }
    out
}

fn blend_descriptor(input_count: u32) -> Vec<ComputeBindingSpec> {
    let mut bindings: Vec<ComputeBindingSpec> = (0..input_count)
        .map(ComputeBindingSpec::storage_buffer)
        .collect();
    // Output sits at binding 8 in every variant of test_blend.slang.
    bindings.push(ComputeBindingSpec::storage_buffer(8));
    bindings
}

fn blend_spv(input_count: u32) -> &'static [u8] {
    match input_count {
        1 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_1.spv")),
        2 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_2.spv")),
        4 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_4.spv")),
        8 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_8.spv")),
        _ => panic!("test_blend.spv variants are only built for 1/2/4/8"),
    }
}

mod binding_validation_and_subgroup_operations;
mod dispatch_and_sampled_images;
mod pipeline_cache_on_disk;
mod samplers;
mod specialization_constants;
