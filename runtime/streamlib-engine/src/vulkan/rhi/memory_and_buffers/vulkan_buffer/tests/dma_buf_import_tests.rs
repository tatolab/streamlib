// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Importing DMA-BUFs on Linux: single and multi-plane buffers, storage buffers, and the refusals that close what they were handed.

use super::*;

use std::os::fd::{FromRawFd as _, OwnedFd};

use crate::vulkan::rhi::device::dma_buf_export_test_device_fixture::a_device_that_exports_dma_buf_or_skip;

/// A pipe's read end stands in for a plane fd a refusal must close;
/// a fake number would make that close land on someone else's fd.
fn a_pipe_read_end() -> OwnedFd {
    let (read_end, _write_end_closed_here) = std::io::pipe().expect("a pipe opens");
    OwnedFd::from(read_end)
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_dma_buf_import_round_trip() {
    let Some(device) = a_device_that_exports_dma_buf_or_skip("test_dma_buf_import_round_trip")
    else {
        return;
    };

    // Create source buffer and write a known pattern
    let width = 64u32;
    let height = 64u32;
    let bpp = 4u32;
    let src = HostVulkanBuffer::new(&device, (width as u64) * (height as u64) * (bpp as u64))
        .expect("source buffer creation failed");

    let size = src.size() as usize;
    let pattern: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
    unsafe {
        for i in (0..size).step_by(4) {
            std::ptr::copy_nonoverlapping(pattern.as_ptr(), src.mapped_ptr().add(i), 4);
        }
    }

    // Export DMA-BUF fd
    let fd = src.export_dma_buf_fd().expect("DMA-BUF export failed");
    assert!(fd >= 0);

    // Import into a new buffer from the DMA-BUF fd
    let imported =
        HostVulkanBuffer::from_dma_buf_fd(&device, unsafe { OwnedFd::from_raw_fd(fd) }, src.size())
            .expect("DMA-BUF import failed");

    // Verify imported buffer has the same data
    unsafe {
        for i in (0..size).step_by(4) {
            let b = std::ptr::read(imported.mapped_ptr().add(i));
            let g = std::ptr::read(imported.mapped_ptr().add(i + 1));
            let r = std::ptr::read(imported.mapped_ptr().add(i + 2));
            let a = std::ptr::read(imported.mapped_ptr().add(i + 3));
            assert_eq!(
                [b, g, r, a],
                pattern,
                "imported data mismatch at byte offset {i}"
            );
        }
    }

    println!("DMA-BUF round-trip verified: {} bytes, fd={fd}", size);
}

/// Multi-plane `from_dma_buf_fds` round-trip: import two independently
/// allocated + pattern-written DMA-BUFs as the two planes of a single
/// `HostVulkanBuffer`, confirm `plane_count()` reports 2, and each
/// plane's bytes survive intact. Mirrors the symmetry the polyglot
/// Python and Deno shims provide via `*_gpu_surface_plane_{count,size,mmap,base_address}`.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_dma_buf_import_multi_plane_round_trip() {
    let Some(device) =
        a_device_that_exports_dma_buf_or_skip("test_dma_buf_import_multi_plane_round_trip")
    else {
        return;
    };

    let width = 64u32;
    let height = 4u32;
    let bpp = 4u32;
    let plane_size = (width * height * bpp) as vk::DeviceSize;

    // Two independent source buffers, each seeded with a distinct
    // byte pattern so a cross-plane swap would be visible in the
    // readback.
    let src0 = HostVulkanBuffer::new(&device, (width as u64) * (height as u64) * (bpp as u64))
        .expect("source plane 0 creation failed");
    let src1 = HostVulkanBuffer::new(&device, (width as u64) * (height as u64) * (bpp as u64))
        .expect("source plane 1 creation failed");
    let pattern0: [u8; 4] = [0xA0, 0xA1, 0xA2, 0xA3];
    let pattern1: [u8; 4] = [0xB0, 0xB1, 0xB2, 0xB3];
    unsafe {
        for i in (0..plane_size as usize).step_by(4) {
            std::ptr::copy_nonoverlapping(pattern0.as_ptr(), src0.mapped_ptr().add(i), 4);
            std::ptr::copy_nonoverlapping(pattern1.as_ptr(), src1.mapped_ptr().add(i), 4);
        }
    }

    let fd0 = src0.export_dma_buf_fd().expect("plane 0 export failed");
    let fd1 = src1.export_dma_buf_fd().expect("plane 1 export failed");

    // Import both as planes of a single pixel buffer.
    let imported = HostVulkanBuffer::from_dma_buf_fds(
        &device,
        vec![unsafe { OwnedFd::from_raw_fd(fd0) }, unsafe {
            OwnedFd::from_raw_fd(fd1)
        }],
        &[plane_size, plane_size],
    )
    .expect("multi-plane DMA-BUF import failed");

    assert_eq!(imported.plane_count(), 2, "plane_count must report 2");
    assert_eq!(imported.plane_size(0), plane_size);
    assert_eq!(imported.plane_size(1), plane_size);
    assert_eq!(
        imported.plane_size(2),
        0,
        "out-of-range plane size must be 0"
    );

    let p0 = imported.plane_mapped_ptr(0);
    let p1 = imported.plane_mapped_ptr(1);
    assert!(!p0.is_null() && !p1.is_null());
    assert_eq!(
        imported.plane_mapped_ptr(2),
        std::ptr::null_mut(),
        "out-of-range plane ptr must be null"
    );

    // Content check: plane 0 matches pattern0, plane 1 matches
    // pattern1. Byte-exact, no cross-contamination.
    unsafe {
        for i in (0..plane_size as usize).step_by(4) {
            let b0 = [*p0.add(i), *p0.add(i + 1), *p0.add(i + 2), *p0.add(i + 3)];
            let b1 = [*p1.add(i), *p1.add(i + 1), *p1.add(i + 2), *p1.add(i + 3)];
            assert_eq!(b0, pattern0, "plane 0 mismatch at offset {i}");
            assert_eq!(b1, pattern1, "plane 1 mismatch at offset {i}");
        }
    }

    println!(
        "Multi-plane DMA-BUF import round-trip verified: 2 planes × {} bytes",
        plane_size
    );
}

