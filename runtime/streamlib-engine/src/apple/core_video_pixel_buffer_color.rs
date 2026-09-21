// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A captured `CVPixelBuffer`'s colour as the H.273 description the video
//! device seam carries.
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

use crate::core::color::H273ColorVui;
use crate::core::rhi::PixelFormat;

/// H.273's "unspecified" on every axis.
const H273_UNSPECIFIED: c_int = 2;

/// The colour `pixel_buffer` is described as. An axis it leaves unattached,
/// or attaches a value CoreVideo has no code point for, is absent.
pub(crate) fn core_video_pixel_buffer_color_to_h273_color_vui(
    pixel_buffer: &CVPixelBuffer,
) -> H273ColorVui {
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
        full_range: full_range_of_biplanar_420(PixelFormat::from_cv_pixel_format_type(
            CVPixelBufferGetPixelFormatType(pixel_buffer),
        )),
    }
}

/// Whether a biplanar 4:2:0 pixel format is full range; `None` for any other
/// format, whose range CoreVideo does not state.
fn full_range_of_biplanar_420(pixel_format: PixelFormat) -> Option<bool> {
    match pixel_format {
        PixelFormat::Nv12VideoRange => Some(false),
        PixelFormat::Nv12FullRange => Some(true),
        _ => None,
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
    use crate::core::color::h273_color_vui::{matrix, primaries, transfer};
    use objc2_core_video::{
        CVAttachmentMode, CVPixelBufferCreate, kCVImageBufferColorPrimaries_ITU_R_709_2,
        kCVImageBufferTransferFunction_ITU_R_709_2, kCVImageBufferYCbCrMatrix_ITU_R_601_4,
        kCVReturnSuccess,
    };
    use std::ptr::NonNull;

    fn a_pixel_buffer_in(pixel_format: PixelFormat) -> CFRetained<CVPixelBuffer> {
        let mut pixel_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: the out-pointer is a valid stack slot and no attributes are
        // passed.
        let created = unsafe {
            CVPixelBufferCreate(
                None,
                64,
                32,
                pixel_format.as_cv_pixel_format_type(),
                None,
                NonNull::from(&mut pixel_buffer),
            )
        };
        assert_eq!(created, kCVReturnSuccess);
        // SAFETY: a successful create hands back a +1 buffer.
        unsafe { CFRetained::from_raw(NonNull::new(pixel_buffer).expect("created")) }
    }

    #[test]
    fn a_buffer_described_as_bt709_with_a_bt601_matrix_carries_those_code_points() {
        let pixel_buffer = a_pixel_buffer_in(PixelFormat::Nv12VideoRange);
        // SAFETY: CoreVideo-exported keys and values, attached to a live
        // buffer.
        unsafe {
            for (key, value) in [
                (
                    kCVImageBufferColorPrimariesKey,
                    kCVImageBufferColorPrimaries_ITU_R_709_2,
                ),
                (
                    kCVImageBufferTransferFunctionKey,
                    kCVImageBufferTransferFunction_ITU_R_709_2,
                ),
                (
                    kCVImageBufferYCbCrMatrixKey,
                    kCVImageBufferYCbCrMatrix_ITU_R_601_4,
                ),
            ] {
                pixel_buffer.set_attachment(key, value, CVAttachmentMode::ShouldPropagate);
            }
        }
        assert_eq!(
            core_video_pixel_buffer_color_to_h273_color_vui(&pixel_buffer),
            H273ColorVui {
                primaries: Some(primaries::BT709),
                transfer: Some(transfer::BT709),
                matrix: Some(matrix::SMPTE170M),
                full_range: Some(false),
            }
        );
    }

    #[test]
    fn a_full_range_buffer_with_no_colour_attached_says_only_its_range() {
        let pixel_buffer = a_pixel_buffer_in(PixelFormat::Nv12FullRange);
        assert_eq!(
            core_video_pixel_buffer_color_to_h273_color_vui(&pixel_buffer),
            H273ColorVui {
                full_range: Some(true),
                ..H273ColorVui::default()
            }
        );
    }

    #[test]
    fn a_format_that_is_not_biplanar_420_states_no_range() {
        assert_eq!(full_range_of_biplanar_420(PixelFormat::Bgra32), None);
    }

    #[test]
    fn unspecified_and_reserved_code_points_are_absent() {
        assert_eq!(h273_code_point_from_core_video(2), None);
        assert_eq!(h273_code_point_from_core_video(0), None);
        assert_eq!(h273_code_point_from_core_video(1), Some(1));
    }
}
