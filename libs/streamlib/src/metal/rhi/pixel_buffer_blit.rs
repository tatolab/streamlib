// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! GPU-based pixel buffer blitting using Metal.
//!
//! Provides fast GPU copy between CVPixelBuffers/IOSurfaces without touching
//! CPU memory. Used for cross-process sharing when IOSurfaceIsGlobal isn't set.

use crate::core::rhi::RhiPixelBuffer;
use crate::core::{Result, StreamError};
use metal::foreign_types::{ForeignType, ForeignTypeRef};
use metal::Device;
use std::sync::OnceLock;

/// CVPixelFormatType for 32-bit BGRA ('BGRA').
const K_CVPIXELFORMATTYPE_32BGRA: u32 = 0x42475241;

/// Shared Metal device for blit operations.
static BLIT_DEVICE: OnceLock<Device> = OnceLock::new();

fn get_blit_device() -> &'static Device {
    BLIT_DEVICE.get_or_init(|| Device::system_default().expect("No Metal device available"))
}

/// Blit (GPU copy) from source to destination pixel buffer.
///
/// This performs a fast GPU-side copy without touching CPU memory.
/// Both buffers must have the same dimensions and format.
pub fn blit_pixel_buffer(src: &RhiPixelBuffer, dst: &RhiPixelBuffer) -> Result<()> {
    use crate::apple::corevideo_ffi::{
        kCVReturnSuccess, CVMetalTextureCacheCreate, CVMetalTextureCacheCreateTextureFromImage,
        CVMetalTextureCacheRef, CVMetalTextureGetTexture, CVMetalTextureRef,
        CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    };
    use std::ptr;

    let device = get_blit_device();

    // Get dimensions
    let src_cv = src.buffer_ref().cv_pixel_buffer();
    let dst_cv = dst.buffer_ref().cv_pixel_buffer();

    if src_cv.is_null() {
        return Err(StreamError::GpuError("Source CVPixelBuffer is null".into()));
    }
    if dst_cv.is_null() {
        return Err(StreamError::GpuError(
            "Destination CVPixelBuffer is null".into(),
        ));
    }

    let width = unsafe { CVPixelBufferGetWidth(src_cv) } as u64;
    let height = unsafe { CVPixelBufferGetHeight(src_cv) } as u64;

    tracing::trace!(
        "blit_pixel_buffer: src={:?} dst={:?} {}x{}",
        src_cv,
        dst_cv,
        width,
        height
    );

    // Create texture cache
    let mut texture_cache: CVMetalTextureCacheRef = ptr::null_mut();
    let status = unsafe {
        CVMetalTextureCacheCreate(
            ptr::null(),
            ptr::null(),
            device.as_ptr() as *mut _,
            ptr::null(),
            &mut texture_cache,
        )
    };
    if status != kCVReturnSuccess {
        return Err(StreamError::GpuError(format!(
            "Failed to create texture cache: {}",
            status
        )));
    }

    // Create source texture
    let pixel_format = unsafe { CVPixelBufferGetPixelFormatType(src_cv) };
    let metal_format = if pixel_format == K_CVPIXELFORMATTYPE_32BGRA {
        metal::MTLPixelFormat::BGRA8Unorm
    } else {
        metal::MTLPixelFormat::RGBA8Unorm
    };

    let mut src_texture: CVMetalTextureRef = ptr::null_mut();
    let status = unsafe {
        CVMetalTextureCacheCreateTextureFromImage(
            ptr::null(),
            texture_cache,
            src_cv,
            ptr::null(),
            metal_format as u64,
            width as usize,
            height as usize,
            0,
            &mut src_texture,
        )
    };
    if status != kCVReturnSuccess {
        unsafe {
            crate::apple::corevideo_ffi::CFRelease(texture_cache as *const _);
        }
        return Err(StreamError::GpuError(format!(
            "Failed to create source texture: {}",
            status
        )));
    }

    // Create destination texture
    let mut dst_texture: CVMetalTextureRef = ptr::null_mut();
    let status = unsafe {
        CVMetalTextureCacheCreateTextureFromImage(
            ptr::null(),
            texture_cache,
            dst_cv,
            ptr::null(),
            metal_format as u64,
            width as usize,
            height as usize,
            0,
            &mut dst_texture,
        )
    };
    if status != kCVReturnSuccess {
        unsafe {
            crate::apple::corevideo_ffi::CFRelease(src_texture as *const _);
            crate::apple::corevideo_ffi::CFRelease(texture_cache as *const _);
        }
        return Err(StreamError::GpuError(format!(
            "Failed to create destination texture: {}",
            status
        )));
    }

    // Get Metal texture references
    let src_mtl = unsafe { CVMetalTextureGetTexture(src_texture) };
    let dst_mtl = unsafe { CVMetalTextureGetTexture(dst_texture) };

    if src_mtl.is_null() {
        unsafe {
            crate::apple::corevideo_ffi::CFRelease(src_texture as *const _);
            crate::apple::corevideo_ffi::CFRelease(dst_texture as *const _);
            crate::apple::corevideo_ffi::CFRelease(texture_cache as *const _);
        }
        return Err(StreamError::GpuError(
            "CVMetalTextureGetTexture returned null for source".into(),
        ));
    }
    if dst_mtl.is_null() {
        unsafe {
            crate::apple::corevideo_ffi::CFRelease(src_texture as *const _);
            crate::apple::corevideo_ffi::CFRelease(dst_texture as *const _);
            crate::apple::corevideo_ffi::CFRelease(texture_cache as *const _);
        }
        return Err(StreamError::GpuError(
            "CVMetalTextureGetTexture returned null for destination".into(),
        ));
    }

    // Convert to metal crate types - safe because we checked for null above
    let src_metal = unsafe { metal::TextureRef::from_ptr(src_mtl as *mut _) };
    let dst_metal = unsafe { metal::TextureRef::from_ptr(dst_mtl as *mut _) };

    // Perform blit - use try_ variants where available
    let command_queue = device.new_command_queue();
    let command_buffer = command_queue.new_command_buffer();
    let blit_encoder = command_buffer.new_blit_command_encoder();

    let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
    let size = metal::MTLSize {
        width,
        height,
        depth: 1,
    };

    blit_encoder.copy_from_texture(
        src_metal, 0, // source slice
        0, // source level
        origin, size, dst_metal, 0, // dest slice
        0, // dest level
        origin,
    );

    blit_encoder.end_encoding();
    command_buffer.commit();
    command_buffer.wait_until_completed();

    // Release textures and cache (they're CFRetained)
    unsafe {
        crate::apple::corevideo_ffi::CFRelease(src_texture as *const _);
        crate::apple::corevideo_ffi::CFRelease(dst_texture as *const _);
        crate::apple::corevideo_ffi::CFRelease(texture_cache as *const _);
    }

    tracing::trace!("blit_pixel_buffer: completed successfully");
    Ok(())
}
