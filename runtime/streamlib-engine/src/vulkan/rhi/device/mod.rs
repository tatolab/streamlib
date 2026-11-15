// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The instance, the device and what is probed or validated on it.

#[cfg(all(test, target_os = "linux"))]
pub(super) mod dma_buf_export_test_device_fixture;
#[cfg(target_os = "linux")]
pub mod drm_modifier_probe;
pub(super) mod host_marker;
pub(super) mod vulkan_device;
pub(super) mod vulkan_device_selection;
pub(super) mod vulkan_instance;
pub(super) mod vulkan_validation_messenger;
