// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The codec nodes on macOS: hardware H.264 and H.265 on Apple's media engine
//! through VideoToolbox, with every frame crossing the host on an IOSurface.

pub mod core_video_picture_colour;
pub mod videotoolbox_compression_session;
pub mod videotoolbox_decompression_session;
pub mod videotoolbox_video_decoder;
pub mod videotoolbox_video_encoder;

use std::ptr::NonNull;
use std::sync::Once;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType, Type};
use objc2_core_media::{CMVideoCodecType, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC};
use objc2_core_video::CVPixelFormatDescriptionArrayCreateWithAllPixelFormatTypes;
use streamlib_video_bitstream::VideoCodecElementaryStream;

/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`.
pub const CORE_VIDEO_NV12_VIDEO_RANGE: u32 = u32::from_be_bytes(*b"420v");
/// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`.
pub const CORE_VIDEO_NV12_FULL_RANGE: u32 = u32::from_be_bytes(*b"420f");

/// The pixel format wire name of an 8-bit biplanar 4:2:0 CoreVideo format;
/// `None` for any other.
pub fn nv12_pixel_format_wire_name_of_core_video_format(
    core_video_pixel_format: u32,
) -> Option<&'static str> {
    match core_video_pixel_format {
        CORE_VIDEO_NV12_VIDEO_RANGE => Some("nv12_video_range"),
        CORE_VIDEO_NV12_FULL_RANGE => Some("nv12_full_range"),
        _ => None,
    }
}

/// The CoreMedia codec type an elementary stream is.
pub fn core_media_codec_type_of(elementary_stream: VideoCodecElementaryStream) -> CMVideoCodecType {
    match elementary_stream {
        VideoCodecElementaryStream::H264 => kCMVideoCodecType_H264,
        VideoCodecElementaryStream::H265 => kCMVideoCodecType_HEVC,
    }
}

/// A VideoToolbox, CoreMedia or CoreVideo call's failure, naming the call and
/// the status it answered.
fn videotoolbox_call_failure(call: &str, os_status: i32) -> String {
    format!("{call} failed with OSStatus {os_status}")
}

/// Take ownership of the CoreFoundation object a create call wrote at +1, or
/// answer the call's refusal when it reported a failing status or wrote none.
/// `OSStatus` and `CVReturn` alike report success as 0.
///
/// # Safety
///
/// `created` is null or the +1 object written by the create call that
/// answered `os_status`.
unsafe fn adopt_created_core_foundation_object<CreatedObject: Type>(
    created: *mut CreatedObject,
    os_status: i32,
    refusal: impl FnOnce(i32) -> String,
) -> Result<CFRetained<CreatedObject>, String> {
    match NonNull::new(created).filter(|_| os_status == 0) {
        // SAFETY: per the caller, the +1 object of a successful create.
        Some(created) => Ok(unsafe { CFRetained::from_raw(created) }),
        None => Err(refusal(os_status)),
    }
}

/// A dictionary of `CFString` keys to boolean values, the shape the encoder
/// and decoder specifications take.
fn cf_dictionary_of_booleans(
    entries: &[(&CFString, bool)],
) -> CFRetained<CFDictionary<CFString, CFType>> {
    let keys: Vec<&CFString> = entries.iter().map(|(key, _)| *key).collect();
    let values: Vec<&CFType> = entries
        .iter()
        .map(|(_, value)| -> &CFType { CFBoolean::new(*value) })
        .collect();
    CFDictionary::from_slices(&keys, &values)
}

/// Build CoreVideo's pixel-format dictionary once, before a session's pools
/// reach it: CoreVideo builds it lazily and not thread-safely.
fn ensure_core_video_pixel_format_dictionary_is_initialised() {
    static INITIALISED: Once = Once::new();
    INITIALISED.call_once(|| {
        let every_pixel_format_description =
            CVPixelFormatDescriptionArrayCreateWithAllPixelFormatTypes(None);
        tracing::debug!(
            initialised = every_pixel_format_description.is_some(),
            "CoreVideo's pixel-format dictionary built"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_elementary_stream_opens_the_core_media_codec_of_the_same_name() {
        assert_eq!(
            core_media_codec_type_of(VideoCodecElementaryStream::H264),
            u32::from_be_bytes(*b"avc1")
        );
        assert_eq!(
            core_media_codec_type_of(VideoCodecElementaryStream::H265),
            u32::from_be_bytes(*b"hvc1")
        );
    }

    #[test]
    fn only_biplanar_420_names_a_pixel_format() {
        assert_eq!(
            nv12_pixel_format_wire_name_of_core_video_format(CORE_VIDEO_NV12_VIDEO_RANGE),
            Some("nv12_video_range")
        );
        assert_eq!(
            nv12_pixel_format_wire_name_of_core_video_format(CORE_VIDEO_NV12_FULL_RANGE),
            Some("nv12_full_range")
        );
        assert_eq!(
            nv12_pixel_format_wire_name_of_core_video_format(u32::from_be_bytes(*b"BGRA")),
            None
        );
    }
}
