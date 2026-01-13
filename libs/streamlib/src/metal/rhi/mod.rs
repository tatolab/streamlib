// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal RHI implementation.

use std::sync::Mutex;

pub mod format_converter;
pub mod gl_interop;
mod metal_command_buffer;
mod metal_command_queue;
mod metal_device;
mod metal_texture;
pub mod pixel_buffer_pool;
pub mod pixel_buffer_ref;
mod pixel_format;
pub mod texture_cache;

pub use metal_command_buffer::MetalCommandBuffer;
pub use metal_command_queue::MetalCommandQueue;
pub use metal_device::MetalDevice;
pub use metal_texture::MetalTexture;

/// Global lock for CoreVideo initialization operations.
///
/// CoreVideo's internal `_pixelFormatDictionaryInit` is not thread-safe and can crash
/// when multiple threads call CoreVideo init functions concurrently (e.g.,
/// `CVPixelBufferPoolCreate`, `CVMetalTextureCacheCreate`, `CVOpenGLTextureCacheCreate`)
/// or when racing with AVFoundation's `AVCaptureDeviceInput` initialization.
///
/// This lock serializes all such operations to prevent the race condition.
pub(crate) static COREVIDEO_INIT_LOCK: Mutex<()> = Mutex::new(());
