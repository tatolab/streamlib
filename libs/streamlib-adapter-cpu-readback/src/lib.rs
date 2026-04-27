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
//! Implementation rides on `VulkanPixelBuffer` (a HOST_VISIBLE,
//! HOST_COHERENT linear `VkBuffer`); each acquire issues a
//! `vkCmdCopyImageToBuffer` from the host's `VkImage` into that staging
//! buffer, blocks until the copy is observable on the host, and hands
//! the customer a `&[u8]` view over the mapped bytes. On WRITE release,
//! the staging bytes are flushed back via `vkCmdCopyBufferToImage`
//! before the timeline release-value is signaled.
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
pub use view::{CpuReadbackReadView, CpuReadbackWriteView};
