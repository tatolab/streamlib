// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Render Hardware Interface (RHI) - Platform-agnostic GPU abstraction.

mod backend;
mod command_buffer;
mod command_queue;
mod device;
mod external_handle;
mod format_converter;
mod format_converter_cache;
mod gl_interop;
mod pixel_buffer;
mod pixel_buffer_pool;
mod pixel_buffer_ref;
mod pixel_format;
mod texture;
mod texture_cache;

pub use backend::RhiBackend;
pub use command_buffer::CommandBuffer;
pub use command_queue::RhiCommandQueue;
pub use device::GpuDevice;
pub use external_handle::{RhiExternalHandle, RhiPixelBufferExport, RhiPixelBufferImport};
pub use format_converter::RhiFormatConverter;
pub use format_converter_cache::RhiFormatConverterCache;
pub use gl_interop::{gl_constants, GlContext, GlTextureBinding};
pub use pixel_buffer::RhiPixelBuffer;
pub use pixel_buffer_pool::{PixelBufferDescriptor, PixelBufferPoolId};
// Note: RhiPixelBufferPool is intentionally not exported - use GpuContext::acquire_pixel_buffer()
pub(crate) use pixel_buffer_pool::RhiPixelBufferPool;
pub use pixel_buffer_ref::RhiPixelBufferRef;
pub use pixel_format::PixelFormat;
pub use texture::{
    NativeTextureHandle, StreamTexture, TextureDescriptor, TextureFormat, TextureUsages,
};
pub use texture_cache::{RhiTextureCache, RhiTextureView};
