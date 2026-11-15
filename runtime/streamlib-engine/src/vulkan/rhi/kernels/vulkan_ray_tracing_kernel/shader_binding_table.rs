// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Laying out a ray-tracing kernel's shader binding table and uploading it.

use std::sync::Arc;

use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrRayTracingPipelineExtensionDeviceCommands as _;
use vulkanalia_vma as vma;

use crate::core::rhi::{
    RayTracingKernelDescriptor, RayTracingShaderGroup, RayTracingShaderStage, RayTracingStage,
};
use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;

use super::{Sbt, allocate_command_buffer, create_command_pool};

/// Categorize each group into one of the four SBT regions.
#[derive(Clone, Copy)]
enum SbtRegion {
    RayGen,
    Miss,
    Hit,
    Callable,
}

fn group_region(stages: &[RayTracingStage<'_>], group: &RayTracingShaderGroup) -> SbtRegion {
    match *group {
        RayTracingShaderGroup::General { general } => match stages[general as usize].stage {
            RayTracingShaderStage::RayGen => SbtRegion::RayGen,
            RayTracingShaderStage::Miss => SbtRegion::Miss,
            RayTracingShaderStage::Callable => SbtRegion::Callable,
            other => unreachable!(
                "validate_shader_groups must reject non-{{RayGen,Miss,Callable}} stages in a General group, got {:?}",
                other
            ),
        },
        RayTracingShaderGroup::TrianglesHit { .. }
        | RayTracingShaderGroup::ProceduralHit { .. } => SbtRegion::Hit,
    }
}

fn align_up(value: vk::DeviceSize, alignment: vk::DeviceSize) -> vk::DeviceSize {
    if alignment == 0 {
        value
    } else {
        (value + alignment - 1) & !(alignment - 1)
    }
}

pub(super) fn build_sbt(
    vulkan_device: &Arc<HostVulkanDevice>,
    descriptor: &RayTracingKernelDescriptor<'_>,
    pipeline: vk::Pipeline,
    rt_props: &crate::vulkan::rhi::RayTracingPipelineProperties,
) -> Result<Sbt> {
    let device = vulkan_device.device();

    let handle_size = rt_props.shader_group_handle_size as vk::DeviceSize;
    let handle_stride = align_up(
        handle_size,
        rt_props.shader_group_handle_alignment as vk::DeviceSize,
    );
    let base_alignment = rt_props.shader_group_base_alignment as vk::DeviceSize;

    // Categorize each group. The vkGetRayTracingShaderGroupHandlesKHR call
    // returns handles in the same order as the groups in the pipeline-create
    // info, so `group_indices` matches descriptor.groups index-for-index.
    let mut raygen_indices = Vec::new();
    let mut miss_indices = Vec::new();
    let mut hit_indices = Vec::new();
    let mut callable_indices = Vec::new();
    for (i, g) in descriptor.groups.iter().enumerate() {
        match group_region(descriptor.stages, g) {
            SbtRegion::RayGen => raygen_indices.push(i),
            SbtRegion::Miss => miss_indices.push(i),
            SbtRegion::Hit => hit_indices.push(i),
            SbtRegion::Callable => callable_indices.push(i),
        }
    }
    if raygen_indices.is_empty() {
        return Err(Error::GpuError(format!(
            "Ray-tracing kernel '{}': pipeline must have at least one RayGen group",
            descriptor.label
        )));
    }

    // Region offsets (within the SBT buffer) are aligned to base_alignment.
    let raygen_offset: vk::DeviceSize = 0;
    let raygen_size = align_up(
        handle_stride * raygen_indices.len() as vk::DeviceSize,
        base_alignment,
    );

    let miss_offset = raygen_offset + raygen_size;
    let miss_size = align_up(
        handle_stride * miss_indices.len() as vk::DeviceSize,
        base_alignment,
    );

    let hit_offset = miss_offset + miss_size;
    let hit_size = align_up(
        handle_stride * hit_indices.len() as vk::DeviceSize,
        base_alignment,
    );

    let callable_offset = hit_offset + hit_size;
    let callable_size = align_up(
        handle_stride * callable_indices.len() as vk::DeviceSize,
        base_alignment,
    );

    let total_size = (callable_offset + callable_size).max(handle_stride);

    // Fetch all group handles into a single buffer.
    let group_count = descriptor.groups.len() as u32;
    let handle_data_size = (handle_size * group_count as vk::DeviceSize) as usize;
    let mut handle_blob = vec![0u8; handle_data_size];
    unsafe {
        device.get_ray_tracing_shader_group_handles_khr(pipeline, 0, group_count, &mut handle_blob)
    }
    .map_err(|e| {
        Error::GpuError(format!(
            "Ray-tracing kernel '{}': vkGetRayTracingShaderGroupHandlesKHR failed: {e}",
            descriptor.label
        ))
    })?;

    // Build the SBT in a CPU staging buffer, then upload to a DEVICE_LOCAL
    // SBT buffer via vkCmdCopyBuffer. Direct DEVICE_LOCAL mapping isn't
    // portable across drivers (NVIDIA-style discrete VRAM is not
    // host-visible), so the staging path is the conservative shape.
    let mut staging_data = vec![0u8; total_size as usize];
    write_region(
        &mut staging_data,
        raygen_offset,
        handle_stride,
        &raygen_indices,
        &handle_blob,
        handle_size,
    );
    write_region(
        &mut staging_data,
        miss_offset,
        handle_stride,
        &miss_indices,
        &handle_blob,
        handle_size,
    );
    write_region(
        &mut staging_data,
        hit_offset,
        handle_stride,
        &hit_indices,
        &handle_blob,
        handle_size,
    );
    write_region(
        &mut staging_data,
        callable_offset,
        handle_stride,
        &callable_indices,
        &handle_blob,
        handle_size,
    );

    // Allocate the DEVICE_LOCAL SBT buffer.
    let allocator = vulkan_device.allocator();
    let buffer_info = vk::BufferCreateInfo::builder()
        .size(total_size)
        .usage(
            vk::BufferUsageFlags::SHADER_BINDING_TABLE_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
                | vk::BufferUsageFlags::TRANSFER_DST,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .build();
    let alloc_opts = vma::AllocationOptions {
        usage: vma::MemoryUsage::AutoPreferDevice,
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };
    let (sbt_buffer, sbt_allocation) = unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }
        .map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel '{}': SBT vmaCreateBuffer (size={total_size}) failed: {e}",
                descriptor.label
            ))
        })?;
    let address_info = vk::BufferDeviceAddressInfo::builder()
        .buffer(sbt_buffer)
        .build();
    let sbt_address = unsafe { device.get_buffer_device_address(&address_info) };

    // Stage + upload.
    if let Err(e) =
        upload_bytes_to_buffer(vulkan_device, sbt_buffer, &staging_data, descriptor.label)
    {
        unsafe { allocator.destroy_buffer(sbt_buffer, sbt_allocation) };
        return Err(e);
    }

    Ok(Sbt {
        buffer: sbt_buffer,
        allocation: Some(sbt_allocation),
        raygen_region: vk::StridedDeviceAddressRegionKHR {
            device_address: sbt_address + raygen_offset,
            stride: handle_stride,
            size: handle_stride * raygen_indices.len() as vk::DeviceSize,
        },
        miss_region: vk::StridedDeviceAddressRegionKHR {
            device_address: if miss_indices.is_empty() {
                0
            } else {
                sbt_address + miss_offset
            },
            stride: if miss_indices.is_empty() {
                0
            } else {
                handle_stride
            },
            size: handle_stride * miss_indices.len() as vk::DeviceSize,
        },
        hit_region: vk::StridedDeviceAddressRegionKHR {
            device_address: if hit_indices.is_empty() {
                0
            } else {
                sbt_address + hit_offset
            },
            stride: if hit_indices.is_empty() {
                0
            } else {
                handle_stride
            },
            size: handle_stride * hit_indices.len() as vk::DeviceSize,
        },
        callable_region: vk::StridedDeviceAddressRegionKHR {
            device_address: if callable_indices.is_empty() {
                0
            } else {
                sbt_address + callable_offset
            },
            stride: if callable_indices.is_empty() {
                0
            } else {
                handle_stride
            },
            size: handle_stride * callable_indices.len() as vk::DeviceSize,
        },
    })
}

