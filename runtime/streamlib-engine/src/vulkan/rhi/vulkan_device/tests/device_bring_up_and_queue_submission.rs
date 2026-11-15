// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_device_creation() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };

    assert!(!device.name().is_empty(), "Device name should not be empty");
    println!("Vulkan device created: {}", device.name());
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_queue_family_discovery() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };

    println!(
        "Graphics queue family: {}, transfer: {}, compute: {:?}",
        device.queue_family_index(),
        device.transfer_queue_family_index(),
        device.compute_queue_family_index(),
    );
}

/// Issue #638 — `supports_cross_device_dma_buf_probe()` must
/// return `false` on NVIDIA and `true` elsewhere. Locks the
/// vendor-id check; mentally-revertible (delete the
/// `vendor_id != 0x10DE` check in `new()` and this test fails on
/// NVIDIA hardware). Skips when no Vulkan device is available.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn supports_cross_device_dma_buf_probe_matches_vendor_blocklist() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };

    let name = device.name();
    let is_nvidia = name.contains("NVIDIA") || name.contains("nvidia");
    let supports = device.supports_cross_device_dma_buf_probe();

    if is_nvidia {
        assert!(
            !supports,
            "expected supports_cross_device_dma_buf_probe()=false on NVIDIA ({name}), got true"
        );
    } else {
        assert!(
            supports,
            "expected supports_cross_device_dma_buf_probe()=true on non-NVIDIA ({name}), got false"
        );
    }
}

/// Locks the canonical inherent `HostVulkanDevice::submit_to_queue`
/// signature the codec layer reaches directly. If a future refactor
/// reintroduces a wrapper trait method returning a different
/// `Result` type, this signature binding fails to compile.
#[test]
fn submit_to_queue_inherent_signature_is_streamlib_result() {
    let _f: unsafe fn(&HostVulkanDevice, vk::Queue, &[vk::SubmitInfo2], vk::Fence) -> Result<()> =
        HostVulkanDevice::submit_to_queue;
}

/// Issue #633 — `supports_qfot_acquire_unmodified` must equal the
/// `has_acquire_unmodified` extension probe from `new()`.
/// Trait-method route must agree with the inherent method.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn supports_qfot_acquire_unmodified_consistent_across_inherent_and_trait() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };
    let inherent = HostVulkanDevice::supports_qfot_acquire_unmodified(&device);
    let via_trait =
        <HostVulkanDevice as streamlib_consumer_rhi::VulkanRhiDevice>::supports_qfot_acquire_unmodified(&device);
    assert_eq!(inherent, via_trait, "trait and inherent reports must agree");
    assert_eq!(
        inherent, device.has_acquire_unmodified,
        "supports_qfot_acquire_unmodified must equal has_acquire_unmodified \
         (VK_QUEUE_FAMILY_EXTERNAL is core 1.1 and always available)"
    );
}

/// Issue #633 — QFOT acquire is a no-op when target is UNDEFINED.
/// **Honest scope**: this asserts only that the call returns
/// `Ok`, not that no GPU work was issued. Reverting the early
/// return makes the helper attempt a barrier with
/// `image = vk::Image::null()`; validation layers reject that
/// (`VUID-VkImageMemoryBarrier2-image-parameter`), but raw
/// drivers may tolerate it silently. Real GPU-correctness for
/// QFOT comes from E2E scenarios run with
/// `VK_LOADER_LAYERS_ENABLE=*validation*`.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn host_acquire_from_foreign_undefined_target_is_noop() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };
    let result = device.acquire_from_foreign(vk::Image::null(), vk::ImageLayout::UNDEFINED);
    assert!(
        result.is_ok(),
        "acquire_from_foreign with target=UNDEFINED must short-circuit Ok"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn test_vma_allocator_created() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };

    // Verify VMA allocator is accessible
    let _ = device.allocator();
    println!("VMA allocator created successfully");
}
