// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Creating host-visible and storage buffers, writing and reading them back, and dropping them.

use super::*;

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_pool_buffer_creation_1920x1080_bgra32() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    const W: u64 = 1920;
    const H: u64 = 1080;
    const BPP: u64 = 4;
    let size = W * H * BPP;
    let buf = HostVulkanBuffer::new(&device, size).expect("buffer creation failed");

    assert_eq!(buf.size(), size);
    assert!(!buf.mapped_ptr().is_null());
    assert_ne!(buf.buffer(), vk::Buffer::null());

    println!("Pool buffer created: {} bytes ({W}x{H}x{BPP})", buf.size());
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_buffer_write_and_readback() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let buf = HostVulkanBuffer::new(&device, 64 * 64 * 4).expect("buffer creation failed");

    let size = buf.size() as usize;
    let ptr = buf.mapped_ptr();

    // Write a repeating BGRA pattern
    let pattern: [u8; 4] = [0xAA, 0xBB, 0xCC, 0xDD];
    unsafe {
        for i in (0..size).step_by(4) {
            std::ptr::copy_nonoverlapping(pattern.as_ptr(), ptr.add(i), 4);
        }
    }

    // Read back and verify
    unsafe {
        for i in (0..size).step_by(4) {
            let b = std::ptr::read(ptr.add(i));
            let g = std::ptr::read(ptr.add(i + 1));
            let r = std::ptr::read(ptr.add(i + 2));
            let a = std::ptr::read(ptr.add(i + 3));
            assert_eq!([b, g, r, a], pattern, "mismatch at byte offset {i}");
        }
    }

    println!("Write/readback verified for {} bytes", size);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_multiple_buffers_coexist() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let b0 = HostVulkanBuffer::new(&device, 1920_u64 * 1080_u64 * 4_u64).expect("buffer 0 failed");
    let b1 = HostVulkanBuffer::new(&device, 1920_u64 * 1080_u64 * 4_u64).expect("buffer 1 failed");
    let b2 = HostVulkanBuffer::new(&device, 1920_u64 * 1080_u64 * 4_u64).expect("buffer 2 failed");
    let b3 = HostVulkanBuffer::new(&device, 1920_u64 * 1080_u64 * 4_u64).expect("buffer 3 failed");

    assert_ne!(b0.buffer(), vk::Buffer::null());
    assert_ne!(b1.buffer(), vk::Buffer::null());
    assert_ne!(b2.buffer(), vk::Buffer::null());
    assert_ne!(b3.buffer(), vk::Buffer::null());

    println!("4 buffers coexist");

    drop(b0);
    drop(b1);
    drop(b2);
    drop(b3);

    println!("All dropped successfully");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_drop_frees_without_panic() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let buf = HostVulkanBuffer::new(&device, 1920_u64 * 1080_u64 * 4_u64)
        .expect("buffer creation failed");
    drop(buf);

    println!("Buffer drop completed without panic");
}

/// `new_storage_buffer_host_visible` allocates a HOST_VISIBLE
/// STORAGE_BUFFER-usage VkBuffer: mapped pointer is non-null, size
/// matches the requested byte count, write→readback round-trips
/// through the persistent mapping.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn storage_buffer_host_visible_write_readback() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    let byte_size: u64 = 4096;
    let buf = HostVulkanBuffer::new_storage_buffer_host_visible(&device, byte_size)
        .expect("storage buffer allocation failed");

    assert_eq!(buf.size(), byte_size as vk::DeviceSize);
    assert!(
        !buf.mapped_ptr().is_null(),
        "mapped pointer must be non-null"
    );
    assert_ne!(buf.buffer(), vk::Buffer::null());

    // Write a counter pattern through the mapped pointer and read it back.
    let ptr = buf.mapped_ptr();
    unsafe {
        for i in 0..byte_size as usize {
            std::ptr::write(ptr.add(i), (i & 0xFF) as u8);
        }
        for i in 0..byte_size as usize {
            let v = std::ptr::read(ptr.add(i));
            assert_eq!(v, (i & 0xFF) as u8, "mismatch at offset {i}");
        }
    }

    println!("storage buffer round-trip verified: {} bytes", byte_size);
}

/// `new_storage_buffer_host_visible` rejects byte_size = 0 and
/// byte_size > u32::MAX with a `Configuration` error. No device touch
/// for the validation paths, so this runs without hardware.
#[test]
fn storage_buffer_host_visible_rejects_invalid_sizes() {
    // Validation errors fire before any device interaction. Build a
    // device lazily — if it fails (no GPU), skip; the validation
    // path itself doesn't depend on the device but the function
    // signature requires one.
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return;
        }
    };

    match HostVulkanBuffer::new_storage_buffer_host_visible(&device, 0) {
        Err(Error::Configuration(msg)) => {
            assert!(msg.contains("byte_size must be > 0"), "got: {msg}");
        }
        Err(other) => panic!("expected Configuration error for zero size, got {other:?}"),
        Ok(_) => panic!("expected zero-size rejection, got Ok"),
    }

    let oversized = (u32::MAX as u64) + 1;
    match HostVulkanBuffer::new_storage_buffer_host_visible(&device, oversized) {
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
