// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A captured `CVPixelBuffer`'s colour, named the way `tatolab.stream.ColorInfo` names it.
//!
//! The primaries, transfer and matrix come off the buffer's CoreVideo
//! attachments through CoreVideo's own string-to-code-point table; the range
//! comes from the pixel format, which is the only place CoreVideo states it.

use objc2_core_foundation::{CFRetained, CFString, CFType};
use objc2_core_video::{
    CVColorPrimariesGetIntegerCodePointForString, CVPixelBuffer, CVPixelBufferGetPixelFormatType,
    CVTransferFunctionGetIntegerCodePointForString, CVYCbCrMatrixGetIntegerCodePointForString,
    kCVImageBufferColorPrimariesKey, kCVImageBufferTransferFunctionKey,
    kCVImageBufferYCbCrMatrixKey,
};

use crate::captured_frame_colour_description::{
    CapturedFrameColourDescription, matrix_name_of_h273_code_point,
    primaries_name_of_h273_code_point, transfer_name_of_h273_code_point,
};

/// The colour `pixel_buffer` is described as. An axis it leaves unattached,
/// or attaches a value CoreVideo has no code point for, is absent.
pub fn colour_description_of_core_video_pixel_buffer(
    pixel_buffer: &CVPixelBuffer,
) -> CapturedFrameColourDescription {
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
    CapturedFrameColourDescription {
        primaries: attached_string(primaries_key).and_then(|primaries| {
            primaries_name_of_h273_code_point(CVColorPrimariesGetIntegerCodePointForString(Some(
                &primaries,
            )))
        }),
        transfer: attached_string(transfer_key).and_then(|transfer| {
            transfer_name_of_h273_code_point(CVTransferFunctionGetIntegerCodePointForString(Some(
                &transfer,
            )))
        }),
        matrix: attached_string(matrix_key).and_then(|matrix| {
            matrix_name_of_h273_code_point(CVYCbCrMatrixGetIntegerCodePointForString(Some(&matrix)))
        }),
        range: match &CVPixelBufferGetPixelFormatType(pixel_buffer).to_be_bytes() {
            b"420v" => Some("limited"),
            b"420f" => Some("full"),
            _ => None,
        },
    }
}
