// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CoreVideo FFI bindings for texture caches and pixel buffer pools.
//!
//! Provides bindings for:
//! - CVMetalTextureCache: Create Metal textures from CVPixelBuffers
//! - CVOpenGLTextureCache: Create OpenGL textures from CVPixelBuffers
//! - CVPixelBufferPool: Efficient pixel buffer allocation/recycling

#![allow(dead_code, non_snake_case, non_upper_case_globals)]

use std::ffi::c_void;

// Type aliases for CoreVideo opaque types
pub type CVMetalTextureCacheRef = *mut c_void;
pub type CVMetalTextureRef = *mut c_void;
pub type CVOpenGLTextureCacheRef = *mut c_void;
pub type CVOpenGLTextureRef = *mut c_void;
pub type CVPixelBufferPoolRef = *mut c_void;
pub type CVPixelBufferRef = *mut c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFNumberRef = *const c_void;

// CGL types for OpenGL context
pub type CGLContextObj = *mut c_void;
pub type CGLPixelFormatObj = *mut c_void;

// CVReturn codes
pub const kCVReturnSuccess: i32 = 0;

// CFNumber types
pub const K_CFNUMBER_SINT32_TYPE: i32 = 3;

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
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
    pub fn CVMetalTextureCacheCreate(
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
    pub fn CVMetalTextureCacheCreateTextureFromImage(
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
    pub fn CVMetalTextureGetTexture(texture: CVMetalTextureRef) -> *mut c_void;

    /// Flushes the texture cache.
    /// Call periodically to free unused textures.
    pub fn CVMetalTextureCacheFlush(texture_cache: CVMetalTextureCacheRef, options: u64);

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
    pub fn CVOpenGLTextureCacheCreate(
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
    pub fn CVOpenGLTextureCacheCreateTextureFromImage(
        allocator: *const c_void,
        texture_cache: CVOpenGLTextureCacheRef,
        source_image: CVPixelBufferRef,
        texture_attributes: CFDictionaryRef,
        texture_out: *mut CVOpenGLTextureRef,
    ) -> i32;

    /// Gets the OpenGL texture name (ID) from a CVOpenGLTexture.
    pub fn CVOpenGLTextureGetName(texture: CVOpenGLTextureRef) -> u32;

    /// Gets the OpenGL texture target from a CVOpenGLTexture.
    /// Returns GL_TEXTURE_RECTANGLE (0x84F5) on macOS.
    pub fn CVOpenGLTextureGetTarget(texture: CVOpenGLTextureRef) -> u32;

    /// Flushes the OpenGL texture cache.
    /// Call periodically to free unused textures.
    pub fn CVOpenGLTextureCacheFlush(texture_cache: CVOpenGLTextureCacheRef, options: u64);

    /// Releases a CVOpenGLTextureCache.
    pub fn CVOpenGLTextureCacheRelease(texture_cache: CVOpenGLTextureCacheRef);

    /// Retains a CVOpenGLTextureCache.
    pub fn CVOpenGLTextureCacheRetain(
        texture_cache: CVOpenGLTextureCacheRef,
    ) -> CVOpenGLTextureCacheRef;

    /// Releases a CVOpenGLTexture.
    pub fn CVOpenGLTextureRelease(texture: CVOpenGLTextureRef);

    /// Retains a CVOpenGLTexture.
    pub fn CVOpenGLTextureRetain(texture: CVOpenGLTextureRef) -> CVOpenGLTextureRef;

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
    pub fn CVPixelBufferPoolCreate(
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
    pub fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: *const c_void,
        pixel_buffer_pool: CVPixelBufferPoolRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> i32;

    /// Releases a CVPixelBufferPool.
    pub fn CVPixelBufferPoolRelease(pool: CVPixelBufferPoolRef);

    /// Retains a CVPixelBufferPool.
    pub fn CVPixelBufferPoolRetain(pool: CVPixelBufferPoolRef) -> CVPixelBufferPoolRef;
}

// CFBoolean type alias
pub type CFBooleanRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFRelease(cf: *const c_void);

    pub fn CFNumberCreate(
        allocator: *const c_void,
        the_type: i32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    pub fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;

    // Boolean constants for dictionary values
    pub static kCFBooleanTrue: CFBooleanRef;
    pub static kCFBooleanFalse: CFBooleanRef;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    // ========================================================================
    // CVPixelBuffer Dictionary Keys
    // ========================================================================

    pub static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    pub static kCVPixelBufferWidthKey: CFStringRef;
    pub static kCVPixelBufferHeightKey: CFStringRef;
    pub static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;

    // Compatibility keys - enable interop with Metal/OpenGL/CGImage
    pub static kCVPixelBufferOpenGLCompatibilityKey: CFStringRef;
    pub static kCVPixelBufferMetalCompatibilityKey: CFStringRef;
    pub static kCVPixelBufferCGImageCompatibilityKey: CFStringRef;
    pub static kCVPixelBufferCGBitmapContextCompatibilityKey: CFStringRef;

    // ========================================================================
    // CVPixelBuffer Reference Counting and Queries
    // ========================================================================

    pub fn CVPixelBufferRetain(pixel_buffer: CVPixelBufferRef) -> CVPixelBufferRef;
    pub fn CVPixelBufferRelease(pixel_buffer: CVPixelBufferRef);
    pub fn CVPixelBufferGetPixelFormatType(pixel_buffer: CVPixelBufferRef) -> u32;
    pub fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;

    /// Get the IOSurface backing a CVPixelBuffer.
    /// Returns null if the pixel buffer is not backed by an IOSurface.
    pub fn CVPixelBufferGetIOSurface(pixel_buffer: CVPixelBufferRef) -> *const c_void;

    /// Create a CVPixelBuffer from an IOSurface.
    pub fn CVPixelBufferCreateWithIOSurface(
        allocator: *const c_void,
        surface: *const c_void,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> i32;
}

// IOSurface types
pub type IOSurfaceRef = *const c_void;
pub type IOSurfaceID = u32;

// Mach port type (allow non-camel-case for FFI compatibility)
#[allow(non_camel_case_types)]
pub type mach_port_t = u32;

// IOSurface property key for global visibility.
// When set to true, the IOSurface can be looked up by ID from any process.
// Note: Deprecated in macOS 10.11, but may still work for cross-process sharing.
#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    pub static kIOSurfaceIsGlobal: CFStringRef;
}

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    /// Get the unique ID of an IOSurface.
    /// This ID can be used to look up the surface in another process.
    pub fn IOSurfaceGetID(buffer: *const c_void) -> IOSurfaceID;

    /// Look up an IOSurface by its ID.
    /// Returns null if no surface exists with this ID.
    /// The returned surface is retained (caller must release).
    pub fn IOSurfaceLookup(csid: IOSurfaceID) -> *const c_void;

    /// Increment the reference count of an IOSurface.
    pub fn IOSurfaceIncrementUseCount(buffer: *const c_void);

    /// Decrement the reference count of an IOSurface.
    pub fn IOSurfaceDecrementUseCount(buffer: *const c_void);

    /// Create a mach port for sending an IOSurface to another process.
    /// The returned mach port should be sent via IPC (e.g., SCM_RIGHTS).
    /// The receiving process uses IOSurfaceLookupFromMachPort.
    pub fn IOSurfaceCreateMachPort(buffer: IOSurfaceRef) -> mach_port_t;

    /// Look up an IOSurface from a mach port received from another process.
    /// Returns a retained IOSurface reference.
    pub fn IOSurfaceLookupFromMachPort(port: mach_port_t) -> IOSurfaceRef;
}

// Mach port deallocation
#[link(name = "System")]
extern "C" {
    /// Deallocate a mach port right.
    pub fn mach_port_deallocate(task: mach_port_t, name: mach_port_t) -> i32;

    /// Get the current task's mach port.
    pub fn mach_task_self() -> mach_port_t;
}
