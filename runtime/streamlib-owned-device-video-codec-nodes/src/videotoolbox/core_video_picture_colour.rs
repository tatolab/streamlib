// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A decoded `CVPixelBuffer`'s colour as an H.273 description.
//!
//! The primaries, transfer and matrix come off the buffer's CoreVideo
//! attachments through CoreVideo's own string-to-code-point table; the range
//! comes from the pixel format, which is the only place CoreVideo states it.

use std::ffi::c_int;

use objc2_core_foundation::{CFRetained, CFString, CFType};
use objc2_core_video::{
    CVColorPrimariesGetIntegerCodePointForString, CVPixelBuffer, CVPixelBufferGetPixelFormatType,
    CVTransferFunctionGetIntegerCodePointForString, CVYCbCrMatrixGetIntegerCodePointForString,
    kCVImageBufferColorPrimariesKey, kCVImageBufferTransferFunctionKey,
    kCVImageBufferYCbCrMatrixKey,
};
use streamlib_consumer_rhi::H273ColorVui;

use super::{CORE_VIDEO_NV12_FULL_RANGE, CORE_VIDEO_NV12_VIDEO_RANGE};

/// H.273's "unspecified" on every axis.
const H273_UNSPECIFIED: c_int = 2;

/// The colour `pixel_buffer` is described as. An axis it leaves unattached,
/// or attaches a value CoreVideo has no code point for, is absent.
pub fn h273_color_vui_of_core_video_pixel_buffer(pixel_buffer: &CVPixelBuffer) -> H273ColorVui {
    // SAFETY: each key is a CoreVideo-exported constant, and a null attachment
    // mode is documented as "don't report it".
    let attached_string = |key: &CFString| -> Option<CFRetained<CFString>> {
        unsafe { pixel_buffer.attachment(key, std::ptr::null_mut()) }
            .and_then(|attachment: CFRetained<CFType>| attachment.downcast::<CFString>().ok())
    };
    // SAFETY: the keys are CoreVideo-exported constants read as `&'static`.
    let (primaries_key, transfer_key, matrix_key) = unsafe {
        (
            kCVImageBufferColorPrimariesKey,
            kCVImageBufferTransferFunctionKey,
            kCVImageBufferYCbCrMatrixKey,
        )
    };
    H273ColorVui {
        primaries: attached_string(primaries_key).and_then(|primaries| {
            h273_code_point_from_core_video(CVColorPrimariesGetIntegerCodePointForString(Some(
                &primaries,
            )))
        }),
        transfer: attached_string(transfer_key).and_then(|transfer| {
            h273_code_point_from_core_video(CVTransferFunctionGetIntegerCodePointForString(Some(
                &transfer,
            )))
        }),
        matrix: attached_string(matrix_key).and_then(|matrix| {
            h273_code_point_from_core_video(CVYCbCrMatrixGetIntegerCodePointForString(Some(
                &matrix,
            )))
        }),
        full_range: match CVPixelBufferGetPixelFormatType(pixel_buffer) {
            CORE_VIDEO_NV12_VIDEO_RANGE => Some(false),
            CORE_VIDEO_NV12_FULL_RANGE => Some(true),
            _ => None,
        },
    }
}

/// A code point CoreVideo answered, or `None` where it said nothing usable: 2
/// is unspecified on every axis, and 0 is reserved for primaries and transfer
/// and, as the matrix, identity — which no YCbCr buffer can mean.
fn h273_code_point_from_core_video(code_point: c_int) -> Option<u8> {
    match code_point {
        0 | H273_UNSPECIFIED => None,
        code_point => u8::try_from(code_point).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unspecified_and_reserved_code_points_are_absent() {
        assert_eq!(h273_code_point_from_core_video(2), None);
        assert_eq!(h273_code_point_from_core_video(0), None);
        assert_eq!(h273_code_point_from_core_video(1), Some(1));
    }
}