/// Oversize vec rejection: we refuse to import a pixel buffer with
/// more planes than the surface-share `MAX_DMA_BUF_PLANES` cap (4 today).
/// Covers the Rust half of the consistency the wire helpers already
/// enforce on sends/receives.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_dma_buf_import_rejects_oversize_plane_vec() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    // fds/sizes vecs with one more entry than the cap. Real pipe ends
    // stand in for the planes: the refusal owns and closes them, and
    // a fake number would make that close land on someone else's fd.
    let fds: Vec<OwnedFd> = (0..=streamlib_surface_client::MAX_DMA_BUF_PLANES)
        .map(|_| a_pipe_read_end())
        .collect();
    let sizes: Vec<vk::DeviceSize> = vec![1024; fds.len()];

    let result = HostVulkanBuffer::from_dma_buf_fds(&device, fds, &sizes);
    match result {
        Ok(_) => panic!("oversize plane vec must be rejected"),
        Err(e) => assert!(
            e.to_string().contains("MAX_DMA_BUF_PLANES"),
            "error should name the cap, got: {e}"
        ),
    }
}

/// `from_dma_buf_fd_as_storage_buffer` imports a DMA-BUF fd as a
/// STORAGE_BUFFER-usage VkBuffer with mapped memory. Round-trip:
/// allocate a source HOST_VISIBLE SSBO, export its DMA-BUF fd,
/// import via `from_dma_buf_fd_as_storage_buffer`, verify the
/// imported mapping carries the same bytes the source wrote.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn storage_buffer_from_dma_buf_fd_round_trip() {
    let Some(device) =
        a_device_that_exports_dma_buf_or_skip("storage_buffer_from_dma_buf_fd_round_trip")
    else {
        return;
    };

    let byte_size: u64 = 8192;
    let src = HostVulkanBuffer::new_storage_buffer_host_visible(&device, byte_size)
        .expect("source SSBO allocation failed");

    let pattern: [u8; 4] = [0x55, 0xAA, 0xCC, 0x33];
    let size_usize = byte_size as usize;
    unsafe {
        for i in (0..size_usize).step_by(4) {
            std::ptr::copy_nonoverlapping(pattern.as_ptr(), src.mapped_ptr().add(i), 4);
        }
    }

    let fd = src.export_dma_buf_fd().expect("DMA-BUF export failed");
    assert!(fd >= 0);

    let imported = HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(
        &device,
        unsafe { OwnedFd::from_raw_fd(fd) },
        byte_size,
    )
    .expect("DMA-BUF SSBO import failed");

    assert_eq!(imported.size(), byte_size as vk::DeviceSize);
    assert!(!imported.mapped_ptr().is_null());

    unsafe {
        for i in (0..size_usize).step_by(4) {
            let b = std::ptr::read(imported.mapped_ptr().add(i));
            let g = std::ptr::read(imported.mapped_ptr().add(i + 1));
            let r = std::ptr::read(imported.mapped_ptr().add(i + 2));
            let a = std::ptr::read(imported.mapped_ptr().add(i + 3));
            assert_eq!([b, g, r, a], pattern, "imported mismatch at offset {i}");
        }
    }

    println!(
        "DMA-BUF SSBO round-trip verified: {} bytes, fd={fd}",
        byte_size
    );
}

