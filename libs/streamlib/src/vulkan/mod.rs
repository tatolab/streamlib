// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan GPU backend (Linux, macOS via MoltenVK).

pub mod rhi;

// Re-exports for public API (intentionally exposed for external use)
#[allow(unused_imports)]
pub use rhi::{VulkanCommandBuffer, VulkanCommandQueue, VulkanDevice, VulkanTexture};
