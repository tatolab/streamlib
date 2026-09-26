// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The VideoToolbox arm of the video codec seam, encode to decode on real
//! hardware: each checked-in PSNR reference goes in as a published surface,
//! comes back out of the decoder as a pooled picture, and scores inside the
//! bands the Linux round-trip proof holds its decodes to — Y at 30 dB or
//! better, both chroma planes at 30 dB or better, over BT.709 full-range
//! 4:2:0 planes, the measurement `cargo xtask psnr score` takes.
//!
//! The access units in between are held to the wire the Linux arm speaks:
//! Annex-B with four-byte start codes, parameter sets in front of every sync
//! point and nowhere else, and a coded extent that is the one the stream's own
//! SPS states.
//!
//! The same arm wrote the checked-in cross-floor clips the Linux rig decodes
//! through Vulkan Video (`vulkan_video_decodes_what_videotoolbox_encoded`);
//! this file decodes them too, and regenerates them on request.
//!
//! Rig tier — it needs Apple's hardware encoder and decoder, which a virtual
//! machine does not expose, so Cargo builds it only under `hardware-tests`.
//! `STREAMLIB_VIDEOTOOLBOX_ROUND_TRIP_PNG_DIR` names a directory each scored
//! decode is written into, as `<codec>__<reference>.png`.

#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::OnceLock;

use streamlib::sdk::color::H273ColorVui;
use streamlib::sdk::context::{
    DecodedVideoPictureInPooledPixelBuffer, EncodedVideoAccessUnitFromSession, GpuContext,
    VideoCodecElementaryStream, VideoDecodeMaximumCodedExtent, VideoDecodeSessionRequest,
    VideoEncodeKnobs, VideoEncodeSessionRequest, VideoEncodeSourceSurface,
    probe_video_codec_backend,
};
use streamlib::sdk::rhi::PixelFormat;

#[path = "support/codec_round_trip_scoring.rs"]
mod codec_round_trip_scoring;

use codec_round_trip_scoring::{
    CHROMA_PSNR_FAIL_FLOOR_DB, CROSS_FLOOR_CLIP_HEIGHT, CROSS_FLOOR_CLIP_WIDTH,
    LUMA_PSNR_FAIL_FLOOR_DB, Rgba8Picture, Yuv420PlanePsnr, cross_floor_clip_reference_picture,
    decode_the_checked_in_videotoolbox_clip_inside_the_bands, psnr_reference_paths,
    videotoolbox_cross_floor_clip_path, write_access_unit_clip,
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

/// Nanoseconds between frames at [`FRAMES_PER_SECOND`].
const FRAME_INTERVAL_NS: i64 = 1_000_000_000 / FRAMES_PER_SECOND as i64;

fn gpu_context() -> &'static GpuContext {
    static GPU_CONTEXT: OnceLock<GpuContext> = OnceLock::new();
    GPU_CONTEXT
        .get_or_init(|| GpuContext::init_for_platform().expect("a Vulkan device on MoltenVK"))
}

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

/// The frames each cross-floor clip holds: a sync point, four frames that
/// reference it, then a second sync point — so a decoder must both enter the
/// stream and re-enter it.
const CROSS_FLOOR_CLIP_FRAMES: u32 = 6;
const CROSS_FLOOR_CLIP_FRAMES_PER_SECOND: u32 = 5;

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn the_checked_in_h264_clip_decodes_inside_the_bands() {
    decode_the_checked_in_videotoolbox_clip_inside_the_bands(
        gpu_context(),
        VideoCodecElementaryStream::H264,
    );
}

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn the_checked_in_h265_clip_decodes_inside_the_bands() {
    decode_the_checked_in_videotoolbox_clip_inside_the_bands(
        gpu_context(),
        VideoCodecElementaryStream::H265,
    );
}