/// `from_dma_buf_fd_as_storage_buffer` returns a typed error for
/// invalid sizes (zero, > u32::MAX) before touching the fd.
#[test]
fn storage_buffer_from_dma_buf_fd_rejects_invalid_sizes() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    // Size validation runs first, so the fd is only ever closed here.
    match HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(&device, a_pipe_read_end(), 0) {
        Err(Error::Configuration(msg)) => {
            assert!(msg.contains("size must be > 0"), "got: {msg}");
        }
        Err(other) => panic!("expected Configuration error for zero size, got {other:?}"),
        Ok(_) => panic!("expected zero-size rejection, got Ok"),
    }

    let oversized = (u32::MAX as u64) + 1;
    match HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(&device, a_pipe_read_end(), oversized)
    {
        Err(Error::Configuration(msg)) => {
            assert!(
                msg.contains("exceeds 4 GB synthetic-width cap"),
                "got: {msg}"
            );
        }
        Err(other) => panic!("expected Configuration error for oversized, got {other:?}"),
        Ok(_) => panic!("expected oversized rejection, got Ok"),
    }
}

/// `from_dma_buf_fd_as_storage_buffer` cleans up on import failure —
/// an undersized DMA-BUF import (small fd, larger requested size)
/// must return `Err` and not leak `VkBuffer` / `VkDeviceMemory`.
///
/// Choice of failure mode: undersized fd is the only failure path
/// every modern Linux Vulkan driver (NVIDIA proprietary, Mesa
/// iris/radeonsi) is empirically known to reject — closed fds, `-1`,
/// and `/dev/null` fds are tolerated by NVIDIA proprietary at
/// `vkAllocateMemory` time despite the spec requiring rejection
/// (driver returns success with phantom memory). Asking the driver
/// to import a 4 KB-backed DMA-BUF as 16 MB hits the kernel's
/// `dma_buf_attach`-time size check, which NVIDIA does honor.
///
/// Source DMA-BUF is itself allocated through the engine so we
/// don't depend on external producers. Import counter is verified
/// to stay flat across the failed call (allocator + cleanup paths
/// must be balanced).
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn storage_buffer_from_dma_buf_fd_drops_on_failure() {
    let Some(device) =
        a_device_that_exports_dma_buf_or_skip("storage_buffer_from_dma_buf_fd_drops_on_failure")
    else {
        return;
    };

    // Allocate a small source DMA-BUF (4 KB). Its kernel-side
    // backing is one page; asking to import it at 16 MB must
    // be rejected per spec (the dma_buf cannot back the
    // requested size).
    let source_size: u64 = 4096;
    let source = HostVulkanBuffer::new_storage_buffer_host_visible(&device, source_size)
        .expect("source SSBO allocation failed");
    let fd = source.export_dma_buf_fd().expect("DMA-BUF export failed");

    let oversized: u64 = 16 * 1024 * 1024;
    let before = device.live_import_allocation_count();
    let result = HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(
        &device,
        unsafe { OwnedFd::from_raw_fd(fd) },
        oversized,
    );
    let after = device.live_import_allocation_count();

    // Strict: the driver MUST reject. If it accepts, either the
    // driver is silently allocating ordinary memory (NVIDIA-style
    // tolerance generalizing beyond closed fds), or our test
    // setup is wrong. Either way the leak invariant isn't being
    // exercised and the test should fail rather than silently no-op.
    let err = match result {
        Ok(_) => {
            panic!(
                "expected DMA-BUF import to reject oversized request \
                 (source={source_size} bytes, requested={oversized} bytes) — \
                 driver accepted; leak invariant unverified"
            );
        }
        Err(e) => e,
    };
    let _ = err;

    assert_eq!(
        before, after,
        "failed import must not leak VkDeviceMemory — live count: before={before}, after={after}"
    );
}
