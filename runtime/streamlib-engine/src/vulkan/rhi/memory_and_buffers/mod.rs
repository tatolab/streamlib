// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Buffers, their bindings, host mappings, pixel buffer pools and device memory budgets.

pub(crate) mod vulkan_buffer;
pub(super) mod vulkan_buffer_binding;
pub(super) mod vulkan_frame_allocation_device_memory_budget;
#[cfg(target_os = "linux")]
pub(super) mod vulkan_host_mapping_read_by_gpu;
#[cfg(target_os = "macos")]
pub(super) mod vulkan_imported_iosurface_storage_buffer;
#[cfg(target_os = "macos")]
pub(super) mod vulkan_metal_working_set_budget;
pub(super) mod vulkan_pixel_buffer_pool;
pub(super) mod vulkan_storage_binding;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod vulkan_upload_resources;
