// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The codec modules' VideoToolbox sessions, encode to decode on real
//! hardware: each checked-in PSNR reference is written into a pixel buffer
//! from the compression session's own pool as the BT.709 limited-range NV12
//! the host's conversion writes, coded, decoded back, and scored inside the
//! bands the Linux round trip holds its decodes to — Y and both chroma planes
//! at 30 dB or better over BT.709 full-range 4:2:0.
//!
//! The access units in between are held to the wire the Linux encoder speaks:
//! Annex-B with four-byte start codes, parameter sets in front of every sync
//! point and nowhere else, a coded extent that is the one the stream's own
//! SPS states, and an SPS VUI stating the BT.709 limited-range samples every
//! encoder writes.
//!
//! This file decodes the checked-in cross-floor clips the Linux owned-device
//! decoder also decodes, and `regenerate_the_cross_floor_clips` rewrites them.
//!
//! Rig tier — it needs Apple's hardware encoder and decoder, which a virtual
//! machine does not expose, so it runs only under `hardware-tests`.
//! `STREAMLIB_VIDEOTOOLBOX_ROUND_TRIP_PNG_DIR` names a directory each scored
//! decode is written into, as `<codec>__<reference>.png`.

#![cfg(target_os = "macos")]

#[path = "support/codec_round_trip_scoring.rs"]
mod codec_round_trip_scoring;

use std::path::Path;

use codec_round_trip_scoring::{
    CHROMA_PSNR_FAIL_FLOOR_DB, CROSS_FLOOR_CLIP_HEIGHT, CROSS_FLOOR_CLIP_WIDTH,
    LUMA_PSNR_FAIL_FLOOR_DB, Rgba8Picture, Yuv420PlanePsnr, cross_floor_clip_reference_picture,
    psnr_reference_paths, read_access_unit_clip, videotoolbox_cross_floor_clip_path,
    write_access_unit_clip,
};
use objc2_core_video::{CVPixelBuffer, CVPixelBufferGetIOSurface, CVPixelBufferGetPixelFormatType};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
use streamlib_consumer_rhi::h273_color_vui::{
    H273ColorVui, color_vui_every_engine_encoder_writes, primaries, transfer,
};
use streamlib_owned_device_video_codec_nodes::encoded_video_publication::EncodedVideoAccessUnit;
use streamlib_owned_device_video_codec_nodes::videotoolbox::videotoolbox_compression_session::{
    VideoToolboxCompressionSession, VideoToolboxCompressionSessionRequest,
};
use streamlib_owned_device_video_codec_nodes::videotoolbox::videotoolbox_decompression_session::{
    DecodedVideoToolboxPicture, VideoToolboxDecompressionSession,
};
use streamlib_owned_device_video_codec_nodes::videotoolbox::{
    CORE_VIDEO_NV12_FULL_RANGE, CORE_VIDEO_NV12_VIDEO_RANGE,
};
use streamlib_video_bitstream::VideoCodecElementaryStream;
use streamlib_video_bitstream::h265_sequence_parameter_set::parse_h265_sequence_parameter_set;
use streamlib_video_bitstream::nal_unit_raw_byte_sequence_payload::{
    RbspBitstreamReader, remove_emulation_prevention_bytes,
};

/// The references' extent. 1080 is a multiple of neither codec's block, so
/// every stream carries a conformance crop the decoder must honour.
const REFERENCE_WIDTH: u32 = 1920;
const REFERENCE_HEIGHT: u32 = 1080;

/// The rate the session is told frames arrive at, and the sync-point cadence:
/// one sync point per reference run, so each run is decodable on its own.
const FRAMES_PER_SECOND: u32 = 10;
const KEYFRAME_INTERVAL_SECONDS: u32 = 1;
const FRAMES_PER_REFERENCE: u32 = FRAMES_PER_SECOND * KEYFRAME_INTERVAL_SECONDS;

/// The frames each cross-floor clip holds: a sync point, four frames that
/// reference it, then a second sync point — so a decoder must both enter the
/// stream and re-enter it.
const CROSS_FLOOR_CLIP_FRAMES: u32 = 6;
const CROSS_FLOOR_CLIP_FRAMES_PER_SECOND: u32 = 5;

