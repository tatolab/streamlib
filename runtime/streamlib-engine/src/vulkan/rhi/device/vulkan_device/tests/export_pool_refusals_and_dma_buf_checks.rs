// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use super::*;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::device::vulkan_device::export_memory_pools::refuse_an_image_the_export_pools_memory_type_cannot_bind;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::device::vulkan_device::opaque_fd_memory_import_and_dma_buf_fd_refusal::refuse_a_file_descriptor_that_is_not_a_dma_buf;

#[cfg(target_os = "linux")]
#[test]
fn an_image_that_cannot_bind_the_export_pools_memory_type_is_refused_naming_its_shape() {
    let image_create_info = vk::ImageCreateInfo::builder()
        .format(vk::Format::B8G8R8A8_UNORM)
        .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .build();
    const EXPORT_POOL_MEMORY_TYPE_INDEX: u32 = 2;

    let refusal = refuse_an_image_the_export_pools_memory_type_cannot_bind(
        0b1011,
        EXPORT_POOL_MEMORY_TYPE_INDEX,
        &image_create_info,
        "an allocation site under test",
    )
    .expect_err("memory_type_bits without bit 2 cannot bind type 2")
    .to_string();
    assert!(
        refusal.contains("an allocation site under test"),
        "{refusal}"
    );
    assert!(refusal.contains("B8G8R8A8_UNORM"), "{refusal}");
    assert!(refusal.contains("COLOR_ATTACHMENT"), "{refusal}");
    assert!(refusal.contains("memory type 2"), "{refusal}");

    refuse_an_image_the_export_pools_memory_type_cannot_bind(
        0b0100,
        EXPORT_POOL_MEMORY_TYPE_INDEX,
        &image_create_info,
        "an allocation site under test",
    )
    .expect("memory_type_bits with bit 2 bind type 2");
}

#[cfg(target_os = "linux")]
fn inode_of(fd: std::os::unix::io::RawFd) -> Option<u64> {
    let mut file_status = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, file_status.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(unsafe { file_status.assume_init() }.st_ino)
}

/// The one exit of the import that is ours to close: no memory type
/// matches, so the fd never reaches `vkAllocateMemory` and nobody but
/// this function ever held it.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_memory_type_refusal_before_the_import_closes_the_fd_it_was_handed() {
    use std::os::fd::{FromRawFd as _, OwnedFd};

    let Some(device) = a_device_that_exports_dma_buf_or_skip(
        "a_memory_type_refusal_before_the_import_closes_the_fd_it_was_handed",
    ) else {
        return;
    };
    let source =
        crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(&device, 4096)
            .expect("source buffer allocation failed");
    let fd = source.export_dma_buf_fd().expect("DMA-BUF export failed");
    let inode = inode_of(fd).expect("an exported DMA-BUF must stat");

    let refusal = device
        .import_dma_buf_memory(
            unsafe { OwnedFd::from_raw_fd(fd) },
            4096,
            0,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .expect_err("no memory type matches an empty type filter");
    assert!(
        refusal.to_string().contains("No suitable memory type"),
        "the refusal must be the memory-type search, not the driver: {refusal}"
    );
    assert_ne!(
        inode_of(fd),
        Some(inode),
        "the refused import left the DMA-BUF fd open — no owner remains to close it"
    );
}

