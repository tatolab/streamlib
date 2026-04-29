// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan-native surface adapter — no-copy passthrough that hands a
//! host-allocated `VkImage` to any consumer that speaks Vulkan.
//!
//! This is the canonical implementor of the `VulkanWritable` and
//! `VulkanImageInfoExt` capability traits from `streamlib-adapter-abi`.
//! Cross-API adapters (`streamlib-adapter-opengl`, `streamlib-adapter-skia`)
//! compose on top of these views without ever seeing DMA-BUF fds, DRM
//! modifiers, or timeline-semaphore handles directly.
//!
//! See `docs/architecture/surface-adapter.md` for the architecture brief
//! and `docs/adapter-authoring.md` for the 3rd-party authoring guide.

#![cfg(target_os = "linux")]

mod adapter;
mod context;
mod raw_handles;
mod state;
mod view;

pub use adapter::VulkanSurfaceAdapter;
pub use context::VulkanContext;
pub use raw_handles::{raw_handles, RawVulkanHandles};
pub use state::HostSurfaceRegistration;
pub use streamlib_consumer_rhi::VulkanLayout;
pub use view::{VulkanReadView, VulkanWriteView};
