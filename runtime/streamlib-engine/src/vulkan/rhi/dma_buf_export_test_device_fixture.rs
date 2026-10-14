// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The device a hardware-gated RHI test runs its DMA-BUF arm, or its
//! DMA-BUF-absent arm, on — each skipping, logged and naming the device,
//! where the device in hand cannot run that arm.

#![cfg(all(test, target_os = "linux"))]

use std::sync::Arc;

use super::HostVulkanDevice;

/// The device to run a DMA-BUF arm on, or `None` — logged, naming the
/// device — where there is no device or it exports no DMA-BUF.
pub(crate) fn a_device_that_exports_dma_buf_or_skip(
    test_name: &str,
) -> Option<Arc<HostVulkanDevice>> {
    let device = a_vulkan_device_or_skip(test_name)?;
    if !device.supports_dma_buf_export() {
        tracing::warn!(
            test_name,
            device = %device.name(),
            "the device exports no DMA-BUF, so this DMA-BUF test skips"
        );
        return None;
    }
    Some(device)
}

/// The device to run a DMA-BUF-absent arm on, or `None` — logged, naming
/// the device — where there is no device or it exports DMA-BUF.
pub(crate) fn a_device_that_exports_no_dma_buf_or_skip(
    test_name: &str,
) -> Option<Arc<HostVulkanDevice>> {
    let device = a_vulkan_device_or_skip(test_name)?;
    if device.supports_dma_buf_export() {
        tracing::warn!(
            test_name,
            device = %device.name(),
            "the device exports DMA-BUF, so this DMA-BUF-absent arm skips; pin \
             VK_DRIVER_FILES to lavapipe to run it"
        );
        return None;
    }
    Some(device)
}

fn a_vulkan_device_or_skip(test_name: &str) -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(device) => Some(device),
        Err(no_device) => {
            tracing::warn!(
                test_name,
                "no Vulkan device, so the test skips: {no_device}"
            );
            None
        }
    }
}
