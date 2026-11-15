// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Exporting buffers on Linux as OPAQUE_FD or DMA-BUF, and importing a host-cached export at its stated memory type.

use super::*;

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::sync::Arc;

use crate::vulkan::rhi::dma_buf_export_test_device_fixture::a_device_that_exports_dma_buf_or_skip;

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_dma_buf_export() {
    let Some(device) = a_device_that_exports_dma_buf_or_skip("test_dma_buf_export") else {
        return;
    };

    let buf = HostVulkanBuffer::new(&device, 1920_u64 * 1080_u64 * 4_u64)
        .expect("buffer creation failed");

    let fd = buf.export_dma_buf_fd().expect("DMA-BUF export failed");
    assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");

    println!("DMA-BUF exported: fd={fd}");
    // fd ownership is caller's — close it
    unsafe { libc::close(fd) };
}

/// A host-cached export lands on a HOST_CACHED memory type, or the
/// device has no cached exportable type and the pool is absent —
/// there is no third outcome, and in particular no refusal.
///
/// Mental-revert: point `new_opaque_fd_export_host_cached` at
/// `opaque_fd_buffer_pool()` and the flags assertion fails on a rig
/// whose cached type differs from its write-combined one. Verified.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_host_cached_export_is_cached_or_the_device_has_no_cached_exportable_type() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_buffer_pool().is_none() {
        println!("Skipping - OPAQUE_FD buffer pool unavailable on this driver");
        return;
    }

    const SIZE: u64 = 128 * 128 * 4;
    let buffer = HostVulkanBuffer::new_opaque_fd_export_host_cached(&device, SIZE)
        .expect("a host-cached export must degrade, never refuse");
    assert_eq!(buffer.size(), SIZE as vk::DeviceSize);
    assert!(
        !buffer.mapped_ptr().is_null(),
        "a host-visible readback staging must be persistently mapped"
    );

    let memory_type_index = buffer
        .vma_allocation_memory_type_index()
        .expect("a VMA-allocated buffer states its memory type index");
    let flags = device.allocator().get_memory_properties().memory_types[memory_type_index as usize]
        .property_flags;
    assert!(
        flags.contains(
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
        ),
        "the child-side OPAQUE_FD import requires HOST_VISIBLE | HOST_COHERENT and \
         nothing on this path flushes or invalidates"
    );
    if device.opaque_fd_buffer_pool_host_cached().is_some() {
        assert!(
            flags.contains(vk::MemoryPropertyFlags::HOST_CACHED),
            "a device with a cached exportable pool must allocate the readback \
             staging from it, not from the write-combined pool"
        );
    } else {
        println!(
            "Device has no cached exportable memory type — the readback staging \
             degrades to the write-combined pool, which is the stated fallback"
        );
    }

    let fd = buffer
        .export_opaque_fd_memory()
        .expect("a host-cached export is still an OPAQUE_FD export");
    assert!(fd >= 0, "OPAQUE_FD fd must be non-negative");
    unsafe { libc::close(fd) };
}

/// The consumer binds the memory type index the exporter states,
/// which is the whole point of putting it on the surface-share wire:
/// OPAQUE_FD has no fd-properties query, so an importer left to
/// search agrees with a host-cached exporter only by coincidence.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_consumer_import_binds_the_stated_memory_type_index_of_a_host_cached_export() {
    use streamlib_consumer_rhi::{ConsumerVulkanBuffer, ConsumerVulkanDevice};

    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_buffer_pool().is_none() {
        println!("Skipping - OPAQUE_FD buffer pool unavailable on this driver");
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

    const SIZE: u64 = 128 * 128 * 4;
    const HOST_STAMP: u8 = 0xA7;
    let exported = HostVulkanBuffer::new_opaque_fd_export_host_cached(&device, SIZE)
        .expect("host-cached export");
    unsafe { std::ptr::write_bytes(exported.mapped_ptr(), HOST_STAMP, SIZE as usize) };
    let stated_memory_type_index = exported
        .vma_allocation_memory_type_index()
        .expect("the exporter states its memory type index");
    let fd = exported.export_opaque_fd_memory().expect("export the fd");

    // SAFETY: a fresh fd the export just returned; nothing else holds it.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let imported = ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
        &consumer_device,
        fd,
        SIZE as vk::DeviceSize,
        stated_memory_type_index,
    )
    .unwrap_or_else(|failure| panic!("a conforming stated-index import must bind: {failure}"));

    let seen = unsafe { std::slice::from_raw_parts(imported.mapped_ptr(), SIZE as usize) };
    assert!(
        seen.iter().all(|byte| *byte == HOST_STAMP),
        "the importer must map the exporter's own memory, not a fresh allocation"
    );
}

