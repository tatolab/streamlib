// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Surface-adapter contract shared by every adapter core.

mod adapter;
mod conformance;
mod error;
mod guard;
mod mock;
mod registry;
mod surface;

#[cfg(target_os = "linux")]
mod subprocess_crash;

pub mod testing;

pub use adapter::{
    CpuReadable, CpuWritable, GlWritable, SurfaceAdapter, VkImageHandle, VkImageInfo,
    VkImageLayoutValue, VulkanImageInfoExt, VulkanWritable,
};
pub use error::AdapterError;
pub use guard::{ReadGuard, WriteGuard};
pub use registry::{Registry, SurfaceRegistration};
pub use surface::{
    AccessMode, StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