/// Rewrites the checked-in cross-floor clips from this machine's encoder.
/// Run on purpose, never by a sweep:
/// `cargo test -p streamlib-engine --features hardware-tests --test
/// videotoolbox_arm_round_trips_the_psnr_references -- --ignored
/// regenerate_the_cross_floor_clips`.
#[test]
#[ignore = "rewrites checked-in fixtures; run by name"]
fn regenerate_the_cross_floor_clips() {
    let backend = probe_video_codec_backend();
    let gpu = gpu_context().limited_access();
    let reference = cross_floor_clip_reference_picture();
    for elementary_stream in [
        VideoCodecElementaryStream::H264,
        VideoCodecElementaryStream::H265,
    ] {
        let mut encode_session = gpu
            .escalate(|full| {
                backend.open_encode_session(
                    full,
                    &VideoEncodeSessionRequest {
                        elementary_stream,
                        width: CROSS_FLOOR_CLIP_WIDTH,
                        height: CROSS_FLOOR_CLIP_HEIGHT,
                        frames_per_second: CROSS_FLOOR_CLIP_FRAMES_PER_SECOND,
                        knobs: VideoEncodeKnobs {
                            bitrate_bps: None,
                            keyframe_interval_seconds: 1,
                            effort_level: None,
                        },
                        color_vui: Some(test_pattern_color_vui()),
                    },
                )
            })
            .expect("a hardware encode session");
        let mut access_units = Vec::new();
        for frame_index in 0..CROSS_FLOOR_CLIP_FRAMES {
            let (surface_id, source_pixel_buffer) = gpu
                .acquire_pixel_buffer(
                    CROSS_FLOOR_CLIP_WIDTH,
                    CROSS_FLOOR_CLIP_HEIGHT,
                    PixelFormat::Rgba32,
                )
                .expect("a pooled source frame");
            source_pixel_buffer
                .write_this_plane_from(0, &reference.rgba)
                .expect("the reference staged into the source frame");
            access_units.extend(
                encode_session
                    .encode_published_surface(&VideoEncodeSourceSurface {
                        surface_id: &surface_id.to_string(),
                        texture_layout: None,
                        width: CROSS_FLOOR_CLIP_WIDTH,
                        height: CROSS_FLOOR_CLIP_HEIGHT,
                        timestamp_ns: i64::from(frame_index + 1),
                    })
                    .expect("the frame encodes")
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

/// What `TestPatternSource` stamps its RGBA frames with.
fn test_pattern_color_vui() -> H273ColorVui {
    H273ColorVui {
        primaries: Some(1),
        transfer: Some(13),
        matrix: None,
        full_range: Some(true),
    }
}

/// CoreMedia reports a decoded stream's extents already cropped, so this arm
/// holds the decode request's cap against the picture: a 1080-line stream
/// passes a 1080-line cap and is refused, by name, by a 1079-line one.
#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn a_decode_cap_below_the_picture_is_refused_and_one_at_it_is_not() {
    let backend = probe_video_codec_backend();
    let gpu = gpu_context().limited_access();
    let reference = Rgba8Picture::read_png(&psnr_reference_paths()[0]);
    let mut encode_session = gpu
        .escalate(|full| {
            backend.open_encode_session(
                full,
                &VideoEncodeSessionRequest {
                    elementary_stream: VideoCodecElementaryStream::H264,
                    width: REFERENCE_WIDTH,
                    height: REFERENCE_HEIGHT,
                    frames_per_second: FRAMES_PER_SECOND,
                    knobs: VideoEncodeKnobs {
                        bitrate_bps: None,
                        keyframe_interval_seconds: KEYFRAME_INTERVAL_SECONDS,
                        effort_level: None,
                    },
                    color_vui: None,
                },
            )
        })
        .expect("a hardware encode session");
    let (surface_id, source_pixel_buffer) = gpu
        .acquire_pixel_buffer(REFERENCE_WIDTH, REFERENCE_HEIGHT, PixelFormat::Rgba32)
        .expect("a pooled source frame");
    source_pixel_buffer
        .write_this_plane_from(0, &reference.rgba)
        .expect("the reference staged into the source frame");
    let sync_point = encode_session
        .encode_published_surface(&VideoEncodeSourceSurface {
            surface_id: &surface_id.to_string(),
            texture_layout: None,
            width: REFERENCE_WIDTH,
            height: REFERENCE_HEIGHT,
            timestamp_ns: 1,
        })
        .expect("the frame encodes")
        .remove(0);

    let decode_under = |max_coded_height: u32| {
        let mut decode_session = gpu
            .escalate(|full| {
                backend.open_decode_session(
                    full,
                    &VideoDecodeSessionRequest {
                        elementary_stream: VideoCodecElementaryStream::H264,
                        maximum_coded_extent: Some(VideoDecodeMaximumCodedExtent {
                            max_coded_width: REFERENCE_WIDTH,
                            max_coded_height,
                        }),
                    },
                )
            })
            .expect("a decode session");
        let mut decoded_pictures = Vec::new();
        decode_session
            .decode_annex_b_access_unit(
                &sync_point.annex_b_access_unit_bytes,
                &mut decoded_pictures,
            )
            .map(|()| decoded_pictures.len())
    };

    let refusal = decode_under(REFERENCE_HEIGHT - 1)
        .expect_err("a 1079-line cap is below the stream's 1080-line pictures")
        .to_string();
    assert!(refusal.contains("1920x1080"), "{refusal}");
    assert_eq!(
        decode_under(REFERENCE_HEIGHT).expect("a cap at the picture's extent admits the stream"),
        1
    );
}

// The measured scores are the run's evidence, printed for whoever ran it.
#[allow(clippy::disallowed_macros)]
fn round_trip_every_psnr_reference(elementary_stream: VideoCodecElementaryStream) {
    let backend = probe_video_codec_backend();
    assert_eq!(backend.backend_name(), "videotoolbox");
    let gpu = gpu_context().limited_access();

    let mut encode_session = gpu
        .escalate(|full| {
            backend.open_encode_session(
                full,
                &VideoEncodeSessionRequest {
                    elementary_stream,
                    width: REFERENCE_WIDTH,
                    height: REFERENCE_HEIGHT,
                    frames_per_second: FRAMES_PER_SECOND,
                    knobs: VideoEncodeKnobs {
                        bitrate_bps: None,
                        keyframe_interval_seconds: KEYFRAME_INTERVAL_SECONDS,
                        effort_level: None,
                    },
                    color_vui: Some(test_pattern_color_vui()),
                },
            )
        })
        .expect("a hardware encode session");
    let mut decode_session = gpu
        .escalate(|full| {
            backend.open_decode_session(
                full,
                &VideoDecodeSessionRequest {
                    elementary_stream,
                    maximum_coded_extent: None,
                },
            )
        })
        .expect("a decode session");
    let coded_extent = encode_session.coded_extent();

    let mut failures = Vec::new();
    let mut frame_index: i64 = 0;
    for reference_path in psnr_reference_paths() {
        let reference_name = reference_path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let reference = Rgba8Picture::read_png(&reference_path);
        let mut last_picture_of_this_reference: Option<DecodedVideoPictureInPooledPixelBuffer> =
            None;

        for frame_of_reference in 0..FRAMES_PER_REFERENCE {
            let (surface_id, source_pixel_buffer) = gpu
                .acquire_pixel_buffer(REFERENCE_WIDTH, REFERENCE_HEIGHT, PixelFormat::Rgba32)
                .expect("a pooled source frame");
            source_pixel_buffer
                .write_this_plane_from(0, &reference.rgba)
                .expect("the reference staged into the source frame");
            let timestamp_ns = 1_000_000_000 + frame_index * FRAME_INTERVAL_NS;
            frame_index += 1;
            let access_units = encode_session
                .encode_published_surface(&VideoEncodeSourceSurface {
                    surface_id: &surface_id.to_string(),
                    texture_layout: None,
                    width: REFERENCE_WIDTH,
                    height: REFERENCE_HEIGHT,
                    timestamp_ns,
                })
                .expect("the frame encodes");
            assert_eq!(
                access_units.len(),
                1,
                "{reference_name}: with reordering off, every frame completes one access unit"
            );
            let access_unit = &access_units[0];
            assert_eq!(
                access_unit.timestamp_ns,
                Some(timestamp_ns),
                "{reference_name}: the access unit carries its source frame's stamp"
            );
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

            let mut decoded_pictures = Vec::new();
            decode_session
                .decode_annex_b_access_unit(
                    &access_unit.annex_b_access_unit_bytes,
                    &mut decoded_pictures,
                )
                .expect("the access unit decodes");
            assert_eq!(
                decoded_pictures.len(),
                1,
                "{reference_name}: every access unit decodes to one picture"
            );
            let picture = decoded_pictures.pop().expect("one picture");
            // The source frame is still held, so the pool cannot hand its slot
            // to the decoder: a decode that wrote nothing would score whatever
            // an earlier reference left in some other slot.
            assert_ne!(
                picture.published_pixel_buffer_frame_id.pool_slot_id(),
                surface_id.pool_slot_id(),
                "{reference_name}: the decoded picture lands in a pool slot of its own"
            );
            drop(source_pixel_buffer);
            last_picture_of_this_reference = Some(picture);
        }

        let picture = last_picture_of_this_reference.expect("a decoded picture");
        assert_eq!(
            (picture.width, picture.height),
            (REFERENCE_WIDTH, REFERENCE_HEIGHT),
            "{reference_name}: the decoder publishes the conformance window, not the coded extent"
        );
        // SAFETY: the pooled picture is held, host-readable once decode hands
        // it back, and exactly `width × height × 4` bytes.
        let decoded_rgba = unsafe {
            std::slice::from_raw_parts(
                picture.pixel_buffer.plane_base_address(0),
                REFERENCE_WIDTH as usize * REFERENCE_HEIGHT as usize * 4,
            )
        };
        if let Some(png_directory) = std::env::var_os("STREAMLIB_VIDEOTOOLBOX_ROUND_TRIP_PNG_DIR") {
            Rgba8Picture {
                width: REFERENCE_WIDTH,
                height: REFERENCE_HEIGHT,
                rgba: decoded_rgba.to_vec(),
            }
            .write_png(&Path::new(&png_directory).join(format!(
                "{}__{reference_name}.png",
                format!("{elementary_stream:?}").to_lowercase()
            )));
        }
        let scored = Yuv420PlanePsnr::between(decoded_rgba, &reference);
        eprintln!(
            "{elementary_stream:?} {reference_name}: Y {:.2} dB, U {:.2} dB, V {:.2} dB",
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

/// Hold one access unit to the wire the Linux arm publishes.
fn assert_the_access_unit_is_the_linux_wire(
    elementary_stream: VideoCodecElementaryStream,
    access_unit: &EncodedVideoAccessUnitFromSession,
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
        assert_eq!(
            coded_extent_stated_by(elementary_stream, sequence_parameter_set),
            coded_extent,
            "{reference_name}: the session reports the coded extent its own SPS states"
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

/// The coded extent a sequence parameter set states, read as far as its
/// picture dimensions.
fn coded_extent_stated_by(
    elementary_stream: VideoCodecElementaryStream,
    sequence_parameter_set: &[u8],
) -> (u32, u32) {
    match elementary_stream {
        VideoCodecElementaryStream::H264 => {
            let mut bits = RbspBitReader::over(&sequence_parameter_set[1..]);
            let profile_idc = bits.bits(8);
            bits.bits(16); // constraint flags, level_idc
            bits.exp_golomb(); // seq_parameter_set_id
            if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile_idc)
            {
                let chroma_format_idc = bits.exp_golomb();
                if chroma_format_idc == 3 {
                    bits.bits(1);
                }
                bits.exp_golomb(); // bit_depth_luma_minus8
                bits.exp_golomb(); // bit_depth_chroma_minus8
                bits.bits(1); // qpprime_y_zero_transform_bypass_flag
                assert_eq!(bits.bits(1), 0, "scaling matrices are not read here");
            }
            bits.exp_golomb(); // log2_max_frame_num_minus4
            match bits.exp_golomb() {
                0 => {
                    bits.exp_golomb();
                }
                1 => {
                    bits.bits(1);
                    bits.signed_exp_golomb();
                    bits.signed_exp_golomb();
                    for _ in 0..bits.exp_golomb() {
                        bits.signed_exp_golomb();
                    }
                }
                _ => {}
            }
            bits.exp_golomb(); // max_num_ref_frames
            bits.bits(1); // gaps_in_frame_num_value_allowed_flag
            let width_in_macroblocks = bits.exp_golomb() + 1;
            let height_in_map_units = bits.exp_golomb() + 1;
            let frame_mbs_only = bits.bits(1);
            (
                width_in_macroblocks * 16,
                height_in_map_units * 16 * (2 - frame_mbs_only),
            )
        }
        VideoCodecElementaryStream::H265 => {
            let mut bits = RbspBitReader::over(&sequence_parameter_set[2..]);
            bits.bits(4); // sps_video_parameter_set_id
            let max_sub_layers_minus1 = bits.bits(3);
            assert_eq!(max_sub_layers_minus1, 0, "sub-layer PTL is not read here");
            bits.bits(1); // sps_temporal_id_nesting_flag
            bits.bits(88); // general profile_tier_level
            bits.bits(8); // general_level_idc
            bits.exp_golomb(); // sps_seq_parameter_set_id
            if bits.exp_golomb() == 3 {
                bits.bits(1);
            }
            (bits.exp_golomb(), bits.exp_golomb())
        }
    }
}

/// Reads an RBSP's bits, emulation-prevention bytes removed.
struct RbspBitReader {
    bytes: Vec<u8>,
    bit_position: usize,
}

impl RbspBitReader {
    fn over(escaped: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(escaped.len());
        let mut zeros = 0;
        for &byte in escaped {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                continue;
            }
            zeros = if byte == 0 { zeros + 1 } else { 0 };
            bytes.push(byte);
        }
        Self {
            bytes,
            bit_position: 0,
        }
    }

    fn bits(&mut self, count: usize) -> u32 {
        let mut value: u64 = 0;
        for _ in 0..count {
            let byte = self.bytes[self.bit_position / 8];
            let bit = (byte >> (7 - self.bit_position % 8)) & 1;
            value = (value << 1) | u64::from(bit);
            self.bit_position += 1;
        }
        value as u32
    }

    fn exp_golomb(&mut self) -> u32 {
        let mut leading_zeros = 0;
        while self.bits(1) == 0 {
            leading_zeros += 1;
        }
        ((1u64 << leading_zeros) - 1 + u64::from(self.bits(leading_zeros))) as u32
    }

    fn signed_exp_golomb(&mut self) -> i32 {
        let code = self.exp_golomb() as i64;
        (if code % 2 == 1 {
            (code + 1) / 2
        } else {
            -code / 2
        }) as i32
    }
}
