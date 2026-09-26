// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Vulkan Video arm decodes what the VideoToolbox arm encoded: the
//! checked-in clips a Mac's encoder wrote (`regenerate_the_cross_floor_clips`
//! in `videotoolbox_arm_round_trips_the_psnr_references`) go through the
//! Linux decoder one access unit at a time, and every one comes back as a
//! picture at the clip's conformance extent, inside the PSNR bands. The wire
//! is one wire on both floors, proven by bytes rather than by shape.
//!
//! Rig tier: it needs a device with Vulkan Video decode queues.

#![cfg(target_os = "linux")]

use std::sync::OnceLock;

use streamlib::sdk::context::{GpuContext, VideoCodecElementaryStream};

#[path = "support/codec_round_trip_scoring.rs"]
mod codec_round_trip_scoring;

use codec_round_trip_scoring::decode_the_checked_in_videotoolbox_clip_inside_the_bands;

fn gpu_context() -> &'static GpuContext {
    static GPU_CONTEXT: OnceLock<GpuContext> = OnceLock::new();
    GPU_CONTEXT.get_or_init(|| GpuContext::init_for_platform().expect("a Vulkan device"))
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video decode queues; see docs/testing-hardware.md"
)]
fn vulkan_video_decodes_the_h264_clip_videotoolbox_encoded() {
    decode_the_checked_in_videotoolbox_clip_inside_the_bands(
        gpu_context(),
        VideoCodecElementaryStream::H264,
    );
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video decode queues; see docs/testing-hardware.md"
)]
fn vulkan_video_decodes_the_h265_clip_videotoolbox_encoded() {
    decode_the_checked_in_videotoolbox_clip_inside_the_bands(
        gpu_context(),
        VideoCodecElementaryStream::H265,
    );
}
