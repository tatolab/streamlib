// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use super::*;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::vulkan_device::export_pool_sentinels::{
    ExportPoolSentinelResource, make_export_pool_buffer_sentinel,
};
#[cfg(target_os = "linux")]
use vma::Alloc as _;

/// Every export pool the device created holds a tiny sentinel for the
/// device's life — the anchor NVIDIA's per-handle-type export state needs
/// after a swapchain exists, which the shared pools freeing their frames
/// would otherwise take away. Locks it at the data-structure level, since
/// the driver failure itself needs a live swapchain and real camera to
/// reproduce.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn every_export_pool_the_device_created_keeps_a_tiny_sentinel_for_its_life() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };

    // Built from the pools the device actually created, so the assertion
    // holds on any driver; empty only when no export pool exists.
    let mut expected_labels: Vec<&'static str> = Vec::new();
    if device.dma_buf_buffer_pool().is_some() {
        expected_labels.push("dma_buf_host_visible");
    }
    if device.dma_buf_image_pool().is_some() {
        expected_labels.push("dma_buf_image");
    }
    if device.dma_buf_image_pool_tiled().is_some() {
        expected_labels.push("dma_buf_image_tiled");
    }
    if device.opaque_fd_buffer_pool().is_some() {
        expected_labels.push("opaque_fd_host_visible");
    }
    if device.opaque_fd_buffer_pool_host_cached().is_some() {
        expected_labels.push("opaque_fd_host_cached");
    }
    if device.opaque_fd_buffer_pool_device_local().is_some() {
        expected_labels.push("opaque_fd_device_local");
    }
    if device.opaque_fd_image_pool().is_some() {
        expected_labels.push("opaque_fd_image");
    }
    if expected_labels.is_empty() {
        println!("Skipping — no export pools on this driver, so no sentinels expected");
        return;
    }

    let actual_labels: Vec<&'static str> = device
        .export_pool_sentinels
        .iter()
        .map(|sentinel| sentinel.label)
        .collect();
    assert_eq!(
        actual_labels, expected_labels,
        "every export pool the device created must hold a sentinel for the device's life"
    );
    if let Some(validation_message_counts) = device.validation_layer_message_counts() {
        assert_eq!(
            validation_message_counts.error_count, 0,
            "allocating the export pool sentinels raised validation errors"
        );
    }

    // A consumer-sized sentinel deterministically blocked the consumer's
    // own allocation on NVIDIA (#637).
    let largest_acceptable_sentinel_bytes: vk::DeviceSize = 64 * 1024;
    for sentinel in device.export_pool_sentinels.iter() {
        let label = sentinel.label;
        assert!(
            sentinel.size <= largest_acceptable_sentinel_bytes,
            "export pool sentinel '{label}' must stay tiny (got {} bytes, at most {})",
            sentinel.size,
            largest_acceptable_sentinel_bytes,
        );
        let live_allocation_bytes = device
            .allocator()
            .get_allocation_info(sentinel.allocation)
            .size;
        assert!(
            live_allocation_bytes > 0,
            "export pool sentinel '{label}' must hold a live allocation"
        );
        match sentinel.resource {
            ExportPoolSentinelResource::Buffer(buffer) => {
                assert_ne!(
                    buffer,
                    vk::Buffer::null(),
                    "sentinel '{label}' has null VkBuffer"
                );
            }
            ExportPoolSentinelResource::Image(image) => {
                assert_ne!(
                    image,
                    vk::Image::null(),
                    "sentinel '{label}' has null VkImage"
                );
            }
        }
    }
}

/// Sentinel allocation returns early when a later sentinel fails,
/// dropping the sentinels it already made. Each must free its own allocation, or the
/// device's pool teardown aborts inside VMA with `Unfreed dedicated
/// allocations found!` (#2247).
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn an_export_pool_sentinel_dropped_before_the_device_frees_its_pool_allocation() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };
    let Some(pinned_pool) = device.opaque_fd_buffer_pool_pinned_to_one_memory_type() else {
        println!("Skipping — no OPAQUE_FD buffer pool on this driver");
        return;
    };
    let live_allocations_in_pool = || {
        pinned_pool
            .pool
            .calculate_statistics()
            .expect("pool statistics")
            .statistics
            .allocationCount
    };
    let allocations_before_the_sentinel = live_allocations_in_pool();

    let sentinel = make_export_pool_buffer_sentinel(
        &device,
        pinned_pool,
        "opaque_fd_host_visible",
        vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        MappedOpaqueFdBufferHostAccessPattern::SequentialWrite.vma_allocation_create_flags(),
    )
    .expect("a tiny OPAQUE_FD sentinel allocates");
    assert_eq!(
        live_allocations_in_pool(),
        allocations_before_the_sentinel + 1,
        "the sentinel allocates out of the pool it pins"
    );

    drop(sentinel);
    assert_eq!(
        live_allocations_in_pool(),
        allocations_before_the_sentinel,
        "a sentinel dropped outside the device's teardown must free its allocation"
    );
}

#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_dma_buf_import_round_trip() {
    use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;

    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };

    if !device.supports_external_memory() {
        println!("Skipping test — external memory not supported");
        return;
    }

    // The export pool is what carries `VkExportMemoryAllocateInfo` onto
    // the allocation, which `vkGetMemoryFdKHR` below requires. Allocating
    // straight from `allocator()` — the default pool — yields memory that
    // cannot legally be exported.
    let Some(export_pool) = device.dma_buf_buffer_pool() else {
        skip_because_this_machine_lacks(RigOnlyCapability::DmaBufModifiers);
        return;
    };

    let buffer_size: vk::DeviceSize = 4096;

    let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .build();

    let buffer_info = vk::BufferCreateInfo::builder()
        .size(buffer_size)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external_buffer_info);

    // No `required_flags`: the pool's memory type was fixed at
    // `create_pool` time, and VMA ignores memory-type selection once
    // `pool` is set.
    let alloc_opts = vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
        ..Default::default()
    };

    let (buffer, allocation) = unsafe { export_pool.create_buffer(buffer_info, &alloc_opts) }
        .expect("create exportable buffer from the DMA-BUF export pool");

    // Get allocation info to access the underlying DeviceMemory for export
    let alloc_info = device.allocator().get_allocation_info(allocation);
    let memory = alloc_info.deviceMemory;

    // Export DMA-BUF fd via vulkanalia extension trait
    let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
        .memory(memory)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .build();

    let fd = unsafe { device.device().get_memory_fd_khr(&get_fd_info) }.expect("export DMA-BUF fd");

    assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");
    println!("Exported DMA-BUF fd: {fd}");

    // Import the fd back
    let mem_reqs = unsafe { device.device().get_buffer_memory_requirements(buffer) };
    let _imported = device
        .import_dma_buf_memory(
            unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) },
            mem_reqs.size.max(buffer_size),
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
        .expect("import DMA-BUF memory");

    println!("DMA-BUF import round-trip passed");

    // Cleanup
    device.free_imported_memory(_imported);
    unsafe { device.allocator().destroy_buffer(buffer, allocation) };
}
