// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS RhiPixelBufferPool implementation using CVPixelBufferPool.

use std::ptr::NonNull;
use std::sync::mpsc::channel;

use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferOpenGLCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, CVPixelBufferPool,
};
use objc2_foundation::NSThread;

use super::COREVIDEO_INIT_LOCK;
use crate::apple::corevideo_ffi::{kCVReturnSuccess, CVPixelBufferRef};
use crate::core::rhi::{
    PixelBufferDescriptor, PixelFormat, RhiPixelBuffer, RhiPixelBufferPool, RhiPixelBufferRef,
};
use crate::core::{Result, StreamError};

/// macOS pixel buffer pool wrapping CVPixelBufferPool.
pub struct PixelBufferPoolMacOS {
    pool: *mut CVPixelBufferPool,
}

impl PixelBufferPoolMacOS {
    /// Create a new pixel buffer pool.
    ///
    /// NOTE: CVPixelBufferPoolCreate must be serialized with AVCaptureSession
    /// initialization to avoid a race condition in CoreVideo's internal
    /// `_pixelFormatDictionaryInit`. This function dispatches to the main thread
    /// if not already there.
    pub fn new(desc: &PixelBufferDescriptor) -> Result<Self> {
        let pixel_format = desc.format;
        let width = desc.width;
        let height = desc.height;

        // Reject Unknown format - CV doesn't support format value 0
        if pixel_format == PixelFormat::Unknown {
            return Err(StreamError::GpuError(
                "Cannot create pixel buffer pool with Unknown format".into(),
            ));
        }

        tracing::debug!(
            "PixelBufferPoolMacOS::new: {}x{} format={:?}",
            width,
            height,
            pixel_format
        );

        // Dispatch to main thread to serialize with camera initialization.
        // CoreVideo's _pixelFormatDictionaryInit is not thread-safe and can crash
        // if CVPixelBufferPoolCreate races with AVCaptureDeviceInput initialization.
        let sendable = run_on_main_thread_blocking(move || {
            create_pool_on_main_thread(width, height, pixel_format)
        })?;

        Ok(Self { pool: sendable.0 })
    }

    /// Acquire a buffer from the pool.
    pub fn acquire(&self) -> Result<RhiPixelBuffer> {
        tracing::trace!("PixelBufferPoolMacOS::acquire: requesting buffer");
        let mut pixel_buffer: CVPixelBufferRef = std::ptr::null_mut();

        // SAFETY: pool is valid and pixel_buffer is a valid out pointer
        let pool_ref = unsafe { &*self.pool };
        let status = unsafe {
            CVPixelBufferPool::create_pixel_buffer(
                None,
                pool_ref,
                NonNull::new(&mut pixel_buffer as *mut _ as *mut _).unwrap(),
            )
        };
        tracing::trace!(
            "PixelBufferPoolMacOS::acquire: status={} buffer={:?}",
            status,
            pixel_buffer
        );

        if status != kCVReturnSuccess || pixel_buffer.is_null() {
            return Err(StreamError::GpuError(format!(
                "Failed to acquire pixel buffer from pool: status {}",
                status
            )));
        }

        // The pool gives us a retained buffer, so use no_retain
        let buffer_ref = unsafe {
            RhiPixelBufferRef::from_cv_pixel_buffer_no_retain(pixel_buffer)
                .ok_or_else(|| StreamError::GpuError("Null pixel buffer from pool".into()))?
        };
        tracing::trace!("PixelBufferPoolMacOS::acquire: created RhiPixelBuffer");

        Ok(RhiPixelBuffer::new(buffer_ref))
    }
}

impl Drop for PixelBufferPoolMacOS {
    fn drop(&mut self) {
        if !self.pool.is_null() {
            // objc2 types use ARC-style reference counting
            // We need to release our ownership
            unsafe {
                // Drop by converting to owned and letting it drop
                let _ = objc2_core_foundation::CFRetained::from_raw(NonNull::new_unchecked(
                    self.pool as *mut _,
                ));
            }
        }
    }
}

// CVPixelBufferPool is thread-safe
unsafe impl Send for PixelBufferPoolMacOS {}
unsafe impl Sync for PixelBufferPoolMacOS {}

