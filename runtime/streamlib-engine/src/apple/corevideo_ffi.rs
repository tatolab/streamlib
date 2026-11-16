// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! CoreVideo FFI bindings for texture caches and pixel buffer pools.
//!
//! Provides bindings for:
//! - CVMetalTextureCache: Create Metal textures from CVPixelBuffers
//! - CVOpenGLTextureCache: Create OpenGL textures from CVPixelBuffers
//! - CVPixelBufferPool: Efficient pixel buffer allocation/recycling

#![allow(
    dead_code,
    non_snake_case,
    non_upper_case_globals,
    reason = "CoreVideo FFI bindings keep the C names and declare more than the engine calls"
)]

use std::ffi::c_void;

// Type aliases for CoreVideo opaque types
pub(crate) type CVMetalTextureCacheRef = *mut c_void;
pub(crate) type CVMetalTextureRef = *mut c_void;
pub(crate) type CVOpenGLTextureCacheRef = *mut c_void;
pub(crate) type CVOpenGLTextureRef = *mut c_void;
pub(crate) type CVPixelBufferPoolRef = *mut c_void;
pub(crate) type CVPixelBufferRef = *mut c_void;
pub(crate) type CFDictionaryRef = *const c_void;
pub(crate) type CFStringRef = *const c_void;
pub(crate) type CFNumberRef = *const c_void;

// CGL types for OpenGL context
pub(crate) type CGLContextObj = *mut c_void;
pub(crate) type CGLPixelFormatObj = *mut c_void;

// CVReturn codes
pub(crate) const kCVReturnSuccess: i32 = 0;

// CFNumber types
pub(crate) const K_CFNUMBER_SINT32_TYPE: i32 = 3;

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    // ========================================================================
    // CVMetalTextureCache
    // ========================================================================

    /// Creates a CVMetalTextureCache for creating Metal textures from CVPixelBuffers.
    ///
    /// Parameters:
    /// - allocator: Pass null for default allocator
    /// - cache_attributes: Pass null for default attributes
    /// - metal_device: The Metal device (MTLDevice)
    /// - texture_attributes: Pass null for default attributes
    /// - cache_out: Receives the created cache
    pub(crate) fn CVMetalTextureCacheCreate(
        allocator: *const c_void,
        cache_attributes: *const c_void,
        metal_device: *const c_void,
        texture_attributes: *const c_void,
        cache_out: *mut CVMetalTextureCacheRef,
    ) -> i32;

    /// Creates a Metal texture from a CVPixelBuffer.
    ///
    /// Parameters:
    /// - allocator: Pass null for default allocator
    /// - texture_cache: The texture cache
    /// - source_image: The CVPixelBuffer to create a texture from
    /// - texture_attributes: Pass null for default attributes
    /// - pixel_format: MTLPixelFormat for the texture
    /// - width: Texture width
    /// - height: Texture height
    /// - plane_index: Plane index (0 for single-plane formats)
    /// - texture_out: Receives the created texture
    pub(crate) fn CVMetalTextureCacheCreateTextureFromImage(
        allocator: *const c_void,
        texture_cache: CVMetalTextureCacheRef,
        source_image: CVPixelBufferRef,
        texture_attributes: *const c_void,
        pixel_format: u64, // MTLPixelFormat
        width: usize,
        height: usize,
        plane_index: usize,
        texture_out: *mut CVMetalTextureRef,
    ) -> i32;

    /// Gets the Metal texture from a CVMetalTexture.
    /// Returns a pointer to the underlying MTLTexture.
    pub(crate) fn CVMetalTextureGetTexture(texture: CVMetalTextureRef) -> *mut c_void;

    /// Flushes the texture cache.
    /// Call periodically to free unused textures.
    pub(crate) fn CVMetalTextureCacheFlush(texture_cache: CVMetalTextureCacheRef, options: u64);

    // ========================================================================
    // CVOpenGLTextureCache (for OpenGL interop)
    // ========================================================================

    /// Creates a CVOpenGLTextureCache for creating OpenGL textures from CVPixelBuffers.
    ///
    /// Parameters:
    /// - allocator: Pass null for default allocator
    /// - cache_attributes: Pass null for default attributes
    /// - cgl_context: The CGL context (CGLContextObj)
    /// - cgl_pixel_format: The CGL pixel format (CGLPixelFormatObj)
    /// - texture_attributes: Pass null for default attributes
    /// - cache_out: Receives the created cache
    ///
    /// Note: The cache is tied to the CGL context and must only be used from
    /// the thread where that context is current.
    pub(crate) fn CVOpenGLTextureCacheCreate(
        allocator: *const c_void,
        cache_attributes: CFDictionaryRef,
        cgl_context: CGLContextObj,
        cgl_pixel_format: CGLPixelFormatObj,
        texture_attributes: CFDictionaryRef,
        cache_out: *mut CVOpenGLTextureCacheRef,
    ) -> i32;

    /// Creates an OpenGL texture from a CVPixelBuffer.
    ///
    /// Parameters:
    /// - allocator: Pass null for default allocator
    /// - texture_cache: The texture cache
    /// - source_image: The CVPixelBuffer to create a texture from
    /// - texture_attributes: Pass null for default attributes
    /// - texture_out: Receives the created texture
    ///
    /// The resulting texture uses GL_TEXTURE_RECTANGLE target on macOS.
    pub(crate) fn CVOpenGLTextureCacheCreateTextureFromImage(
        allocator: *const c_void,
        texture_cache: CVOpenGLTextureCacheRef,
        source_image: CVPixelBufferRef,
        texture_attributes: CFDictionaryRef,
        texture_out: *mut CVOpenGLTextureRef,
    ) -> i32;

    /// Gets the OpenGL texture name (ID) from a CVOpenGLTexture.
    pub(crate) fn CVOpenGLTextureGetName(texture: CVOpenGLTextureRef) -> u32;

    /// Gets the OpenGL texture target from a CVOpenGLTexture.
    /// Returns GL_TEXTURE_RECTANGLE (0x84F5) on macOS.
    pub(crate) fn CVOpenGLTextureGetTarget(texture: CVOpenGLTextureRef) -> u32;

    /// Flushes the OpenGL texture cache.
    /// Call periodically to free unused textures.
    pub(crate) fn CVOpenGLTextureCacheFlush(texture_cache: CVOpenGLTextureCacheRef, options: u64);

    /// Releases a CVOpenGLTextureCache.
    pub(crate) fn CVOpenGLTextureCacheRelease(texture_cache: CVOpenGLTextureCacheRef);

    /// Retains a CVOpenGLTextureCache.
    pub(crate) fn CVOpenGLTextureCacheRetain(
        texture_cache: CVOpenGLTextureCacheRef,
    ) -> CVOpenGLTextureCacheRef;

    /// Releases a CVOpenGLTexture.
    pub(crate) fn CVOpenGLTextureRelease(texture: CVOpenGLTextureRef);

    /// Retains a CVOpenGLTexture.
    pub(crate) fn CVOpenGLTextureRetain(texture: CVOpenGLTextureRef) -> CVOpenGLTextureRef;

    // ========================================================================
    // CVPixelBufferPool
    // ========================================================================

    /// Creates a pool of reusable CVPixelBuffers.
    ///
    /// Parameters:
    /// - allocator: Pass null for default allocator
    /// - pool_attributes: Dictionary with pool configuration
    /// - pixel_buffer_attributes: Dictionary with pixel buffer configuration
    /// - pool_out: Receives the created pool
    pub(crate) fn CVPixelBufferPoolCreate(
        allocator: *const c_void,
        pool_attributes: CFDictionaryRef,
        pixel_buffer_attributes: CFDictionaryRef,
        pool_out: *mut CVPixelBufferPoolRef,
    ) -> i32;

    /// Creates a CVPixelBuffer from the pool.
    ///
    /// Parameters:
    /// - allocator: Pass null for default allocator
    /// - pixel_buffer_pool: The pool to allocate from
    /// - pixel_buffer_out: Receives the created pixel buffer
    pub(crate) fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: *const c_void,
        pixel_buffer_pool: CVPixelBufferPoolRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> i32;

    /// Releases a CVPixelBufferPool.
    pub(crate) fn CVPixelBufferPoolRelease(pool: CVPixelBufferPoolRef);

    /// Retains a CVPixelBufferPool.
    pub(crate) fn CVPixelBufferPoolRetain(pool: CVPixelBufferPoolRef) -> CVPixelBufferPoolRef;
}

