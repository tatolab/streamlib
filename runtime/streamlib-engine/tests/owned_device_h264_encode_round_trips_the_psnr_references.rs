// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! H.264 encode on a device a module owns, scored as the engine's own encode
//! is: each checked-in PSNR reference is written into memory the runtime's
//! device exports, imported onto an owned Vulkan Video encode device on the
//! same GPU, encoded there, decoded back on the runtime's device, and scored
//! inside the bands — Y and both chroma planes at 30 dB or better over BT.709
//! full-range 4:2:0. `complex_pattern`'s chroma holds the 33.52 dB lock the
//! engine's encoder set.
//!
//! Rig tier — it needs Vulkan Video encode and decode queues, so Cargo builds
//! it only under `hardware-tests`.

#![cfg(target_os = "linux")]

#[path = "support/codec_round_trip_scoring.rs"]
mod codec_round_trip_scoring;
#[path = "support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::sync::{Arc, OnceLock};

use codec_round_trip_scoring::{
    CHROMA_PSNR_FAIL_FLOOR_DB, LUMA_PSNR_FAIL_FLOOR_DB, Rgba8Picture, Yuv420PlanePsnr,
    psnr_reference_paths,
};
use streamlib::sdk::color::H273ColorVui;
use streamlib::sdk::context::{
    GpuContext, VideoCodecElementaryStream, VideoDecodeSessionRequest, probe_video_codec_backend,
};
use streamlib::sdk::engine::HostGpuDeviceExt as _;
use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection,
};
use streamlib_engine::core::color::h273_color_vui::{primaries, transfer};
use streamlib_vulkan_video::{
    Codec, Preset, RgbaPixelBufferVideoEncoder, SimpleEncoder, SimpleEncoderConfig,
    VulkanVideoCodecDevice,
};

/// The references' extent; 1080 lines is not a multiple of the macroblock, so
/// the stream carries a conformance crop.
const REFERENCE_WIDTH: u32 = 1920;
const REFERENCE_HEIGHT: u32 = 1080;
const FRAMES_PER_SECOND: u32 = 10;
const KEYFRAME_INTERVAL_SECONDS: u32 = 1;
const FRAMES_PER_REFERENCE: u32 = FRAMES_PER_SECOND * KEYFRAME_INTERVAL_SECONDS;
const FRAME_INTERVAL_NS: i64 = 1_000_000_000 / FRAMES_PER_SECOND as i64;

/// The reference whose chroma carries the lowest clean figure in the set.
const CHROMA_LOCK_REFERENCE: &str = "complex_pattern";
/// The engine encoder's `complex_pattern` chroma figure, to two places.
const COMPLEX_PATTERN_CHROMA_LOCK_DB: f64 = 33.52;

fn gpu_context() -> &'static GpuContext {
    static GPU_CONTEXT: OnceLock<GpuContext> = OnceLock::new();
    GPU_CONTEXT.get_or_init(|| GpuContext::init_for_platform().expect("a Vulkan device"))
}

