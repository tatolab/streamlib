// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! vImage FFI bindings for format conversion via the Accelerate framework.

#![allow(
    dead_code,
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types
)]

use std::ffi::c_void;

use super::corevideo_ffi::CVPixelBufferRef;

// ==========================================================================
// vImage Types
// ==========================================================================

/// vImage error code (ssize_t on 64-bit).
pub type vImage_Error = isize;

/// Opaque reference to a vImageConverter.
pub type vImageConverterRef = *mut c_void;

/// Opaque reference to a CV image format descriptor.
pub type vImageCVImageFormatRef = *mut c_void;

/// vImage buffer descriptor - just a view pointing to existing memory.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vImage_Buffer {
    /// Pointer to the top-left pixel of the buffer.
    pub data: *mut c_void,
    /// Height in pixels.
    pub height: usize,
    /// Width in pixels.
    pub width: usize,
    /// Bytes per row (stride). May include padding.
    pub rowBytes: usize,
}

// ==========================================================================
// vImage Error Codes
// ==========================================================================

pub const kvImageNoError: vImage_Error = 0;
pub const kvImageRoiLargerThanInputBuffer: vImage_Error = -21766;
pub const kvImageInvalidKernelSize: vImage_Error = -21767;
pub const kvImageInvalidEdgeStyle: vImage_Error = -21768;
pub const kvImageInvalidOffset_X: vImage_Error = -21769;
pub const kvImageInvalidOffset_Y: vImage_Error = -21770;
pub const kvImageMemoryAllocationError: vImage_Error = -21771;
pub const kvImageNullPointerArgument: vImage_Error = -21772;
pub const kvImageInvalidParameter: vImage_Error = -21773;
pub const kvImageBufferSizeMismatch: vImage_Error = -21774;
pub const kvImageUnknownFlagsBit: vImage_Error = -21775;
pub const kvImageInternalError: vImage_Error = -21776;
pub const kvImageInvalidRowBytes: vImage_Error = -21777;
pub const kvImageInvalidImageFormat: vImage_Error = -21778;
pub const kvImageColorSyncIsAbsent: vImage_Error = -21779;
pub const kvImageOutOfPlaceOperationRequired: vImage_Error = -21780;
pub const kvImageInvalidImageObject: vImage_Error = -21781;
pub const kvImageInvalidCVImageFormat: vImage_Error = -21782;
pub const kvImageUnsupportedConversion: vImage_Error = -21783;

// ==========================================================================
// vImage Flags
// ==========================================================================

pub type vImage_Flags = u32;

/// Default flags - no special behavior.
pub const kvImageNoFlags: vImage_Flags = 0;
/// Do not clamp output to representable range.
pub const kvImageDoNotClamp: vImage_Flags = 1;
/// Don't allocate temp buffers - pass your own.
pub const kvImageLeaveAlphaUnchanged: vImage_Flags = 2;
/// Return only size of temp buffer needed.
pub const kvImageGetTempBufferSize: vImage_Flags = 4;
/// Print diagnostics to console.
pub const kvImagePrintDiagnosticsToConsole: vImage_Flags = 8;
/// Allow converter to skip alpha channel processing for performance.
pub const kvImageNoAllocate: vImage_Flags = 16;
/// High quality resampling (slower).
pub const kvImageHighQualityResampling: vImage_Flags = 64;

// ==========================================================================
// CVPixelBuffer Lock Flags
// ==========================================================================

pub type CVPixelBufferLockFlags = u64;

/// Lock for read-only access.
pub const kCVPixelBufferLock_ReadOnly: CVPixelBufferLockFlags = 0x00000001;

// ==========================================================================
// vImage Functions (Accelerate Framework)
// ==========================================================================