fn write_region(
    out: &mut [u8],
    region_offset: vk::DeviceSize,
    stride: vk::DeviceSize,
    group_indices: &[usize],
    handle_blob: &[u8],
    handle_size: vk::DeviceSize,
) {
    for (i, &group_idx) in group_indices.iter().enumerate() {
        let dst_off = (region_offset + stride * i as vk::DeviceSize) as usize;
        let src_off = group_idx * handle_size as usize;
        let len = handle_size as usize;
        out[dst_off..dst_off + len].copy_from_slice(&handle_blob[src_off..src_off + len]);
    }
}

fn upload_bytes_to_buffer(
    vulkan_device: &Arc<HostVulkanDevice>,
    dst_buffer: vk::Buffer,
    bytes: &[u8],
    label: &str,
) -> Result<()> {
    let device = vulkan_device.device();
    let allocator = vulkan_device.allocator();
    let size = bytes.len() as vk::DeviceSize;
    if size == 0 {
        return Ok(());
    }

    let staging_info = vk::BufferCreateInfo::builder()
        .size(size)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .build();
    let staging_opts = vma::AllocationOptions {
        usage: vma::MemoryUsage::AutoPreferHost,
        required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT,
        flags: vma::AllocationCreateFlags::MAPPED
            | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
        ..Default::default()
    };
    let (staging_buffer, staging_allocation) =
        unsafe { allocator.create_buffer(staging_info, &staging_opts) }.map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel '{label}': SBT staging vmaCreateBuffer ({size}) failed: {e}"
            ))
        })?;
    let info = allocator.get_allocation_info(staging_allocation);
    let dst_ptr = info.pMappedData as *mut u8;
    if dst_ptr.is_null() {
        unsafe { allocator.destroy_buffer(staging_buffer, staging_allocation) };
        return Err(Error::GpuError(format!(
            "Ray-tracing kernel '{label}': SBT staging mapping returned null"
        )));
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst_ptr, bytes.len()) };

    let queue = vulkan_device.queue();
    let queue_family = vulkan_device.queue_family_index();
    let command_pool = create_command_pool(device, queue_family).inspect_err(|_e| {
        unsafe { allocator.destroy_buffer(staging_buffer, staging_allocation) };
    })?;
    let cmd = match allocate_command_buffer(device, command_pool) {
        Ok(c) => c,
        Err(e) => {
            unsafe {
                device.destroy_command_pool(command_pool, None);
                allocator.destroy_buffer(staging_buffer, staging_allocation);
            }
            return Err(e);
        }
    };
    let begin = vk::CommandBufferBeginInfo::builder()
        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
        .build();
    unsafe { device.begin_command_buffer(cmd, &begin) }.map_err(|e| {
        unsafe {
            device.destroy_command_pool(command_pool, None);
            allocator.destroy_buffer(staging_buffer, staging_allocation);
        }
        Error::GpuError(format!(
            "Ray-tracing kernel '{label}': SBT begin_command_buffer failed: {e}"
        ))
    })?;
    let region = vk::BufferCopy::builder()
        .src_offset(0)
        .dst_offset(0)
        .size(size)
        .build();
    unsafe { device.cmd_copy_buffer(cmd, staging_buffer, dst_buffer, &[region]) };
    unsafe { device.end_command_buffer(cmd) }.map_err(|e| {
        unsafe {
            device.destroy_command_pool(command_pool, None);
            allocator.destroy_buffer(staging_buffer, staging_allocation);
        }
        Error::GpuError(format!(
            "Ray-tracing kernel '{label}': SBT end_command_buffer failed: {e}"
        ))
    })?;

    let fence_info = vk::FenceCreateInfo::builder().build();
    let fence = unsafe { device.create_fence(&fence_info, None) }.map_err(|e| {
        unsafe {
            device.destroy_command_pool(command_pool, None);
            allocator.destroy_buffer(staging_buffer, staging_allocation);
        }
        Error::GpuError(format!(
            "Ray-tracing kernel '{label}': SBT fence creation failed: {e}"
        ))
    })?;

    let cmd_info = vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build();
    let cmd_infos = [cmd_info];
    let submit = vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build();

    let submit_result =
        unsafe { HostVulkanDevice::submit_to_queue(vulkan_device, queue, &[submit], fence) };
    let wait_result: Result<()> = if submit_result.is_ok() {
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map(|_| ())
            .map_err(|e| {
                Error::GpuError(format!(
                    "Ray-tracing kernel '{label}': SBT wait_for_fences failed: {e}"
                ))
            })
    } else {
        Ok(())
    };

    unsafe {
        device.destroy_fence(fence, None);
        device.destroy_command_pool(command_pool, None);
        allocator.destroy_buffer(staging_buffer, staging_allocation);
    }

    submit_result.and(wait_result)
}

pub(super) fn drop_sbt(sbt: &Sbt, vulkan_device: &Arc<HostVulkanDevice>) {
    if let Some(allocation) = sbt.allocation.as_ref().copied() {
        unsafe {
            vulkan_device
                .allocator()
                .destroy_buffer(sbt.buffer, allocation)
        };
    }
}