fn skip_unless_the_device_encodes_and_decodes_vulkan_video() -> bool {
    let vulkan_device = gpu_context().device().vulkan_device();
    rig_only_capability_skip::skip_unless_this_machine_has(
        rig_only_capability_skip::RigOnlyCapability::VulkanVideo,
        vulkan_device.supports_video_encode() && vulkan_device.supports_video_decode(),
    )
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

/// The encoder `H264Encoder`'s module mints for a reference-sized frame.
fn owned_device_h264_encoder(
    owned_device: &Arc<OwnedVulkanVideoCodecDevice>,
) -> RgbaPixelBufferVideoEncoder {
    let codec_device: Arc<dyn VulkanVideoCodecDevice> = owned_device.clone();
    RgbaPixelBufferVideoEncoder::new(
        SimpleEncoder::new(
            codec_device,
            SimpleEncoderConfig {
                width: REFERENCE_WIDTH,
                height: REFERENCE_HEIGHT,
                fps: FRAMES_PER_SECOND,
                codec: Codec::H264,
                preset: Preset::Medium,
                qp: None,
                bitrate_bps: None,
                streaming: true,
                idr_interval_secs: KEYFRAME_INTERVAL_SECONDS,
                prepend_header_to_idr: Some(true),
                effort_level: None,
                color_vui: Some(test_pattern_color_vui()),
            },
        )
        .expect("an encode session on the owned device"),
    )
}

/// One reference frame in memory the runtime's device exported, imported
/// onto the owned device as a pixel buffer.
fn reference_imported_onto_the_owned_device(
    reference: &Rgba8Picture,
    owned_device: &OwnedVulkanVideoCodecDevice,
) -> ConsumerVulkanBuffer {
    let gpu = gpu_context();
    let exported_by_the_runtime = gpu
        .create_opaque_fd_export_buffer(reference.rgba.len() as u64, false)
        .expect("an exportable host-visible buffer on the runtime's device");
    exported_by_the_runtime
        .write_from(&reference.rgba)
        .expect("the reference written into the runtime's buffer");
    let (raw_fd, byte_size, _) = gpu
        .export_storage_buffer_opaque_fd(&exported_by_the_runtime)
        .expect("an opaque fd of the runtime's buffer");
    // SAFETY: a fresh fd the export handed over, owned by nothing else.
    let opaque_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    ConsumerVulkanBuffer::from_opaque_fd(
        owned_device.consumer_vulkan_device(),
        opaque_fd,
        byte_size,
    )
    .expect("the runtime's memory imports onto the owned device")
}

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
// The measured scores are the run's evidence, printed for whoever ran it.
#[allow(clippy::disallowed_macros)]
fn owned_device_h264_round_trips_every_psnr_reference_inside_the_bands() {
    if skip_unless_the_device_encodes_and_decodes_vulkan_video() {
        return;
    }
    let runtime_device_uuid = gpu_context()
        .device()
        .vulkan_device()
        .physical_device_uuid();
    let owned_device = Arc::new(
        OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
            runtime_device_uuid,
            VulkanVideoCodecDirection::Encode,
        )
        .expect("an owned Vulkan Video encode device on the runtime's GPU"),
    );
    let mut encoder = owned_device_h264_encoder(&owned_device);
    let backend = probe_video_codec_backend();
    let mut decode_session = gpu_context()
        .limited_access()
        .escalate(|full| {
            backend.open_decode_session(
                full,
                &VideoDecodeSessionRequest {
                    elementary_stream: VideoCodecElementaryStream::H264,
                    maximum_coded_extent: None,
                    processor_id_of_the_decoding_node: "owned-device-psnr-decoder".to_string(),
                },
            )
        })
        .expect("a decode session on the runtime's device");

    let mut failures = Vec::new();
    let mut complex_pattern_chroma_db = None;
    let mut frame_index: i64 = 0;
    for reference_path in psnr_reference_paths() {
        let reference_name = reference_path
            .file_stem()
            .expect("a reference file name")
            .to_string_lossy()
            .to_string();
        let reference = Rgba8Picture::read_png(&reference_path);
        let pixel_buffer = reference_imported_onto_the_owned_device(&reference, &owned_device);
        let mut last_picture = None;
        for frame_of_reference in 0..FRAMES_PER_REFERENCE {
            let timestamp_ns = 1_000_000_000 + frame_index * FRAME_INTERVAL_NS;
            frame_index += 1;
            let packets = encoder
                .encode_rgba_pixel_buffer(
                    &pixel_buffer,
                    u64::from(REFERENCE_WIDTH) * 4,
                    REFERENCE_WIDTH,
                    REFERENCE_HEIGHT,
                    timestamp_ns,
                )
                .expect("the frame encodes on the owned device");
            assert_eq!(
                packets.len(),
                1,
                "{reference_name}: one access unit per frame"
            );
            if frame_of_reference == 0 {
                assert!(
                    packets[0].is_keyframe,
                    "{reference_name}: a reference run opens a new second, so a sync point"
                );
            }
            let mut decoded_pictures = Vec::new();
            decode_session
                .decode_annex_b_access_unit(&packets[0].data, &mut decoded_pictures)
                .expect("the access unit decodes on the runtime's device");
            assert_eq!(
                decoded_pictures.len(),
                1,
                "{reference_name}: one picture per access unit"
            );
            last_picture = decoded_pictures.pop();
        }
        let picture = last_picture.expect("a decoded picture");
        assert_eq!(
            (picture.width, picture.height),
            (REFERENCE_WIDTH, REFERENCE_HEIGHT)
        );
        // SAFETY: the pooled picture is held, host-readable once decode hands
        // it back, and exactly `width × height × 4` bytes.
        let decoded_rgba = unsafe {
            std::slice::from_raw_parts(
                picture.pixel_buffer.plane_base_address(0),
                REFERENCE_WIDTH as usize * REFERENCE_HEIGHT as usize * 4,
            )
        };
        let scored = Yuv420PlanePsnr::between(decoded_rgba, &reference);
        eprintln!(
            "owned-device H264 {reference_name}: Y {:.2} dB, U {:.2} dB, V {:.2} dB",
            scored.luma_db, scored.blue_difference_db, scored.red_difference_db
        );
        if reference_name == CHROMA_LOCK_REFERENCE {
            complex_pattern_chroma_db =
                Some(scored.blue_difference_db.min(scored.red_difference_db));
        }
        if scored.fails_the_bands() {
            failures.push(format!("{reference_name}: {scored:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "the owned device's H.264 decodes below the {LUMA_PSNR_FAIL_FLOOR_DB} dB Y / \
         {CHROMA_PSNR_FAIL_FLOOR_DB} dB chroma bands: {failures:#?}"
    );
    let complex_pattern_chroma_db =
        complex_pattern_chroma_db.expect("complex_pattern is among the references");
    assert!(
        (complex_pattern_chroma_db * 100.0).round() / 100.0 >= COMPLEX_PATTERN_CHROMA_LOCK_DB,
        "complex_pattern's chroma is {complex_pattern_chroma_db:.3} dB, under the \
         {COMPLEX_PATTERN_CHROMA_LOCK_DB} dB lock"
    );
}
