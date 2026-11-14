// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! One access unit decoded by the codec modules' VideoToolbox decompression session, its NV12
//! pixel buffer read on the CPU and converted to RGBA.

use objc2_core_video::{
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use streamlib_asset_media_facts::{
    Nv12PictureColour, PosterFrameVideoCodec, RgbaPictureFromThePosterFrameHelper,
    rgba_of_an_nv12_picture,
};
use streamlib_owned_device_video_codec_nodes::videotoolbox::core_video_picture_colour::h273_color_vui_of_core_video_pixel_buffer;
use streamlib_owned_device_video_codec_nodes::videotoolbox::videotoolbox_decompression_session::{
    DecodedVideoToolboxPicture, VideoToolboxDecompressionSession,
};
use streamlib_video_bitstream::VideoCodecElementaryStream;

const LUMA_PLANE_INDEX: usize = 0;
const CHROMA_PLANE_INDEX: usize = 1;

fn rgba_picture_of(
    decoded: &DecodedVideoToolboxPicture,
) -> Result<RgbaPictureFromThePosterFrameHelper, String> {
    let pixel_buffer = &decoded.pixel_buffer;
    let colour_vui = h273_color_vui_of_core_video_pixel_buffer(pixel_buffer);
    let colour = Nv12PictureColour {
        h273_matrix_coefficients: colour_vui.matrix,
        full_range: colour_vui.full_range.unwrap_or(false),
    };
    // SAFETY: a decoded pixel buffer VideoToolbox handed back, locked read-only for the reads
    // below and unlocked after; each plane's base address spans its bytes per row times its
    // height while locked.
    unsafe {
        if CVPixelBufferLockBaseAddress(pixel_buffer, CVPixelBufferLockFlags::ReadOnly) != 0 {
            return Err("the decoded pixel buffer could not be locked for reading".to_owned());
        }
        let plane = |plane_index: usize| -> Option<(&[u8], usize)> {
            let base_address = CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, plane_index);
            let row_stride_bytes = CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, plane_index);
            let rows = CVPixelBufferGetHeightOfPlane(pixel_buffer, plane_index);
            (!base_address.is_null()).then(|| {
                (
                    std::slice::from_raw_parts(base_address.cast::<u8>(), row_stride_bytes * rows),
                    row_stride_bytes,
                )
            })
        };
        let rgba_bytes = match (plane(LUMA_PLANE_INDEX), plane(CHROMA_PLANE_INDEX)) {
            (
                Some((luma_plane, luma_row_stride_bytes)),
                Some((chroma_plane, chroma_row_stride_bytes)),
            ) => rgba_of_an_nv12_picture(
                decoded.width,
                decoded.height,
                luma_plane,
                luma_row_stride_bytes,
                chroma_plane,
                chroma_row_stride_bytes,
                colour,
            ),
            _ => None,
        };
        CVPixelBufferUnlockBaseAddress(pixel_buffer, CVPixelBufferLockFlags::ReadOnly);
        rgba_bytes
            .map(|rgba_bytes| RgbaPictureFromThePosterFrameHelper {
                pixel_width: decoded.width,
                pixel_height: decoded.height,
                rgba_bytes,
            })
            .ok_or_else(|| "the decoded pixel buffer is no NV12 picture of its extent".to_owned())
    }
}

/// The first picture `annex_b_access_unit` decodes to, as RGBA.
pub(crate) fn decode_the_first_picture_on_videotoolbox(
    codec: PosterFrameVideoCodec,
    annex_b_access_unit: &[u8],
) -> Result<RgbaPictureFromThePosterFrameHelper, String> {
    let elementary_stream = match codec {
        PosterFrameVideoCodec::H264 => VideoCodecElementaryStream::H264,
        PosterFrameVideoCodec::H265 => VideoCodecElementaryStream::H265,
    };
    let mut decompression_session =
        VideoToolboxDecompressionSession::open(elementary_stream, None)?;
    let decoded = decompression_session.decode_annex_b_access_unit(annex_b_access_unit)?;
    decoded
        .first()
        .ok_or_else(|| "the access unit decoded to no picture".to_owned())
        .and_then(rgba_picture_of)
}