impl RhiPixelBufferPool {
    /// Create a new pixel buffer pool (macOS).
    pub fn new_with_descriptor(desc: &PixelBufferDescriptor) -> Result<Self> {
        Ok(Self {
            inner: PixelBufferPoolMacOS::new(desc)?,
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
        tracing::debug!("PixelBufferPool: already on main thread, executing directly");
        return f();
    }

    tracing::debug!("PixelBufferPool: dispatching to main thread");
    let (tx, rx) = channel();

    dispatch2::DispatchQueue::main().exec_async(move || {
        let result = f();
        let _ = tx.send(result);
    });

    rx.recv()
        .expect("Failed to receive result from main thread")
}

/// Wrapper to send CVPixelBufferPool pointer across threads.
/// CVPixelBufferPool is thread-safe, but the raw pointer isn't Send by default.
struct SendablePoolRef(*mut CVPixelBufferPool);
unsafe impl Send for SendablePoolRef {}

/// Create the CVPixelBufferPool - must be called on main thread.
fn create_pool_on_main_thread(
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Result<SendablePoolRef> {
    // Acquire global lock to serialize with other CoreVideo init operations
    let _guard = COREVIDEO_INIT_LOCK.lock().unwrap();

    let pixel_format = format.as_cv_pixel_format_type();

    // Log detailed format info for debugging
    tracing::info!(
        "CVPixelBufferPool: Creating pool on main thread: {}x{}, format={:?}, cv_format=0x{:08X} ('{}')",
        width, height, format, pixel_format, format.fourcc_string()
    );

    // NOTE: kCVPixelFormatType_32RGBA ('RGBA') is defined but not well-supported.
    // Prefer using the camera's native output format (usually BGRA) throughout the pipeline.
    // See: https://developer.apple.com/forums/thread/744038
    if format == PixelFormat::Rgba32 {
        tracing::warn!(
            "CVPixelBufferPool: PixelFormat::Rgba32 ('RGBA') may not be well-supported. \
             Consider using the input buffer's format instead."
        );
    }

    // Build pixel buffer attributes dictionary using core_foundation crate
    // This ensures proper CF object handling and uses the correct objc2 key constants
    let pixel_buffer_attrs = unsafe {
        // Create empty dictionary for IOSurface properties (enables GPU interop)
        let io_surface_props: CFMutableDictionary<CFString, CFTypeRef> = CFMutableDictionary::new();

        // Build attributes dictionary
        let mut attrs: CFMutableDictionary<CFString, CFTypeRef> = CFMutableDictionary::new();

        // Get keys from objc2_core_video - these are proper &CFString refs
        // Convert to core_foundation CFString by casting the raw pointer
        // SAFETY: Both objc2 and core_foundation CFString wrap the same __CFString opaque type
        let format_key_cf =
            CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey as *const _ as *const _);
        let width_key_cf =
            CFString::wrap_under_get_rule(kCVPixelBufferWidthKey as *const _ as *const _);
        let height_key_cf =
            CFString::wrap_under_get_rule(kCVPixelBufferHeightKey as *const _ as *const _);
        let iosurface_key_cf = CFString::wrap_under_get_rule(
            kCVPixelBufferIOSurfacePropertiesKey as *const _ as *const _,
        );
        let opengl_key_cf = CFString::wrap_under_get_rule(
            kCVPixelBufferOpenGLCompatibilityKey as *const _ as *const _,
        );
        let metal_key_cf = CFString::wrap_under_get_rule(
            kCVPixelBufferMetalCompatibilityKey as *const _ as *const _,
        );

        // Add values
        let format_num = CFNumber::from(pixel_format as i32);
        let width_num = CFNumber::from(width as i32);
        let height_num = CFNumber::from(height as i32);

        attrs.set(format_key_cf, format_num.as_CFTypeRef());
        attrs.set(width_key_cf, width_num.as_CFTypeRef());
        attrs.set(height_key_cf, height_num.as_CFTypeRef());
        attrs.set(iosurface_key_cf, io_surface_props.as_CFTypeRef());
        attrs.set(opengl_key_cf, CFBoolean::true_value().as_CFTypeRef());
        attrs.set(metal_key_cf, CFBoolean::true_value().as_CFTypeRef());

        attrs.to_immutable()
    };

    // Create pool using objc2_core_video API
    let mut pool: *mut CVPixelBufferPool = std::ptr::null_mut();

    let status = unsafe {
        // Convert core_foundation dictionary to objc2 reference
        // SAFETY: Both wrap the same __CFDictionary opaque type
        let attrs_ptr = pixel_buffer_attrs.as_concrete_TypeRef();
        let attrs_ref: &objc2_core_foundation::CFDictionary =
            &*(attrs_ptr as *const _ as *const objc2_core_foundation::CFDictionary);

        CVPixelBufferPool::create(
            None,            // allocator
            None,            // pool attributes
            Some(attrs_ref), // pixel buffer attributes
            NonNull::new(&mut pool).unwrap(),
        )
    };

    if status != kCVReturnSuccess || pool.is_null() {
        return Err(StreamError::GpuError(format!(
            "Failed to create CVPixelBufferPool: status {}",
            status
        )));
    }

    tracing::debug!("PixelBufferPool: created on main thread");
    Ok(SendablePoolRef(pool))
}