/// An index the imported buffer cannot bind is refused by name
/// rather than handed to `vkBindBufferMemory`, which would violate
/// VUID-vkBindBufferMemory-memory-01035 inside the driver, and the
/// refusal closes the fd it was handed — as does a zero-size refusal.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_stated_memory_type_index_the_importer_cannot_bind_is_refused_by_name() {
    use crate::core::rhi::plane_fd_under_test::PlaneFdUnderTest;
    use streamlib_consumer_rhi::{
        ConsumerVulkanBuffer, ConsumerVulkanDevice,
        vulkan_device_uuid_of_the_first_device_the_loader_enumerates,
    };

    let consumer_device = match vulkan_device_uuid_of_the_first_device_the_loader_enumerates()
        .and_then(ConsumerVulkanDevice::new_on_the_device_the_runtime_named)
    {
        Ok(d) => Arc::new(d),
        Err(e) => {
            println!("Skipping - ConsumerVulkanDevice unavailable: {e}");
            return;
        }
    };

    const NOT_A_MEMORY_TYPE_INDEX: u32 = 99;
    let handed_fd = PlaneFdUnderTest::mint();
    let refusal = match ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
        &consumer_device,
        // SAFETY: the pipe's read end, handed over; the fixture closes it
        // only if this import did not.
        unsafe { OwnedFd::from_raw_fd(handed_fd.plane_fd) },
        4096,
        NOT_A_MEMORY_TYPE_INDEX,
    ) {
        Ok(_) => panic!("an index past VK_MAX_MEMORY_TYPES binds nothing"),
        Err(refusal) => refusal.to_string(),
    };
    assert!(
        refusal.contains("99") && refusal.contains("memoryTypeBits"),
        "the refusal must name the stated index and what the buffer can bind, \
         got: {refusal}"
    );
    assert!(
        handed_fd.was_closed(),
        "the memoryTypeBits refusal must close the fd it was handed"
    );

    let handed_fd = PlaneFdUnderTest::mint();
    ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
        &consumer_device,
        // SAFETY: as above.
        unsafe { OwnedFd::from_raw_fd(handed_fd.plane_fd) },
        0,
        0,
    )
    .err()
    .expect("a zero-size import is refused");
    assert!(
        handed_fd.was_closed(),
        "the zero-size refusal must close the fd it was handed"
    );
}

/// `new_opaque_fd_export` allocates from the OPAQUE_FD pool;
/// `export_opaque_fd_memory` returns a valid kernel fd; cross-flavor
/// export is rejected (calling `export_opaque_fd_memory` on a DMA-BUF
/// buffer produces an error).
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn opaque_fd_export_round_trip_and_cross_flavor_rejection() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_buffer_pool().is_none() {
        println!("Skipping - OPAQUE_FD buffer pool unavailable on this driver");
        return;
    }

    // Positive: OPAQUE_FD-allocated buffer exports an OPAQUE_FD fd.
    let buf = HostVulkanBuffer::new_opaque_fd_export(&device, 128_u64 * 128_u64 * 4_u64)
        .expect("new_opaque_fd_export failed");
    assert!(
        !buf.mapped_ptr().is_null(),
        "mapped pointer should be non-null"
    );
    assert_eq!(buf.size(), (128 * 128 * 4) as vk::DeviceSize);
    let fd = buf
        .export_opaque_fd_memory()
        .expect("export_opaque_fd_memory failed");
    assert!(fd >= 0, "OPAQUE_FD fd must be non-negative");
    unsafe { libc::close(fd) };

    // Negative: DMA-BUF-allocated buffer rejects OPAQUE_FD export.
    let dma_buf =
        HostVulkanBuffer::new(&device, 64_u64 * 64_u64 * 4_u64).expect("dma-buf buffer failed");
    match dma_buf.export_opaque_fd_memory() {
        Err(crate::core::Error::GpuError(msg)) => {
            assert!(
                msg.contains("not created with `new_opaque_fd_export`"),
                "error must call out the cross-flavor mismatch, got: {msg}"
            );
        }
        other => panic!("expected cross-flavor rejection on DMA-BUF buffer, got {other:?}"),
    }
}

/// `new_opaque_fd_export_device_local` allocates from the dedicated
/// DEVICE_LOCAL pool, returns a non-mappable buffer (mapped_ptr null
/// by construction), and exports an OPAQUE_FD fd. Sibling of
/// `opaque_fd_export_round_trip_and_cross_flavor_rejection` covering
/// the GPU-resident path.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn opaque_fd_device_local_export_round_trip() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    if device.opaque_fd_buffer_pool_device_local().is_none() {
        println!("Skipping - DEVICE_LOCAL OPAQUE_FD buffer pool unavailable on this driver");
        return;
    }

    let buf =
        HostVulkanBuffer::new_opaque_fd_export_device_local(&device, 128_u64 * 128_u64 * 4_u64)
            .expect("new_opaque_fd_export_device_local failed");
    // DEVICE_LOCAL allocations are not host-mapped.
    assert!(
        buf.mapped_ptr().is_null(),
        "DEVICE_LOCAL OPAQUE_FD buffers must not expose a host pointer; \
         callers populate via vkCmdCopyImageToBuffer / vkCmdBlitImage"
    );
    assert_eq!(buf.size(), (128 * 128 * 4) as vk::DeviceSize);
    let fd = buf
        .export_opaque_fd_memory()
        .expect("export_opaque_fd_memory failed");
    assert!(fd >= 0, "OPAQUE_FD fd must be non-negative");
    unsafe { libc::close(fd) };
}