/// BT.709 luma weights and the chroma scale factors that go with them.
const BT709_RED_WEIGHT: f64 = 0.2126;
const BT709_GREEN_WEIGHT: f64 = 0.7152;
const BT709_BLUE_WEIGHT: f64 = 0.0722;
const BT709_BLUE_DIFFERENCE_SCALE: f64 = 1.8556;
const BT709_RED_DIFFERENCE_SCALE: f64 = 1.5748;

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn h264_round_trips_every_psnr_reference_inside_the_bands() {
    round_trip_every_psnr_reference(VideoCodecElementaryStream::H264);
}

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn h265_round_trips_every_psnr_reference_inside_the_bands() {
    round_trip_every_psnr_reference(VideoCodecElementaryStream::H265);
}

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn the_checked_in_h264_clip_decodes_inside_the_bands() {
    decode_the_checked_in_clip_inside_the_bands(VideoCodecElementaryStream::H264);
}

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn the_checked_in_h265_clip_decodes_inside_the_bands() {
    decode_the_checked_in_clip_inside_the_bands(VideoCodecElementaryStream::H265);
}

/// Rewrites the checked-in cross-floor clips from this machine's encoder.
/// Run on purpose, never by a sweep:
/// `cargo test -p streamlib-owned-device-video-codec-nodes --features
/// hardware-tests --test videotoolbox_sessions_round_trip_the_psnr_references
/// -- --ignored regenerate_the_cross_floor_clips`.
#[test]
#[ignore = "rewrites checked-in fixtures; run by name"]
fn regenerate_the_cross_floor_clips() {
    let reference = cross_floor_clip_reference_picture();
    for elementary_stream in [
        VideoCodecElementaryStream::H264,
        VideoCodecElementaryStream::H265,
    ] {
        let mut compression_session =
            VideoToolboxCompressionSession::open(&VideoToolboxCompressionSessionRequest {
                elementary_stream,
                width: CROSS_FLOOR_CLIP_WIDTH,
                height: CROSS_FLOOR_CLIP_HEIGHT,
                frames_per_second: CROSS_FLOOR_CLIP_FRAMES_PER_SECOND,
                keyframe_interval_seconds: 1,
                bitrate_bps: None,
                color_vui: color_vui_encoded_for_the_test_pattern(),
            })
            .expect("a hardware compression session");
        let mut access_units = Vec::new();
        for _ in 0..CROSS_FLOOR_CLIP_FRAMES {
            access_units.extend(
                encode_the_reference(&mut compression_session, &reference)
                    .into_iter()
                    .map(|access_unit| access_unit.annex_b_access_unit_bytes),
            );
        }
        write_access_unit_clip(
            &videotoolbox_cross_floor_clip_path(elementary_stream),
            &access_units,
        );
    }
}

/// CoreMedia reports a decoded stream's extents already cropped, so the
/// session holds its cap against the picture: a 1080-line stream passes a
/// 1080-line cap and is refused, by name, by a 1079-line one.
#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn a_decode_cap_below_the_picture_is_refused_and_one_at_it_is_not() {
    let reference = Rgba8Picture::read_png(&psnr_reference_paths()[0]);
    let mut compression_session =
        VideoToolboxCompressionSession::open(&VideoToolboxCompressionSessionRequest {
            elementary_stream: VideoCodecElementaryStream::H264,
            width: REFERENCE_WIDTH,
            height: REFERENCE_HEIGHT,
            frames_per_second: FRAMES_PER_SECOND,
            keyframe_interval_seconds: KEYFRAME_INTERVAL_SECONDS,
            bitrate_bps: None,
            color_vui: color_vui_encoded_for_the_test_pattern(),
        })
        .expect("a hardware compression session");
    let sync_point = encode_the_reference(&mut compression_session, &reference).remove(0);

    let decode_under = |maximum_height: u32| {
        VideoToolboxDecompressionSession::open(
            VideoCodecElementaryStream::H264,
            Some((REFERENCE_WIDTH, maximum_height)),
        )
        .expect("a decompression session")
        .decode_annex_b_access_unit(&sync_point.annex_b_access_unit_bytes)
        .map(|decoded_pictures| decoded_pictures.len())
    };

    let refusal = decode_under(REFERENCE_HEIGHT - 1)
        .expect_err("a 1079-line cap is below the stream's 1080-line pictures");
    assert!(refusal.contains("1920x1080"), "{refusal}");
    assert_eq!(
        decode_under(REFERENCE_HEIGHT).expect("a cap at the picture's extent admits the stream"),
        1
    );
}

