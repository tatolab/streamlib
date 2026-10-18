// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video codec seam's force-sync-point flag on real hardware, on whichever
//! arm this floor probes: a frame asking for a sync point mid-GOP is one, the
//! frames either side are not, and every sync point carries an SPS spelling
//! the codec string a catalog entry names.
//!
//! Rig tier — it needs a hardware encoder, so Cargo builds it only under
//! `hardware-tests`.

#![cfg(any(target_os = "linux", target_os = "macos"))]

#[path = "support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;

use std::sync::OnceLock;

use streamlib::sdk::context::{
    GpuContext, VideoCodecElementaryStream, VideoEncodeKnobs, VideoEncodeSessionRequest,
    VideoEncodeSourceSurface, probe_video_codec_backend,
};
use streamlib::sdk::rhi::PixelFormat;
use streamlib::sdk::video_bitstream::parameter_sets_of_annex_b_access_unit;

const SOURCE_WIDTH: u32 = 1280;
const SOURCE_HEIGHT: u32 = 720;
const FRAMES_PER_SECOND: u32 = 10;

/// Long enough that no sync point but the first and the forced one falls
/// inside the run.
const KEYFRAME_INTERVAL_SECONDS: u32 = 10;

const FRAMES_ENCODED: usize = 6;
const FRAME_FORCED_TO_A_SYNC_POINT: usize = 3;

fn gpu_context() -> &'static GpuContext {
    static GPU_CONTEXT: OnceLock<GpuContext> = OnceLock::new();
    GPU_CONTEXT.get_or_init(|| GpuContext::init_for_platform().expect("a GPU device"))
}

#[cfg(target_os = "linux")]
fn this_machine_encodes_vulkan_video() -> bool {
    streamlib::sdk::engine::host_rhi::HostVulkanDevice::new()
        .expect("a Vulkan device comes up on this machine")
        .supports_video_encode()
}

/// The sync-point flag of every access unit `FRAMES_ENCODED` frames yield,
/// and the codec string the first one's SPS spells.
fn encode_forcing_one_sync_point(
    elementary_stream: VideoCodecElementaryStream,
) -> (Vec<bool>, Option<String>) {
    let backend = probe_video_codec_backend();
    let gpu = gpu_context().limited_access();
    let mut encode_session = gpu
        .escalate(|full| {
            backend.open_encode_session(
                full,
                &VideoEncodeSessionRequest {
                    elementary_stream,
                    width: SOURCE_WIDTH,
                    height: SOURCE_HEIGHT,
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

    let mut sync_point_flags = Vec::new();
    let mut first_codec_string = None;
    for frame_index in 0..FRAMES_ENCODED {
        let (surface_id, source_pixel_buffer) = gpu
            .acquire_pixel_buffer(SOURCE_WIDTH, SOURCE_HEIGHT, PixelFormat::Rgba32)
            .expect("a pooled source frame");
        let mid_grey = vec![0x80u8; (SOURCE_WIDTH * SOURCE_HEIGHT * 4) as usize];
        source_pixel_buffer
            .write_this_plane_from(0, &mid_grey)
            .expect("the source frame staged");
        let access_units = encode_session
            .encode_published_surface(&VideoEncodeSourceSurface {
                surface_id: &surface_id.to_string(),
                texture_layout: None,
                width: SOURCE_WIDTH,
                height: SOURCE_HEIGHT,
                timestamp_ns: 1_000_000_000 + frame_index as i64 * 100_000_000,
                force_sync_point: frame_index == FRAME_FORCED_TO_A_SYNC_POINT,
            })
            .expect("the frame encodes");
        for access_unit in access_units {
            if access_unit.is_sync_point && first_codec_string.is_none() {
                first_codec_string = parameter_sets_of_annex_b_access_unit(
                    &access_unit.annex_b_access_unit_bytes,
                    elementary_stream,
                )
                .codec_string_of_the_sequence_parameter_set(elementary_stream);
            }
            sync_point_flags.push(access_unit.is_sync_point);
        }
    }
    (sync_point_flags, first_codec_string)
}

fn assert_the_forced_frame_alone_joins_the_first_as_a_sync_point(
    elementary_stream: VideoCodecElementaryStream,
    expected_codec_string_prefix: &str,
) {
    #[cfg(target_os = "linux")]
    if rig_only_capability_skip::skip_unless_this_machine_has(
        rig_only_capability_skip::RigOnlyCapability::VulkanVideo,
        this_machine_encodes_vulkan_video(),
    ) {
        return;
    }
    let (sync_point_flags, codec_string) = encode_forcing_one_sync_point(elementary_stream);
    let expected_sync_point_flags: Vec<bool> = (0..FRAMES_ENCODED)
        .map(|frame_index| frame_index == 0 || frame_index == FRAME_FORCED_TO_A_SYNC_POINT)
        .collect();
    assert_eq!(
        sync_point_flags, expected_sync_point_flags,
        "{elementary_stream:?}: one access unit per frame, sync points at the first frame and \
         the forced one only"
    );
    let codec_string = codec_string.expect("the first sync point carries an SPS");
    assert!(
        codec_string.starts_with(expected_codec_string_prefix),
        "{elementary_stream:?}: the SPS spells `{codec_string}`, expected \
         `{expected_codec_string_prefix}…`"
    );
}

/// Both arms encode H.264 at High, so the profile byte is `0x64`.
#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn h264_sync_point_forced_mid_gop_lands_on_the_frame_that_asked() {
    assert_the_forced_frame_alone_joins_the_first_as_a_sync_point(
        VideoCodecElementaryStream::H264,
        "avc1.64",
    );
}

/// Both arms encode H.265 at Main, profile 1.
#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn h265_sync_point_forced_mid_gop_lands_on_the_frame_that_asked() {
    assert_the_forced_frame_alone_joins_the_first_as_a_sync_point(
        VideoCodecElementaryStream::H265,
        "hvc1.1.",
    );
}