/// A memfd and a pipe are refused and closed without a device. A kept
/// second descriptor of each file holds its inode, so a closed number
/// that a parallel test reopens can never stat as the same file.
#[cfg(target_os = "linux")]
#[test]
fn a_file_descriptor_that_is_not_a_dma_buf_is_refused_and_closed() {
    use std::os::fd::{FromRawFd as _, OwnedFd};

    let memfd = unsafe { libc::memfd_create(c"not-a-dma-buf".as_ptr(), 0) };
    assert!(
        memfd >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    let memfd_duplicate = unsafe { libc::fcntl(memfd, libc::F_DUPFD_CLOEXEC, 0) };
    assert!(
        memfd_duplicate >= 0,
        "dup: {}",
        std::io::Error::last_os_error()
    );
    let _memfd_kept_open = unsafe { OwnedFd::from_raw_fd(memfd_duplicate) };

    let mut pipe_ends = [0 as std::os::unix::io::RawFd; 2];
    assert_eq!(
        unsafe { libc::pipe2(pipe_ends.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let _pipe_write_end_kept_open = unsafe { OwnedFd::from_raw_fd(pipe_ends[1]) };

    for (candidate_fd, descriptor_kind) in [(memfd, "memfd"), (pipe_ends[0], "pipe")] {
        let inode = inode_of(candidate_fd).expect("a fresh fd must stat");
        let refusal = refuse_a_file_descriptor_that_is_not_a_dma_buf(unsafe {
            OwnedFd::from_raw_fd(candidate_fd)
        })
        .expect_err("only a DMA-BUF passes");
        assert!(
            refusal.to_string().contains("is not a DMA-BUF"),
            "the {descriptor_kind} refusal must say why: {refusal}"
        );
        assert_ne!(
            inode_of(candidate_fd),
            Some(inode),
            "the refused {descriptor_kind} fd was left open — no owner remains to close it"
        );
    }
}

#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn an_exported_dma_buf_passes_the_dma_buf_check_as_the_same_fd() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    let Some(device) = a_device_that_exports_dma_buf_or_skip(
        "an_exported_dma_buf_passes_the_dma_buf_check_as_the_same_fd",
    ) else {
        return;
    };
    let source =
        crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(&device, 4096)
            .expect("source buffer allocation failed");
    let fd = source.export_dma_buf_fd().expect("DMA-BUF export failed");

    let fd_that_passed_the_dma_buf_check =
        refuse_a_file_descriptor_that_is_not_a_dma_buf(unsafe { OwnedFd::from_raw_fd(fd) })
            .expect("an exported DMA-BUF must pass");
    assert_eq!(fd_that_passed_the_dma_buf_check.as_raw_fd(), fd);
}

/// Build a `VkPhysicalDeviceMemoryProperties` whose first
/// `property_flags.len()` types carry the given flags.
#[cfg(target_os = "linux")]
fn memory_properties_with_types(
    property_flags: &[vk::MemoryPropertyFlags],
) -> vk::PhysicalDeviceMemoryProperties {
    let mut memory_properties = vk::PhysicalDeviceMemoryProperties {
        memory_type_count: property_flags.len() as u32,
        ..Default::default()
    };
    for (memory_type, flags) in memory_properties
        .memory_types
        .iter_mut()
        .zip(property_flags.iter())
    {
        memory_type.property_flags = *flags;
    }
    memory_properties
}

/// The host-cached pool's presence is decided by reading the probed
/// index's flags, never by the probe failing: VMA prefers HOST_CACHED
/// under `HOST_ACCESS_RANDOM` but cannot require it, so a cache-less
/// device answers a *successful* probe with an uncached index.
///
/// Mental-revert: return `true` unconditionally and a cache-less
/// device pins its readback pool to write-combined memory while
/// reporting it cached. Verified.
#[cfg(target_os = "linux")]
#[test]
fn a_device_whose_probed_type_is_not_host_cached_gets_no_host_cached_pool() {
    let uncached = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let cached = uncached | vk::MemoryPropertyFlags::HOST_CACHED;

    let cache_less_device =
        memory_properties_with_types(&[vk::MemoryPropertyFlags::DEVICE_LOCAL, uncached]);
    assert!(
        !memory_type_index_is_host_cached(&cache_less_device, 1),
        "a HOST_VISIBLE | HOST_COHERENT type without HOST_CACHED must not pass"
    );

    let cached_device =
        memory_properties_with_types(&[vk::MemoryPropertyFlags::DEVICE_LOCAL, uncached, cached]);
    assert!(
        memory_type_index_is_host_cached(&cached_device, 2),
        "the cached exportable type must pass"
    );
    assert!(
        !memory_type_index_is_host_cached(&cached_device, 1),
        "the flags are read at the probed index, not searched for"
    );
}

/// An index past `memoryTypeCount` names no memory type, so it is
/// not cached — the trailing entries of the fixed 32-slot array are
/// zeroed padding, never allocatable types.
#[cfg(target_os = "linux")]
#[test]
fn a_memory_type_index_past_the_device_count_is_not_host_cached() {
    let memory_properties = memory_properties_with_types(&[vk::MemoryPropertyFlags::HOST_VISIBLE
        | vk::MemoryPropertyFlags::HOST_COHERENT
        | vk::MemoryPropertyFlags::HOST_CACHED]);
    assert!(memory_type_index_is_host_cached(&memory_properties, 0));
    assert!(!memory_type_index_is_host_cached(&memory_properties, 1));
    assert!(!memory_type_index_is_host_cached(&memory_properties, 31));
    assert!(!memory_type_index_is_host_cached(
        &memory_properties,
        u32::MAX
    ));
}