#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    // ========================================================================
    // CV Image Format
    // ========================================================================

    /// Create a vImageCVImageFormat from a CVPixelFormatType.
    ///
    /// Parameters:
    /// - pixelFormatType: The CVPixelFormatType (e.g., 'BGRA', 'RGBA')
    /// - matrix: Color matrix (NULL for RGB formats)
    /// - chromaSiting: Chroma siting (NULL for RGB formats)
    /// - colorSpace: CGColorSpace (NULL for default)
    /// - alphaIsOpaqueHint: 0 for normal alpha, 1 if alpha is always opaque
    ///
    /// Returns a new format reference, or NULL on failure.
    pub fn vImageCVImageFormat_Create(
        pixelFormatType: u32,
        matrix: *const c_void,       // vImage_YpCbCrToARGBMatrix, NULL for RGB
        chromaSiting: *const c_void, // vImageChromaSiting, NULL for RGB
        colorSpace: *const c_void,   // CGColorSpaceRef, NULL for default
        alphaIsOpaqueHint: i32,
    ) -> vImageCVImageFormatRef;

    /// Get the CVPixelFormatType from a vImageCVImageFormat.
    pub fn vImageCVImageFormat_GetFormatCode(format: vImageCVImageFormatRef) -> u32;

    /// Release a vImageCVImageFormat.
    pub fn vImageCVImageFormat_Release(format: vImageCVImageFormatRef);

    /// Retain a vImageCVImageFormat.
    pub fn vImageCVImageFormat_Retain(format: vImageCVImageFormatRef) -> vImageCVImageFormatRef;

    // ========================================================================
    // vImage Converter
    // ========================================================================

    /// Create a converter between two CVPixelBuffer formats.
    ///
    /// Parameters:
    /// - srcFormat: Source format descriptor
    /// - srcRange: Source color range (NULL for full range)
    /// - dstFormat: Destination format descriptor
    /// - dstRange: Destination color range (NULL for full range)
    /// - backgroundColor: Background color for alpha compositing (NULL for transparent)
    /// - flags: Conversion flags (kvImageNoFlags for default)
    /// - error: Receives error code on failure
    ///
    /// Returns a converter, or NULL on failure.
    pub fn vImageConverter_CreateForCVToCVImageFormat(
        srcFormat: vImageCVImageFormatRef,
        srcRange: *const c_void, // vImage_CGImageRange, NULL for full range
        dstFormat: vImageCVImageFormatRef,
        dstRange: *const c_void, // vImage_CGImageRange, NULL for full range
        backgroundColor: *const c_void, // CGFloat[4] for ARGB, NULL for transparent
        flags: vImage_Flags,
        error: *mut vImage_Error,
    ) -> vImageConverterRef;

    /// Release a vImageConverter.
    pub fn vImageConverter_Release(converter: vImageConverterRef);

    /// Retain a vImageConverter.
    pub fn vImageConverter_Retain(converter: vImageConverterRef) -> vImageConverterRef;

    /// Get the number of source buffers required by the converter.
    pub fn vImageConverter_GetNumberOfSourceBuffers(converter: vImageConverterRef) -> usize;

    /// Get the number of destination buffers required by the converter.
    pub fn vImageConverter_GetNumberOfDestinationBuffers(converter: vImageConverterRef) -> usize;

    // ========================================================================
    // Conversion Functions
    // ========================================================================

    /// Convert between any two formats using a pre-created converter.
    ///
    /// Parameters:
    /// - converter: The converter to use
    /// - srcs: Array of source buffer pointers
    /// - dsts: Array of destination buffer pointers
    /// - tempBuffer: Temporary buffer for intermediate results (NULL to auto-allocate)
    /// - flags: Conversion flags
    ///
    /// Returns kvImageNoError on success, or an error code.
    pub fn vImageConvert_AnyToAny(
        converter: vImageConverterRef,
        srcs: *const *const vImage_Buffer,
        dsts: *const *const vImage_Buffer,
        tempBuffer: *mut c_void,
        flags: vImage_Flags,
    ) -> vImage_Error;

    /// Get the size of temporary buffer needed for conversion.
    ///
    /// Call with kvImageGetTempBufferSize flag to get size without allocating.
    pub fn vImageConverter_GetTempBufferSize(
        converter: vImageConverterRef,
        flags: vImage_Flags,
    ) -> usize;
}

