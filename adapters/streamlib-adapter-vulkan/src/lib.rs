// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Vulkan-native surface adapter — no-copy passthrough that hands a
//! host-allocated `VkImage` to any consumer that speaks Vulkan.
//!
//! This is the canonical implementor of the `VulkanWritable` and
//! `VulkanImageInfoExt` capability traits from `streamlib-surface-adapter`.
//! Cross-API adapters (`streamlib-adapter-opengl`, `streamlib-adapter-skia`)
//! compose on top of these views without ever seeing DMA-BUF fds, DRM
//! modifiers, or timeline-semaphore handles directly.

#![cfg(any(target_os = "linux", target_os = "macos"))]

mod adapter;
mod context;
mod raw_handles;
mod state;
mod view;

pub use adapter::VulkanSurfaceAdapter;
pub use context::VulkanContext;
pub use raw_handles::{RawVulkanHandles, raw_handles};
pub use state::HostSurfaceRegistration;
pub use streamlib_consumer_rhi::VulkanLayout;
pub use view::{VulkanReadView, VulkanWriteView};
