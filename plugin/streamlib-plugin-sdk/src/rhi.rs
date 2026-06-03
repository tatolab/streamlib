// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm RHI surface — the GPU resource VIEW types a
//! Vulkan-compute plugin holds.
//!
//! Engine-free twins of the engine's `core::rhi` / `vulkan::rhi`
//! PluginAbiObject views: every type is `#[repr(C)]` and layout-matched
//! to the engine's, with Clone/Drop/method dispatch routed through the
//! host-installed vtables. The host `*Inner` backings + the
//! raw-`HostVulkanDevice` constructors stay in the engine.
//!
//! `TextureFormat` / `TextureUsages` / `PixelFormat` / `VulkanLayout` are
//! already engine-free in [`streamlib_consumer_rhi`]; they're re-exported
//! here so a plugin reaches the whole RHI surface from one module.

mod color_converter;
mod command_recorder;
mod compute_kernel_descriptor;
mod pipeline_flags;
mod storage_buffer;
mod texture;
mod texture_ring;
mod vulkan_compute_kernel;

pub use color_converter::{
    pixel_format_color_kind, ColorConverterPushConstants, RhiColorConverter, SourceLayoutInfo,
    COLOR_CONVERTER_PUSH_CONSTANT_SIZE,
};
pub use command_recorder::{ImageCopyRegion, RhiCommandRecorder};
pub use compute_kernel_descriptor::{
    ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor,
};
pub use pipeline_flags::{VulkanAccess, VulkanStage};
pub use storage_buffer::StorageBuffer;
pub use texture::{NativeTextureHandle, Texture, TextureDescriptor};
pub use texture_ring::{
    TextureRing, TextureRingSlot, TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES,
};
pub use vulkan_compute_kernel::VulkanComputeKernel;

// Format / layout primitives — already engine-free in consumer-rhi.
pub use streamlib_consumer_rhi::{PixelFormat, TextureFormat, TextureUsages, VulkanLayout};

// Internal staging helper for `create_compute_kernel`.
pub(crate) use compute_kernel_descriptor::stage_compute_kernel_descriptor;

// =============================================================================
// Cdylib vtable resolver
// =============================================================================

/// Cdylib-arm resolver for the host's [`GpuContextLimitedAccessVTable`].
/// Returns the host-installed pointer cached on
/// [`crate::plugin::HostCallbacks`], or null when no callbacks are
/// installed / the field is null. (No host static exists in the
/// engine-free SDK — the host arm is the engine's.)
///
/// Used by [`Texture::from_raw_handle_for_cdylib`] to pair a host-cloned
/// texture handle with the same limited-access vtable the host's
/// `clone_texture` slot was minted against, so `Texture::Drop` reaches
/// the matching `drop_texture`.
pub(crate) fn host_gpu_context_limited_access_vtable(
) -> *const streamlib_plugin_abi::GpuContextLimitedAccessVTable {
    crate::plugin::host_callbacks()
        .map(|c| c.gpu_context_limited_access_vtable)
        .filter(|p| !p.is_null())
        .unwrap_or(std::ptr::null())
}
