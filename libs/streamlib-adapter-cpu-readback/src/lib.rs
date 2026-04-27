// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Explicit GPU→CPU surface adapter — the single named opt-in path for
//! customers who need CPU memory access to a streamlib surface.
//!
//! This crate is the canonical implementor of the
//! [`streamlib_adapter_abi::CpuReadable`] /
//! [`streamlib_adapter_abi::CpuWritable`] capability marker traits.
//! GPU adapters (`streamlib-adapter-vulkan`, `-opengl`, `-skia`)
//! deliberately do not implement these — that asymmetry is the
//! architectural enforcement of "switch adapter to opt into CPU".
//!
//! Implementation rides on one [`streamlib::adapter_support::VulkanPixelBuffer`]
//! (a HOST_VISIBLE, HOST_COHERENT linear `VkBuffer`) **per plane**.
//! Single-plane formats (BGRA8/RGBA8) allocate one staging buffer;
//! multi-plane formats (NV12) allocate one per logical plane (Y + UV).
//! Each acquire issues a per-plane `vkCmdCopyImageToBuffer` from the
//! host's `VkImage` (with the matching `VK_IMAGE_ASPECT_*_BIT` aspect)
//! into the corresponding staging buffer, blocks until the copies are
//! observable on the host, and hands the customer per-plane `&[u8]`
//! views over the mapped bytes. On WRITE release, every plane's staging
//! bytes are flushed back via per-plane `vkCmdCopyBufferToImage` before
//! the timeline release-value is signaled.
//!
//! See `docs/architecture/surface-adapter.md` for the architecture
//! brief.

#![cfg(target_os = "linux")]

mod adapter;
mod context;
mod state;
mod view;

pub use adapter::CpuReadbackSurfaceAdapter;
pub use context::CpuReadbackContext;
pub use state::HostSurfaceRegistration;
pub use view::{
    CpuReadbackPlaneView, CpuReadbackPlaneViewMut, CpuReadbackReadView, CpuReadbackWriteView,
};
