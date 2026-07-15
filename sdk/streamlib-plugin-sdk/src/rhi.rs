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
mod graphics_kernel_descriptor;
mod host_timeline_semaphore;
mod pipeline_flags;
mod pixel_buffer;
mod pooled_texture_handle;
mod present_target;
mod storage_buffer;
mod surface_store;
mod texture;
mod texture_readback;
mod texture_registration;
mod texture_ring;
mod vulkan_compute_kernel;
mod vulkan_graphics_kernel;

pub use color_converter::{
    COLOR_CONVERTER_PUSH_CONSTANT_SIZE, ColorConverterPushConstants, RhiColorConverter,
    SourceLayoutInfo, pixel_format_color_kind,
};
pub use command_recorder::{ImageCopyRegion, RhiCommandRecorder};
pub use compute_kernel_descriptor::{
    ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor,
};
pub use graphics_kernel_descriptor::{
    AttachmentFormats, BlendFactor, BlendOp, ColorBlendAttachment, ColorBlendState, ColorWriteMask,
    CullMode, DepthCompareOp, DepthFormat, DepthStencilState, DrawCall, DrawIndexedCall, FrontFace,
    GraphicsBindingKind, GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor,
    GraphicsPipelineState, GraphicsPushConstants, GraphicsShaderStage, GraphicsShaderStageFlags,
    GraphicsStage, IndexType, MultisampleState, OffscreenColorTarget, OffscreenDraw, PolygonMode,
    PrimitiveTopology, RasterizationState, ScissorRect, VertexAttributeFormat,
    VertexInputAttribute, VertexInputBinding, VertexInputRate, VertexInputState, Viewport,
};
pub use host_timeline_semaphore::HostTimelineSemaphore;
pub use pipeline_flags::{VulkanAccess, VulkanStage};
pub use pixel_buffer::{PixelBuffer, PixelBufferPoolId};
pub use pooled_texture_handle::{PooledTextureHandle, TexturePoolDescriptor};
pub use present_target::{PresentTarget, PresentTargetFrame};
pub use storage_buffer::StorageBuffer;
pub use surface_store::SurfaceStore;
pub use texture::{NativeTextureHandle, Texture, TextureDescriptor};
pub use texture_readback::{ReadbackTicket, TextureReadback, TextureSourceLayout};
pub use texture_registration::TextureRegistration;
pub use texture_ring::{TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES, TextureRing, TextureRingSlot};
pub use vulkan_compute_kernel::VulkanComputeKernel;
pub use vulkan_graphics_kernel::VulkanGraphicsKernel;

// Format / layout primitives — already engine-free in consumer-rhi.
pub use streamlib_consumer_rhi::{PixelFormat, TextureFormat, TextureUsages, VulkanLayout};

// OPAQUE_FD/CUDA export descriptor (#1262) — the `#[repr(C)]` POD is
// authored once in `streamlib-plugin-abi`; re-exported here so cdylib
// authors reach it as `sdk::rhi::OpaqueFdExportDescriptorRepr` without a
// twin definition.
pub use streamlib_plugin_abi::OpaqueFdExportDescriptorRepr;

// Internal staging helper for `create_compute_kernel`.
pub(crate) use compute_kernel_descriptor::stage_compute_kernel_descriptor;
// Internal staging helper for `create_graphics_kernel`.
pub(crate) use graphics_kernel_descriptor::stage_graphics_kernel_descriptor;

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
pub(crate) fn host_gpu_context_limited_access_vtable()
-> *const streamlib_plugin_abi::GpuContextLimitedAccessVTable {
    crate::plugin::host_callbacks()
        .map(|c| c.gpu_context_limited_access_vtable)
        .filter(|p| !p.is_null())
        .unwrap_or(std::ptr::null())
}