// CFBoolean type alias
pub(crate) type CFBooleanRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub(crate) fn CFRelease(cf: *const c_void);

    pub(crate) fn CFNumberCreate(
        allocator: *const c_void,
        the_type: i32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    pub(crate) fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;

    // Boolean constants for dictionary values
    pub(crate) static kCFBooleanTrue: CFBooleanRef;
    pub(crate) static kCFBooleanFalse: CFBooleanRef;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    // ========================================================================
    // CVPixelBuffer Dictionary Keys
    // ========================================================================

    pub(crate) static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    pub(crate) static kCVPixelBufferWidthKey: CFStringRef;
    pub(crate) static kCVPixelBufferHeightKey: CFStringRef;
    pub(crate) static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;

    // Compatibility keys - enable interop with Metal/OpenGL/CGImage
    pub(crate) static kCVPixelBufferOpenGLCompatibilityKey: CFStringRef;
    pub(crate) static kCVPixelBufferMetalCompatibilityKey: CFStringRef;
    pub(crate) static kCVPixelBufferCGImageCompatibilityKey: CFStringRef;
    pub(crate) static kCVPixelBufferCGBitmapContextCompatibilityKey: CFStringRef;

    // ========================================================================
    // CVPixelBuffer Reference Counting and Queries
    // ========================================================================

    pub(crate) fn CVPixelBufferRetain(pixel_buffer: CVPixelBufferRef) -> CVPixelBufferRef;
    pub(crate) fn CVPixelBufferRelease(pixel_buffer: CVPixelBufferRef);
    pub(crate) fn CVPixelBufferGetPixelFormatType(pixel_buffer: CVPixelBufferRef) -> u32;
    pub(crate) fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    pub(crate) fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;

    /// Get the IOSurface backing a CVPixelBuffer.
    /// Returns null if the pixel buffer is not backed by an IOSurface.
    pub(crate) fn CVPixelBufferGetIOSurface(pixel_buffer: CVPixelBufferRef) -> *const c_void;

    /// Create a CVPixelBuffer from an IOSurface.
    pub(crate) fn CVPixelBufferCreateWithIOSurface(
        allocator: *const c_void,
        surface: *const c_void,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> i32;
}
