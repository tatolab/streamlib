// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS RhiFormatConverter implementation using vImageConverter.

use crate::apple::corevideo_ffi::kCVReturnSuccess;
use crate::apple::vimage_ffi::{
    kCVPixelBufferLock_ReadOnly, kvImageNoError, kvImageNoFlags, vImageCVImageFormat_Create,
    vImageCVImageFormat_Release, vImageConvert_AnyToAny, vImageConverterRef,
    vImageConverter_CreateForCVToCVImageFormat, vImageConverter_Release, vImage_Buffer,
    vimage_error_description, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferUnlockBaseAddress,
};
use crate::core::rhi::{PixelFormat, RhiPixelBuffer};
use crate::core::{Result, StreamError};
use std::ptr;

/// macOS format converter wrapping vImageConverter.
pub struct FormatConverterMacOS {
    converter: vImageConverterRef,
    source_format: PixelFormat,
    dest_format: PixelFormat,
}

impl FormatConverterMacOS {
    /// Create a new converter for the given format pair.
    pub fn new(source_format: PixelFormat, dest_format: PixelFormat) -> Result<Self> {
        // Create vImageCVImageFormat descriptors
        let src_cv_format = unsafe {
            vImageCVImageFormat_Create(
                source_format.as_cv_pixel_format_type(),
                ptr::null(), // matrix - NULL for RGB formats
                ptr::null(), // chromaSiting - NULL for RGB formats
                ptr::null(), // colorSpace - NULL for default
                0,           // alphaIsOpaqueHint
            )
        };

        if src_cv_format.is_null() {
            return Err(StreamError::GpuError(format!(
                "Failed to create vImageCVImageFormat for source format {:?}",
                source_format
            )));
        }

        let dst_cv_format = unsafe {
            vImageCVImageFormat_Create(
                dest_format.as_cv_pixel_format_type(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                0,
            )
        };

        if dst_cv_format.is_null() {
            unsafe { vImageCVImageFormat_Release(src_cv_format) };
            return Err(StreamError::GpuError(format!(
                "Failed to create vImageCVImageFormat for dest format {:?}",
                dest_format
            )));
        }

        // Create the converter
        let mut error = 0isize;
        let converter = unsafe {
            vImageConverter_CreateForCVToCVImageFormat(
                src_cv_format,
                ptr::null(), // srcRange - NULL for full range
                dst_cv_format,
                ptr::null(), // dstRange - NULL for full range
                ptr::null(), // backgroundColor - NULL for transparent
                kvImageNoFlags,
                &mut error,
            )
        };

        // Release format descriptors (converter retains what it needs)
        unsafe {
            vImageCVImageFormat_Release(src_cv_format);
            vImageCVImageFormat_Release(dst_cv_format);
        }

        if converter.is_null() || error != kvImageNoError {
            return Err(StreamError::GpuError(format!(
                "Failed to create vImageConverter: {} ({:?} -> {:?})",
                vimage_error_description(error),
                source_format,
                dest_format
            )));
        }

        Ok(Self {
            converter,
            source_format,
            dest_format,
        })
    }

    /// Get the source format.
    pub fn source_format(&self) -> PixelFormat {
        self.source_format
    }

    /// Get the destination format.
    pub fn dest_format(&self) -> PixelFormat {
        self.dest_format
    }

    /// Convert pixel data from source to destination buffer.
    ///
    /// Thread-safe: vImageConvert_AnyToAny is safe for concurrent use with
    /// the same converter as long as source/dest buffers are distinct.
    pub fn convert(&self, source: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        // Verify dimensions match
        if source.width != dest.width || source.height != dest.height {
            return Err(StreamError::Configuration(format!(
                "Buffer dimension mismatch: source {}x{}, dest {}x{}",
                source.width, source.height, dest.width, dest.height
            )));
        }

        let src_ptr = source.as_ptr();
        let dst_ptr = dest.as_ptr();
        let width = source.width as usize;
        let height = source.height as usize;

        // Lock source for read-only access
        let src_lock_result =
            unsafe { CVPixelBufferLockBaseAddress(src_ptr, kCVPixelBufferLock_ReadOnly) };
        if src_lock_result != kCVReturnSuccess {
            return Err(StreamError::GpuError(format!(
                "Failed to lock source pixel buffer: {}",
                src_lock_result
            )));
        }

        // Lock destination for read-write access
        let dst_lock_result = unsafe { CVPixelBufferLockBaseAddress(dst_ptr, 0) };
        if dst_lock_result != kCVReturnSuccess {
            unsafe { CVPixelBufferUnlockBaseAddress(src_ptr, kCVPixelBufferLock_ReadOnly) };
            return Err(StreamError::GpuError(format!(
                "Failed to lock destination pixel buffer: {}",
                dst_lock_result
            )));
        }

        // Create vImage_Buffer views (just pointers, no allocation)
        let src_buffer = vImage_Buffer {
            data: unsafe { CVPixelBufferGetBaseAddress(src_ptr) },
            height,
            width,
            rowBytes: unsafe { CVPixelBufferGetBytesPerRow(src_ptr) },
        };

        let mut dst_buffer = vImage_Buffer {
            data: unsafe { CVPixelBufferGetBaseAddress(dst_ptr) },
            height,
            width,
            rowBytes: unsafe { CVPixelBufferGetBytesPerRow(dst_ptr) },
        };

        // Perform conversion
        let src_buffers: [*const vImage_Buffer; 1] = [&src_buffer];
        let dst_buffers: [*const vImage_Buffer; 1] = [&mut dst_buffer];

        let result = unsafe {
            vImageConvert_AnyToAny(
                self.converter,
                src_buffers.as_ptr(),
                dst_buffers.as_ptr(),
                ptr::null_mut(), // tempBuffer - let vImage allocate if needed
                kvImageNoFlags,
            )
        };

        // Unlock buffers (always, even on error)
        unsafe {
            CVPixelBufferUnlockBaseAddress(dst_ptr, 0);
            CVPixelBufferUnlockBaseAddress(src_ptr, kCVPixelBufferLock_ReadOnly);
        }

        if result != kvImageNoError {
            return Err(StreamError::GpuError(format!(
                "vImageConvert_AnyToAny failed: {}",
                vimage_error_description(result)
            )));
        }

        Ok(())
    }
}

impl Drop for FormatConverterMacOS {
    fn drop(&mut self) {
        if !self.converter.is_null() {
            unsafe { vImageConverter_Release(self.converter) };
        }
    }
}

// vImageConverter is thread-safe for concurrent conversions
unsafe impl Send for FormatConverterMacOS {}
unsafe impl Sync for FormatConverterMacOS {}