// ==========================================================================
// CVPixelBuffer Functions (CoreVideo Framework)
// ==========================================================================

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    /// Lock the base address of a CVPixelBuffer for CPU access.
    ///
    /// Parameters:
    /// - pixelBuffer: The pixel buffer to lock
    /// - lockFlags: 0 for read-write, kCVPixelBufferLock_ReadOnly for read-only
    ///
    /// Returns kCVReturnSuccess on success.
    pub fn CVPixelBufferLockBaseAddress(
        pixelBuffer: CVPixelBufferRef,
        lockFlags: CVPixelBufferLockFlags,
    ) -> i32;

    /// Unlock the base address of a CVPixelBuffer.
    ///
    /// Parameters:
    /// - pixelBuffer: The pixel buffer to unlock
    /// - unlockFlags: Same flags used when locking
    ///
    /// Returns kCVReturnSuccess on success.
    pub fn CVPixelBufferUnlockBaseAddress(
        pixelBuffer: CVPixelBufferRef,
        unlockFlags: CVPixelBufferLockFlags,
    ) -> i32;

    /// Get the base address of a locked CVPixelBuffer.
    ///
    /// Buffer must be locked first with CVPixelBufferLockBaseAddress.
    pub fn CVPixelBufferGetBaseAddress(pixelBuffer: CVPixelBufferRef) -> *mut c_void;

    /// Get the bytes per row (stride) of a CVPixelBuffer.
    pub fn CVPixelBufferGetBytesPerRow(pixelBuffer: CVPixelBufferRef) -> usize;
}

// ==========================================================================
// Helper Functions
// ==========================================================================

impl vImage_Buffer {
    /// Create a vImage_Buffer view from a locked CVPixelBuffer.
    ///
    /// # Safety
    /// The CVPixelBuffer must be locked with CVPixelBufferLockBaseAddress before
    /// calling this, and remain locked while the vImage_Buffer is in use.
    pub unsafe fn from_locked_pixel_buffer(
        pixel_buffer: CVPixelBufferRef,
        width: usize,
        height: usize,
    ) -> Self {
        Self {
            data: CVPixelBufferGetBaseAddress(pixel_buffer),
            height,
            width,
            rowBytes: CVPixelBufferGetBytesPerRow(pixel_buffer),
        }
    }
}

/// Convert vImage error code to human-readable description.
pub fn vimage_error_description(error: vImage_Error) -> &'static str {
    match error {
        kvImageNoError => "No error",
        kvImageRoiLargerThanInputBuffer => "ROI larger than input buffer",
        kvImageInvalidKernelSize => "Invalid kernel size",
        kvImageInvalidEdgeStyle => "Invalid edge style",
        kvImageInvalidOffset_X => "Invalid X offset",
        kvImageInvalidOffset_Y => "Invalid Y offset",
        kvImageMemoryAllocationError => "Memory allocation error",
        kvImageNullPointerArgument => "Null pointer argument",
        kvImageInvalidParameter => "Invalid parameter",
        kvImageBufferSizeMismatch => "Buffer size mismatch",
        kvImageUnknownFlagsBit => "Unknown flags bit",
        kvImageInternalError => "Internal error",
        kvImageInvalidRowBytes => "Invalid row bytes",
        kvImageInvalidImageFormat => "Invalid image format",
        kvImageColorSyncIsAbsent => "ColorSync is absent",
        kvImageOutOfPlaceOperationRequired => "Out of place operation required",
        kvImageInvalidImageObject => "Invalid image object",
        kvImageInvalidCVImageFormat => "Invalid CV image format",
        kvImageUnsupportedConversion => "Unsupported conversion",
        _ => "Unknown error",
    }
}
