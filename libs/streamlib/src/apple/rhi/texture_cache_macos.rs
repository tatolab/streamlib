// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS RhiTextureCache implementation using CVMetalTextureCache.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::channel;

use metal::foreign_types::ForeignTypeRef;
use objc2_foundation::NSThread;

use super::COREVIDEO_INIT_LOCK;

/// Auto-flush interval: flush stale cache entries every N texture creations.
/// 60 = roughly once per second at 60fps.
const AUTO_FLUSH_INTERVAL: u64 = 60;
use crate::apple::corevideo_ffi::{
    kCVReturnSuccess, CFRelease, CVMetalTextureCacheCreate,
    CVMetalTextureCacheCreateTextureFromImage, CVMetalTextureCacheFlush, CVMetalTextureCacheRef,
    CVMetalTextureGetTexture, CVMetalTextureRef,
};
use crate::core::rhi::{RhiPixelBuffer, RhiTextureCache, RhiTextureView};
use crate::core::{Result, StreamError};

/// macOS texture cache wrapping CVMetalTextureCache.
///
/// Automatically flushes stale entries every [`AUTO_FLUSH_INTERVAL`] texture creations
/// to prevent memory accumulation during long-running streams.
pub struct TextureCacheMacOS {
    cache: CVMetalTextureCacheRef,
    /// Counter for auto-flush. Incremented on each create_view call.
    view_count: AtomicU64,
}

impl TextureCacheMacOS {
    /// Create a new texture cache for the given Metal device.
    ///
    /// NOTE: CVMetalTextureCacheCreate must be serialized with AVCaptureSession
    /// initialization to avoid a race condition in CoreVideo's internal
    /// `_pixelFormatDictionaryInit`. This function dispatches to the main thread
    /// if not already there.
    pub fn new(metal_device: &metal::DeviceRef) -> Result<Self> {
        // Get the raw device pointer to pass across thread boundary
        let device_ptr = metal_device.as_ptr() as usize;

        tracing::debug!("TextureCacheMacOS::new: creating cache");

        // Dispatch to main thread to serialize with camera initialization.
        // CoreVideo's _pixelFormatDictionaryInit is not thread-safe and can crash
        // if CVMetalTextureCacheCreate races with AVCaptureDeviceInput initialization.
        let sendable =
            run_on_main_thread_blocking(move || create_cache_on_main_thread(device_ptr))?;

        Ok(Self {
            cache: sendable.0,
            view_count: AtomicU64::new(0),
        })
    }

    /// Create a texture view from a pixel buffer.
    ///
    /// Automatically flushes stale cache entries every [`AUTO_FLUSH_INTERVAL`] calls.
    pub fn create_view(&self, buffer: &RhiPixelBuffer) -> Result<RhiTextureView> {
        // Auto-flush stale entries periodically to prevent memory accumulation
        let count = self.view_count.fetch_add(1, Ordering::Relaxed);
        if count > 0 && count.is_multiple_of(AUTO_FLUSH_INTERVAL) {
            self.flush();
        }

        let cv_buffer = buffer.as_ptr();
        let format = buffer.format();
        let mtl_format = format.to_mtl_pixel_format();

        let mut texture: CVMetalTextureRef = ptr::null_mut();

        let status = unsafe {
            CVMetalTextureCacheCreateTextureFromImage(
                ptr::null(), // allocator
                self.cache,  // texture cache
                cv_buffer,   // source image (CVPixelBuffer)
                ptr::null(), // texture attributes
                mtl_format,  // MTLPixelFormat
                buffer.width as usize,
                buffer.height as usize,
                0, // plane index
                &mut texture,
            )
        };

        if status != kCVReturnSuccess || texture.is_null() {
            return Err(StreamError::GpuError(format!(
                "Failed to create texture from CVPixelBuffer: status {}",
                status
            )));
        }

        Ok(RhiTextureView {
            inner: TextureViewMacOS { texture },
            source_buffer: buffer.clone(),
        })
    }

    /// Flush the cache to free unused textures.
    pub fn flush(&self) {
        unsafe {
            CVMetalTextureCacheFlush(self.cache, 0);
        }
    }
}

impl Drop for TextureCacheMacOS {
    fn drop(&mut self) {
        if !self.cache.is_null() {
            unsafe {
                CFRelease(self.cache as *const c_void);
            }
        }
    }
}

// CVMetalTextureCache is thread-safe
unsafe impl Send for TextureCacheMacOS {}
unsafe impl Sync for TextureCacheMacOS {}

/// macOS texture view wrapping CVMetalTexture.
pub struct TextureViewMacOS {
    texture: CVMetalTextureRef,
}

impl TextureViewMacOS {
    /// Get the underlying Metal texture.
    pub fn as_metal_texture(&self) -> &metal::TextureRef {
        unsafe {
            let mtl_texture = CVMetalTextureGetTexture(self.texture);
            metal::TextureRef::from_ptr(mtl_texture as *mut _)
        }
    }
}

impl Drop for TextureViewMacOS {
    fn drop(&mut self) {
        if !self.texture.is_null() {
            unsafe {
                CFRelease(self.texture as *const c_void);
            }
        }
    }
}

// CVMetalTexture is thread-safe
unsafe impl Send for TextureViewMacOS {}
unsafe impl Sync for TextureViewMacOS {}

impl RhiTextureCache {
    /// Create a new texture cache for the given Metal device (macOS).
    pub fn new_metal(metal_device: &metal::DeviceRef) -> Result<Self> {
        Ok(Self {
            inner: TextureCacheMacOS::new(metal_device)?,
        })
    }
}

/// Dispatch a closure to the main thread and wait for the result.
///
/// If already on main thread, executes directly to avoid deadlock.
fn run_on_main_thread_blocking<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let is_main_thread = NSThread::currentThread().isMainThread();

    if is_main_thread {
        tracing::debug!("TextureCache: already on main thread, executing directly");
        return f();
    }

    tracing::debug!("TextureCache: dispatching to main thread");
    let (tx, rx) = channel();

    dispatch2::DispatchQueue::main().exec_async(move || {
        let result = f();
        let _ = tx.send(result);
    });

    rx.recv()
        .expect("Failed to receive result from main thread")
}

/// Wrapper to send CVMetalTextureCacheRef across threads.
/// CVMetalTextureCache is thread-safe, but the raw pointer isn't Send by default.
struct SendableCacheRef(CVMetalTextureCacheRef);
unsafe impl Send for SendableCacheRef {}

/// Create the CVMetalTextureCache - must be called on main thread.
fn create_cache_on_main_thread(device_ptr: usize) -> Result<SendableCacheRef> {
    // Acquire global lock to serialize with other CoreVideo init operations
    let _guard = COREVIDEO_INIT_LOCK.lock().unwrap();

    let mut cache: CVMetalTextureCacheRef = ptr::null_mut();

    let status = unsafe {
        CVMetalTextureCacheCreate(
            ptr::null(),                 // allocator
            ptr::null(),                 // cache attributes
            device_ptr as *const c_void, // metal device
            ptr::null(),                 // texture attributes
            &mut cache,
        )
    };

    if status != kCVReturnSuccess || cache.is_null() {
        return Err(StreamError::GpuError(format!(
            "Failed to create CVMetalTextureCache: status {}",
            status
        )));
    }

    tracing::debug!("TextureCache: created on main thread");
    Ok(SendableCacheRef(cache))
}
