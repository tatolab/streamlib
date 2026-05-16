// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Render Hardware Interface (RHI) - Platform-agnostic GPU abstraction.

mod backend;
pub mod blitter;
mod color_converter;
mod command_buffer;
mod command_queue;
mod compute_kernel;
mod device;
mod external_handle;
mod gl_interop;
mod graphics_kernel;
mod index_buffer;
mod pixel_buffer;
mod pixel_buffer_pool;
mod pixel_buffer_ref;
mod ray_tracing_kernel;
mod storage_buffer;
mod texture;
mod uniform_buffer;
mod vertex_buffer;
mod texture_cache;
mod texture_readback;

pub use backend::RhiBackend;
pub use blitter::RhiBlitter;
pub use command_buffer::CommandBuffer;
pub use command_queue::RhiCommandQueue;
pub use color_converter::{
    pixel_format_color_kind, ColorConverterPushConstants, RhiColorConverter, SourceLayoutInfo,
    COLOR_CONVERTER_PUSH_CONSTANT_SIZE,
};
pub use compute_kernel::{
    derive_bindings_from_spirv, ComputeBindingKind, ComputeBindingSpec,
    ComputeKernelDescriptor,
};
pub use device::GpuDevice;
pub use graphics_kernel::{
    derive_bindings_from_spirv_multistage, AttachmentFormats, BlendFactor, BlendOp,
    ColorBlendAttachment, ColorBlendState, ColorWriteMask, CullMode, DepthCompareOp, DepthFormat,
    DepthStencilState, DrawCall, DrawIndexedCall, FrontFace, GraphicsBindingKind,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStage, GraphicsShaderStageFlags, GraphicsStage,
    IndexType, MultisampleState, PolygonMode, PrimitiveTopology, RasterizationState, ScissorRect,
    VertexAttributeFormat, VertexInputAttribute, VertexInputBinding, VertexInputRate,
    VertexInputState, Viewport,
};
pub use external_handle::{RhiExternalHandle, RhiPixelBufferExport, RhiPixelBufferImport};
pub use ray_tracing_kernel::{
    validate_shader_groups, RayTracingBindingKind, RayTracingBindingSpec,
    RayTracingKernelDescriptor, RayTracingPushConstants, RayTracingShaderGroup,
    RayTracingShaderStage, RayTracingShaderStageFlags, RayTracingStage,
};
pub use gl_interop::{gl_constants, GlContext, GlTextureBinding};
pub use pixel_buffer::PixelBuffer;
pub use pixel_buffer_pool::{PixelBufferDescriptor, PixelBufferPoolId};
#[cfg(target_os = "linux")]
pub use storage_buffer::StorageBuffer;
#[cfg(target_os = "linux")]
pub use uniform_buffer::UniformBuffer;
#[cfg(target_os = "linux")]
pub use vertex_buffer::VertexBuffer;
#[cfg(target_os = "linux")]
pub use index_buffer::IndexBuffer;
// Note: RhiPixelBufferPool is intentionally not exported - use GpuContext::acquire_pixel_buffer()
pub(crate) use pixel_buffer_pool::RhiPixelBufferPool;
pub use pixel_buffer_ref::PixelBufferRef;
// PixelFormat / TextureFormat / TextureUsages / VulkanLayout are
// defined in the `streamlib-consumer-rhi` crate so subprocess-shape dep
// graphs can reach them without pulling streamlib. Re-exported here for
// in-tree call sites (`crate::core::rhi::PixelFormat` keeps working).
//
// `VulkanLayout` is the typed `VkImageLayout` value used by
// `TextureRegistration` and surface-share's cross-process layout
// coordination (#633). In-tree consumers reach it via this re-export
// rather than depending on `streamlib-consumer-rhi` directly.
pub use streamlib_consumer_rhi::{PixelFormat, TextureFormat, TextureUsages, VulkanLayout};
pub use texture::{NativeTextureHandle, Texture, TextureDescriptor};
pub use texture_cache::{RhiTextureCache, RhiTextureView};
pub use texture_readback::{
    ReadbackTicket, TextureReadbackDescriptor, TextureReadbackError, TextureSourceLayout,
};