/// `export_external_handle` dispatches to the correct
/// `RhiExternalHandle` variant for each allocation flavor.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn export_external_handle_dispatches_on_allocation_flavor() {
    use crate::core::rhi::RhiExternalHandle;

    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    // DMA-BUF flavor → DmaBuf variant, on a device that exports DMA-BUF;
    // a refusal naming the device on one that does not.
    let dma_buf =
        HostVulkanBuffer::new(&device, 64_u64 * 64_u64 * 4_u64).expect("dma-buf buffer failed");
    match dma_buf.export_external_handle() {
        Ok(RhiExternalHandle::DmaBuf { fd, size }) if device.supports_dma_buf_export() => {
            assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");
            assert_eq!(size, (64 * 64 * 4) as usize);
            unsafe { libc::close(fd) };
        }
        Err(Error::NotSupported(refusal)) if !device.supports_dma_buf_export() => {
            assert!(refusal.contains(&device.name()), "{refusal}");
            assert!(refusal.contains("exports no DMA-BUF"), "{refusal}");
        }
        other => panic!(
            "expected DmaBuf on a DMA-BUF device, a refusal elsewhere (supports_dma_buf_export \
             = {}), got: {other:?}",
            device.supports_dma_buf_export()
        ),
    }

    // OPAQUE_FD flavor → OpaqueFd variant. Skip if the pool isn't
    // available on this driver (already tested separately above).
    if device.opaque_fd_buffer_pool().is_some() {
        let opaque_buf = HostVulkanBuffer::new_opaque_fd_export(&device, 64_u64 * 64_u64 * 4_u64)
            .expect("new_opaque_fd_export failed");
        match opaque_buf.export_external_handle() {
            Ok(RhiExternalHandle::OpaqueFd {
                fd,
                size,
                exporting_allocation_memory_type_index,
            }) => {
                assert!(fd >= 0, "OPAQUE_FD fd must be non-negative, got {fd}");
                assert_eq!(size, (64 * 64 * 4) as usize);
                assert_eq!(
                    Some(exporting_allocation_memory_type_index),
                    opaque_buf.vma_allocation_memory_type_index(),
                    "the handle states the allocation's own memory type"
                );
                unsafe { libc::close(fd) };
            }
            other => panic!("expected OpaqueFd, got: {other:?}"),
        }
        match opaque_buf.export_dma_buf_fd() {
            Err(Error::NotSupported(refusal)) => {
                assert!(refusal.contains(&device.name()), "{refusal}");
                assert!(refusal.contains("exports no DMA-BUF"), "{refusal}");
            }
            other => panic!("an OPAQUE_FD buffer must refuse a DMA-BUF export, got {other:?}"),
        }
    }
}

/// `physical_device_uuid()` returns 16 bytes that look like a real
/// UUID — neither all-zero (would indicate `vkGetPhysicalDeviceProperties2`
/// never ran) nor all-the-same-byte (would catch a constant-write bug
/// like `[0u8; 16].fill(1)`). Real GPU UUIDs from NVIDIA / AMD / Intel
/// have many distinct byte values; a threshold of >= 4 distinct bytes
/// is comfortably below every observed real-device UUID and well
/// above any plausible constant-write bug.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn physical_device_uuid_is_populated() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };
    let uuid = device.physical_device_uuid();
    assert!(
        uuid.iter().any(|b| *b != 0),
        "physical_device_uuid should not be all-zero — got {uuid:?}"
    );
    let distinct: std::collections::HashSet<u8> = uuid.iter().copied().collect();
    assert!(
        distinct.len() >= 4,
        "physical_device_uuid should have >= 4 distinct byte values \
         (real GPU UUIDs do; a constant-write bug wouldn't) — got {uuid:02x?} \
         with {} distinct values",
        distinct.len()
    );

    // Cheap idempotence check: the accessor is a copy of a stored
    // array; calling it twice must yield identical bytes.
    assert_eq!(
        uuid,
        device.physical_device_uuid(),
        "physical_device_uuid must be stable across calls"
    );

    println!("physical_device_uuid: {uuid:02x?}");
}
