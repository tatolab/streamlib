// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Compute, graphics and ray tracing kernels, their pipeline cache and acceleration structures.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod vulkan_acceleration_structure;
pub(super) mod vulkan_compute_kernel;
pub(super) mod vulkan_graphics_kernel;
pub(super) mod vulkan_kernel_capability_refusal;
pub(super) mod vulkan_pipeline_cache_on_disk;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod vulkan_ray_tracing_kernel;
