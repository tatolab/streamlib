// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public ABI for StreamLib surface adapters.
//!
//! See `docs/architecture/surface-adapter.md` for the architecture
//! brief and `docs/architecture/adapter-authoring.md` for the 3rd-party
//! adapter author guide.

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
    VkImageLayoutValue, VulkanImageInfoExt, VulkanWritable, STREAMLIB_ADAPTER_ABI_VERSION,
};
pub use error::AdapterError;
pub use guard::{ReadGuard, WriteGuard};
pub use registry::{Registry, SurfaceRegistration};
pub use surface::{
    AccessMode, SurfaceFormat, SurfaceId, SurfaceSyncState, SurfaceTransportHandle,
    SurfaceUsage, StreamlibSurface,
};