// The measured scores are the run's evidence, printed for whoever ran it.
#[allow(clippy::disallowed_macros)]
fn round_trip_every_psnr_reference(elementary_stream: VideoCodecElementaryStream) {
    let mut compression_session =
        VideoToolboxCompressionSession::open(&VideoToolboxCompressionSessionRequest {
            elementary_stream,
            width: REFERENCE_WIDTH,
            height: REFERENCE_HEIGHT,
            frames_per_second: FRAMES_PER_SECOND,
            keyframe_interval_seconds: KEYFRAME_INTERVAL_SECONDS,
            bitrate_bps: None,
            color_vui: color_vui_encoded_for_the_test_pattern(),
        })
        .expect("a hardware compression session");
    let mut decompression_session = VideoToolboxDecompressionSession::open(elementary_stream, None)
        .expect("a hardware decompression session");
    let coded_extent = compression_session.coded_extent();

    let mut failures = Vec::new();
    for reference_path in psnr_reference_paths() {
        let reference_name = reference_path
            .file_stem()
            .expect("a reference file name")
            .to_string_lossy()
            .to_string();
        let reference = Rgba8Picture::read_png(&reference_path);
        let mut last_picture_of_this_reference: Option<DecodedVideoToolboxPicture> = None;

        for frame_of_reference in 0..FRAMES_PER_REFERENCE {
            let access_units = encode_the_reference(&mut compression_session, &reference);
            assert_eq!(
                access_units.len(),
                1,
                "{reference_name}: with reordering off, every frame completes one access unit"
            );
            let access_unit = &access_units[0];
            if frame_of_reference == 0 {
                assert!(
                    access_unit.is_sync_point,
                    "{reference_name}: a reference run opens a new second, so a sync point"
                );
            }
            assert_the_access_unit_is_the_linux_wire(
                elementary_stream,
                access_unit,
                coded_extent,
                &reference_name,
            );

            let mut decoded_pictures = decompression_session
                .decode_annex_b_access_unit(&access_unit.annex_b_access_unit_bytes)
                .expect("the access unit decodes");
            assert_eq!(
                decoded_pictures.len(),
                1,
                "{reference_name}: every access unit decodes to one picture"
            );
            last_picture_of_this_reference = decoded_pictures.pop();
        }

        let picture = last_picture_of_this_reference.expect("a decoded picture");
        assert_eq!(
            (picture.width, picture.height),
            (REFERENCE_WIDTH, REFERENCE_HEIGHT),
            "{reference_name}: the decoder hands back the conformance window, not the coded extent"
        );
        let decoded = rgba_of_the_decoded_picture(&picture);
        if let Some(png_directory) = std::env::var_os("STREAMLIB_VIDEOTOOLBOX_ROUND_TRIP_PNG_DIR") {
            decoded.write_png(&Path::new(&png_directory).join(format!(
                "{}__{reference_name}.png",
                format!("{elementary_stream:?}").to_lowercase()
            )));
        }
        let scored = Yuv420PlanePsnr::between(&decoded.rgba, &reference);
        eprintln!(
            "VideoToolbox {elementary_stream:?} {reference_name}: Y {:.2} dB, U {:.2} dB, V {:.2} dB",
            scored.luma_db, scored.blue_difference_db, scored.red_difference_db
        );
        if scored.fails_the_bands() {
            failures.push(format!("{reference_name}: {scored:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{elementary_stream:?} decodes below the {LUMA_PSNR_FAIL_FLOOR_DB} dB Y / \
         {CHROMA_PSNR_FAIL_FLOOR_DB} dB chroma bands: {failures:#?}"
    );
}

/// Decode the clip VideoToolbox wrote for `elementary_stream`, holding every
/// access unit to one picture and the last picture to the bands.
fn decode_the_checked_in_clip_inside_the_bands(elementary_stream: VideoCodecElementaryStream) {
    let access_units =
        read_access_unit_clip(&videotoolbox_cross_floor_clip_path(elementary_stream));
    let mut decompression_session = VideoToolboxDecompressionSession::open(elementary_stream, None)
        .expect("a hardware decompression session");
    let mut decoded_pictures = Vec::new();
    for access_unit in &access_units {
        decoded_pictures.extend(
            decompression_session
                .decode_annex_b_access_unit(access_unit)
                .expect("the access unit decodes"),
        );
    }
    assert_eq!(
        decoded_pictures.len(),
        access_units.len(),
        "every access unit of the clip decodes to one picture"
    );
    let last_picture = decoded_pictures.last().expect("a decoded picture");
    assert_eq!(
        (last_picture.width, last_picture.height),
        (CROSS_FLOOR_CLIP_WIDTH, CROSS_FLOOR_CLIP_HEIGHT)
    );
    let scored = Yuv420PlanePsnr::between(
        &rgba_of_the_decoded_picture(last_picture).rgba,
        &cross_floor_clip_reference_picture(),
    );
    assert!(
        !scored.fails_the_bands(),
        "{elementary_stream:?}: {scored:?}"
    );
}

/// What `TestPatternSource` stamps its RGBA frames with.
fn test_pattern_color_vui() -> H273ColorVui {
    H273ColorVui {
        primaries: Some(primaries::BT709),
        transfer: Some(transfer::SRGB),
        matrix: None,
        full_range: Some(true),
    }
}

/// What an encoder writes for a [`test_pattern_color_vui`] frame: its
/// primaries and transfer, the BT.709 matrix, limited range.
fn color_vui_encoded_for_the_test_pattern() -> H273ColorVui {
    color_vui_every_engine_encoder_writes(Some(test_pattern_color_vui()))
}

/// Write `reference` into the next pool pixel buffer and code it.
fn encode_the_reference(
    compression_session: &mut VideoToolboxCompressionSession,
    reference: &Rgba8Picture,
) -> Vec<EncodedVideoAccessUnit> {
    let pool_pixel_buffer = compression_session
        .pixel_buffer_from_the_session_pool()
        .expect("a pixel buffer from the session's pool");
    write_rgba_as_bt709_video_range_nv12(reference, &pool_pixel_buffer);
    compression_session
        .encode_pool_pixel_buffer(&pool_pixel_buffer, false)
        .expect("the frame encodes")
}

/// The IOSurface behind `pixel_buffer`, locked with `lock_options` while
/// `use_the_planes` runs.
fn with_the_locked_iosurface_of<T>(
    pixel_buffer: &CVPixelBuffer,
    lock_options: IOSurfaceLockOptions,
    use_the_planes: impl FnOnce(&IOSurfaceRef) -> T,
) -> T {
    let iosurface = CVPixelBufferGetIOSurface(Some(pixel_buffer))
        .expect("the pixel buffer is IOSurface-backed");
    assert_eq!(iosurface.plane_count(), 2, "NV12 has two planes");
    // SAFETY: a live surface; a null seed is allowed.
    let locked = unsafe { iosurface.lock(lock_options, std::ptr::null_mut()) };
    assert_eq!(locked, 0, "the IOSurface locks");
    let answer = use_the_planes(&iosurface);
    // SAFETY: the surface was locked above with the same options.
    unsafe { iosurface.unlock(lock_options, std::ptr::null_mut()) };
    answer
}

/// `reference` written into `pixel_buffer` as BT.709 limited-range NV12 with
/// 2×2-averaged chroma, its last row and column repeated past its extent.
fn write_rgba_as_bt709_video_range_nv12(reference: &Rgba8Picture, pixel_buffer: &CVPixelBuffer) {
    let (reference_width, reference_height) = (reference.width as usize, reference.height as usize);
    let rgba_at = |column: usize, row: usize| -> [f64; 3] {
        let index =
            (row.min(reference_height - 1) * reference_width + column.min(reference_width - 1)) * 4;
        [0, 1, 2].map(|channel| f64::from(reference.rgba[index + channel]))
    };
    let luma_of = |[red, green, blue]: [f64; 3]| {
        BT709_RED_WEIGHT * red + BT709_GREEN_WEIGHT * green + BT709_BLUE_WEIGHT * blue
    };
    let to_eight_bits = |value: f64| value.round().clamp(0.0, 255.0) as u8;
    with_the_locked_iosurface_of(pixel_buffer, IOSurfaceLockOptions::empty(), |iosurface| {
        let (luma_width, luma_height) = (iosurface.width_of_plane(0), iosurface.height_of_plane(0));
        let luma_row_bytes = iosurface.bytes_per_row_of_plane(0);
        let luma_plane = iosurface.base_address_of_plane(0).as_ptr().cast::<u8>();
        for row in 0..luma_height {
            for column in 0..luma_width {
                let luma = luma_of(rgba_at(column, row));
                // SAFETY: inside the locked luma plane's rows.
                unsafe {
                    *luma_plane.add(row * luma_row_bytes + column) =
                        to_eight_bits(16.0 + luma * 219.0 / 255.0);
                }
            }
        }
        let (chroma_width, chroma_height) =
            (iosurface.width_of_plane(1), iosurface.height_of_plane(1));
        let chroma_row_bytes = iosurface.bytes_per_row_of_plane(1);
        let chroma_plane = iosurface.base_address_of_plane(1).as_ptr().cast::<u8>();
        for chroma_row in 0..chroma_height {
            for chroma_column in 0..chroma_width {
                let (mut blue_difference_sum, mut red_difference_sum) = (0.0, 0.0);
                for (column_offset, row_offset) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let rgb = rgba_at(
                        chroma_column * 2 + column_offset,
                        chroma_row * 2 + row_offset,
                    );
                    let luma = luma_of(rgb);
                    blue_difference_sum += (rgb[2] - luma) / BT709_BLUE_DIFFERENCE_SCALE;
                    red_difference_sum += (rgb[0] - luma) / BT709_RED_DIFFERENCE_SCALE;
                }
                let offset = chroma_row * chroma_row_bytes + chroma_column * 2;
                // SAFETY: inside the locked chroma plane's rows.
                unsafe {
                    *chroma_plane.add(offset) =
                        to_eight_bits(128.0 + blue_difference_sum / 4.0 * 224.0 / 255.0);
                    *chroma_plane.add(offset + 1) =
                        to_eight_bits(128.0 + red_difference_sum / 4.0 * 224.0 / 255.0);
                }
            }
        }
    });
}

/// The decoded picture's display window as RGBA8, read from its BT.709 NV12
/// at the range its pixel format names.
fn rgba_of_the_decoded_picture(picture: &DecodedVideoToolboxPicture) -> Rgba8Picture {
    let video_range = match CVPixelBufferGetPixelFormatType(&picture.pixel_buffer) {
        CORE_VIDEO_NV12_VIDEO_RANGE => true,
        CORE_VIDEO_NV12_FULL_RANGE => false,
        other => panic!(
            "the decoder handed back pixel format {:?}, not 8-bit NV12",
            other.to_be_bytes().map(char::from)
        ),
    };
    let (width, height) = (picture.width as usize, picture.height as usize);
    let rgba = with_the_locked_iosurface_of(
        &picture.pixel_buffer,
        IOSurfaceLockOptions::ReadOnly,
        |iosurface| {
            let luma_row_bytes = iosurface.bytes_per_row_of_plane(0);
            let luma_plane = iosurface.base_address_of_plane(0).as_ptr().cast::<u8>();
            let chroma_row_bytes = iosurface.bytes_per_row_of_plane(1);
            let chroma_plane = iosurface.base_address_of_plane(1).as_ptr().cast::<u8>();
            let mut rgba = Vec::with_capacity(width * height * 4);
            for row in 0..height {
                for column in 0..width {
                    let chroma_offset = (row / 2) * chroma_row_bytes + (column / 2) * 2;
                    // SAFETY: inside the locked planes; the display window
                    // lies inside the coded planes.
                    let (luma, blue_difference, red_difference) = unsafe {
                        (
                            f64::from(*luma_plane.add(row * luma_row_bytes + column)),
                            f64::from(*chroma_plane.add(chroma_offset)),
                            f64::from(*chroma_plane.add(chroma_offset + 1)),
                        )
                    };
                    let (luma, blue_difference, red_difference) = if video_range {
                        (
                            (luma - 16.0) * 255.0 / 219.0,
                            (blue_difference - 128.0) * 255.0 / 224.0,
                            (red_difference - 128.0) * 255.0 / 224.0,
                        )
                    } else {
                        (luma, blue_difference - 128.0, red_difference - 128.0)
                    };
                    let red = luma + BT709_RED_DIFFERENCE_SCALE * red_difference;
                    let blue = luma + BT709_BLUE_DIFFERENCE_SCALE * blue_difference;
                    let green = (luma - BT709_RED_WEIGHT * red - BT709_BLUE_WEIGHT * blue)
                        / BT709_GREEN_WEIGHT;
                    rgba.extend(
                        [red, green, blue].map(|channel| channel.round().clamp(0.0, 255.0) as u8),
                    );
                    rgba.push(255);
                }
            }
            rgba
        },
    );
    Rgba8Picture {
        width: picture.width,
        height: picture.height,
        rgba,
    }
}

/// Hold one access unit to the wire the Linux encoder publishes.
fn assert_the_access_unit_is_the_linux_wire(
    elementary_stream: VideoCodecElementaryStream,
    access_unit: &EncodedVideoAccessUnit,
    coded_extent: (u32, u32),
    reference_name: &str,
) {
    let bytes = &access_unit.annex_b_access_unit_bytes;
    assert_eq!(
        bytes.get(..4),
        Some(&[0u8, 0, 0, 1][..]),
        "{reference_name}: an access unit opens on a four-byte start code"
    );
    let nal_units = nal_units_of(bytes);
    let nal_unit_types: Vec<u8> = nal_units
        .iter()
        .map(|nal_unit| nal_unit_type(elementary_stream, nal_unit))
        .collect();
    for nal_unit in &nal_units {
        assert_eq!(
            nal_unit[0] & 0x80,
            0,
            "{reference_name}: forbidden_zero_bit is set — a length prefix leaked into the \
             Annex-B stream? types {nal_unit_types:?}"
        );
    }
    let parameter_set_types: &[u8] = match elementary_stream {
        VideoCodecElementaryStream::H264 => &[7, 8],
        VideoCodecElementaryStream::H265 => &[32, 33, 34],
    };
    let leading_parameter_sets: Vec<u8> = nal_unit_types
        .iter()
        .copied()
        .take_while(|nal_type| parameter_set_types.contains(nal_type))
        .collect();
    if access_unit.is_sync_point {
        assert_eq!(
            leading_parameter_sets, parameter_set_types,
            "{reference_name}: a sync point carries every parameter set in front, in order; \
             types {nal_unit_types:?}"
        );
        let sequence_parameter_set = nal_units[parameter_set_types.len() - 2];
        let (stated_coded_extent, stated_color_vui) =
            coded_extent_and_colour_stated_by(elementary_stream, sequence_parameter_set);
        assert_eq!(
            stated_coded_extent, coded_extent,
            "{reference_name}: the session reports the coded extent its own SPS states"
        );
        assert_eq!(
            stated_color_vui,
            color_vui_encoded_for_the_test_pattern(),
            "{reference_name}: the SPS VUI states the BT.709 limited-range samples the \
             encoder wrote for a full-range source naming no matrix"
        );
    } else {
        assert!(
            nal_unit_types
                .iter()
                .all(|nal_type| !parameter_set_types.contains(nal_type)),
            "{reference_name}: parameter sets ride sync points only; types {nal_unit_types:?}"
        );
    }
}

fn nal_units_of(annex_b: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 <= annex_b.len() {
        if annex_b[index..index + 3] == [0, 0, 1] {
            starts.push(index + 3);
            index += 3;
        } else {
            index += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(position, &start)| {
            let end = starts
                .get(position + 1)
                .map_or(annex_b.len(), |&next| next - 3);
            let nal_unit = &annex_b[start..end];
            let meaningful = nal_unit
                .iter()
                .rposition(|&byte| byte != 0)
                .map_or(0, |last| last + 1);
            &nal_unit[..meaningful]
        })
        .collect()
}

fn nal_unit_type(elementary_stream: VideoCodecElementaryStream, nal_unit: &[u8]) -> u8 {
    match elementary_stream {
        VideoCodecElementaryStream::H264 => nal_unit[0] & 0x1F,
        VideoCodecElementaryStream::H265 => (nal_unit[0] >> 1) & 0x3F,
    }
}

/// The coded extent and the VUI colour a sequence parameter set states.
fn coded_extent_and_colour_stated_by(
    elementary_stream: VideoCodecElementaryStream,
    sequence_parameter_set: &[u8],
) -> ((u32, u32), H273ColorVui) {
    match elementary_stream {
        VideoCodecElementaryStream::H264 => {
            h264_coded_extent_and_colour_stated_by(sequence_parameter_set)
                .expect("the H.264 SPS reads through its VUI")
        }
        VideoCodecElementaryStream::H265 => {
            h265_coded_extent_and_colour_stated_by(sequence_parameter_set)
        }
    }
}

fn h265_coded_extent_and_colour_stated_by(
    sequence_parameter_set: &[u8],
) -> ((u32, u32), H273ColorVui) {
    let raw_byte_sequence_payload = remove_emulation_prevention_bytes(&sequence_parameter_set[2..]);
    let parsed_h265_sequence_parameter_set = parse_h265_sequence_parameter_set(
        &mut RbspBitstreamReader::new(&raw_byte_sequence_payload),
    )
    .expect("the H.265 SPS parses");
    let vui = &parsed_h265_sequence_parameter_set.vui;
    let colour_description_present = vui.flags.colour_description_present_flag;
    let stated_color_vui = if vui.flags.video_signal_type_present_flag {
        H273ColorVui {
            primaries: colour_description_present.then_some(vui.colour_primaries),
            transfer: colour_description_present.then_some(vui.transfer_characteristics),
            matrix: colour_description_present.then_some(vui.matrix_coeffs),
            full_range: Some(vui.flags.video_full_range_flag),
        }
    } else {
        H273ColorVui::default()
    };
    (
        (
            parsed_h265_sequence_parameter_set.pic_width_in_luma_samples,
            parsed_h265_sequence_parameter_set.pic_height_in_luma_samples,
        ),
        stated_color_vui,
    )
}

/// ITU-T H.264 §7.3.2.1.1 read through the VUI's `video_signal_type`.
fn h264_coded_extent_and_colour_stated_by(
    sequence_parameter_set: &[u8],
) -> Option<((u32, u32), H273ColorVui)> {
    let raw_byte_sequence_payload = remove_emulation_prevention_bytes(&sequence_parameter_set[1..]);
    let mut bits = RbspBitstreamReader::new(&raw_byte_sequence_payload);
    let profile_idc = bits.u(8)?;
    bits.u(16)?; // constraint flags, level_idc
    bits.ue()?; // seq_parameter_set_id
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile_idc) {
        if bits.ue()? == 3 {
            bits.u(1)?; // separate_colour_plane_flag
        }
        bits.ue()?; // bit_depth_luma_minus8
        bits.ue()?; // bit_depth_chroma_minus8
        bits.u(1)?; // qpprime_y_zero_transform_bypass_flag
        assert_eq!(bits.u(1)?, 0, "scaling matrices are not read here");
    }
    bits.ue()?; // log2_max_frame_num_minus4
    match bits.ue()? {
        0 => {
            bits.ue()?;
        }
        1 => {
            bits.u(1)?;
            bits.se()?;
            bits.se()?;
            for _ in 0..bits.ue()? {
                bits.se()?;
            }
        }
        _ => {}
    }
    bits.ue()?; // max_num_ref_frames
    bits.u(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_in_macroblocks = bits.ue()? + 1;
    let height_in_map_units = bits.ue()? + 1;
    let frame_mbs_only = bits.u(1)?;
    let coded_extent = (
        width_in_macroblocks * 16,
        height_in_map_units * 16 * (2 - frame_mbs_only),
    );

    if frame_mbs_only == 0 {
        bits.u(1)?; // mb_adaptive_frame_field_flag
    }
    bits.u(1)?; // direct_8x8_inference_flag
    if bits.u(1)? == 1 {
        for _ in 0..4 {
            bits.ue()?; // frame_crop_{left,right,top,bottom}_offset
        }
    }
    let vui_parameters_present = bits.u(1)? == 1;
    if !vui_parameters_present {
        return Some((coded_extent, H273ColorVui::default()));
    }
    if bits.u(1)? == 1 && bits.u(8)? == 255 {
        bits.u(32)?; // sar_width, sar_height
    }
    if bits.u(1)? == 1 {
        bits.u(1)?; // overscan_appropriate_flag
    }
    let video_signal_type_present = bits.u(1)? == 1;
    if !video_signal_type_present {
        return Some((coded_extent, H273ColorVui::default()));
    }
    bits.u(3)?; // video_format
    let full_range = bits.u(1)? == 1;
    let stated_color_vui = if bits.u(1)? == 1 {
        H273ColorVui {
            primaries: Some(bits.u(8)? as u8),
            transfer: Some(bits.u(8)? as u8),
            matrix: Some(bits.u(8)? as u8),
            full_range: Some(full_range),
        }
    } else {
        H273ColorVui {
            full_range: Some(full_range),
            ..H273ColorVui::default()
        }
    };
    Some((coded_extent, stated_color_vui))
}
