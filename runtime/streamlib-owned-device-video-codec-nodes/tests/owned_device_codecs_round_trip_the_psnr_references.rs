// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! H.264 and H.265 on the devices the codec modules own, scored as the rig
//! scores them: each checked-in PSNR reference is written into memory the
//! runtime's device exports, imported onto an owned Vulkan Video encode device
//! on the same GPU as a module receives a frame, encoded there, decoded back on
//! an owned decode device, and scored inside the bands — Y and both chroma
//! planes at 30 dB or better over BT.709 full-range 4:2:0. `complex_pattern`'s
//! chroma holds each codec's lock.
//!
//! Rig tier — it needs Vulkan Video encode and decode queues, so it runs only
//! under `hardware-tests`.

#![cfg(target_os = "linux")]

#[path = "support/codec_round_trip_scoring.rs"]
mod codec_round_trip_scoring;
#[path = "../../streamlib-engine/tests/support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::sync::{Arc, OnceLock};

use codec_round_trip_scoring::{
    CHROMA_PSNR_FAIL_FLOOR_DB, LUMA_PSNR_FAIL_FLOOR_DB, Rgba8Picture, Yuv420PlanePsnr,
    psnr_reference_paths,
};
use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::HostGpuDeviceExt as _;
use streamlib_consumer_rhi::h273_color_vui::{H273ColorVui, primaries, transfer};
use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection,
};
use streamlib_vulkan_video::{
    Codec, DecodedPictureDelivery, Preset, RgbaPixelBufferVideoEncoder, SimpleDecoder,
    SimpleDecoderConfig, SimpleEncoder, SimpleEncoderConfig, VulkanVideoCodecDevice,
};

/// The references' extent; 1080 lines is a multiple of neither codec's block,
/// so the stream carries a conformance crop.
const REFERENCE_WIDTH: u32 = 1920;
const REFERENCE_HEIGHT: u32 = 1080;
const FRAMES_PER_SECOND: u32 = 10;
const KEYFRAME_INTERVAL_SECONDS: u32 = 1;
const FRAMES_PER_REFERENCE: u32 = FRAMES_PER_SECOND * KEYFRAME_INTERVAL_SECONDS;
const FRAME_INTERVAL_NS: i64 = 1_000_000_000 / FRAMES_PER_SECOND as i64;

/// The reference whose chroma carries the lowest clean figure in the set.
const CHROMA_LOCK_REFERENCE: &str = "complex_pattern";

/// `complex_pattern`'s chroma figure for `codec`, to two places.
fn complex_pattern_chroma_lock_db(codec: Codec) -> f64 {
    match codec {
        Codec::H264 => 33.52,
        Codec::H265 => 33.42,
    }
}

fn gpu_context() -> &'static GpuContext {
    static GPU_CONTEXT: OnceLock<GpuContext> = OnceLock::new();
    GPU_CONTEXT.get_or_init(|| GpuContext::init_for_platform().expect("a Vulkan device"))
}

/// An owned device on the runtime's GPU coding in `direction`, or `None`,
/// having printed the skip line, when the GPU has no Vulkan Video that way.
fn owned_device_on_the_runtimes_gpu(
    direction: VulkanVideoCodecDirection,
) -> Option<Arc<OwnedVulkanVideoCodecDevice>> {
    let runtime_device_uuid = gpu_context()
        .device()
        .vulkan_device()
        .physical_device_uuid();
    let owned_device = OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
        runtime_device_uuid,
        direction,
    );
    if rig_only_capability_skip::skip_unless_this_machine_has(
        rig_only_capability_skip::RigOnlyCapability::VulkanVideo,
        owned_device.is_ok(),
    ) {
        return None;
    }
    owned_device.ok().map(Arc::new)
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

/// The encoder a codec module mints for a reference-sized frame.
fn owned_device_encoder(
    owned_device: &Arc<OwnedVulkanVideoCodecDevice>,
    codec: Codec,
) -> RgbaPixelBufferVideoEncoder {
    let codec_device: Arc<dyn VulkanVideoCodecDevice> = owned_device.clone();
    RgbaPixelBufferVideoEncoder::new(
        SimpleEncoder::new(
            codec_device,
            SimpleEncoderConfig {
                width: REFERENCE_WIDTH,
                height: REFERENCE_HEIGHT,
                fps: FRAMES_PER_SECOND,
                codec,
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

/// The decoder a codec module mints, handing each picture back as RGBA.
fn owned_device_decoder(
    owned_device: &Arc<OwnedVulkanVideoCodecDevice>,
    codec: Codec,
) -> SimpleDecoder {
    let codec_device: Arc<dyn VulkanVideoCodecDevice> = owned_device.clone();
    SimpleDecoder::new(
        codec_device,
        SimpleDecoderConfig {
            codec,
            decoded_picture_delivery: DecodedPictureDelivery::ReadBackRgba,
            ..SimpleDecoderConfig::default()
        },
    )
    .expect("a decode session on the owned device")
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
fn owned_device_h264_round_trips_every_psnr_reference_inside_the_bands() {
    round_trip_every_psnr_reference_on_owned_devices(Codec::H264);
}

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn owned_device_h265_round_trips_every_psnr_reference_inside_the_bands() {
    round_trip_every_psnr_reference_on_owned_devices(Codec::H265);
}

// The measured scores are the run's evidence, printed for whoever ran it.
#[allow(clippy::disallowed_macros)]
fn round_trip_every_psnr_reference_on_owned_devices(codec: Codec) {
    let Some(owned_encode_device) =
        owned_device_on_the_runtimes_gpu(VulkanVideoCodecDirection::Encode)
    else {
        return;
    };
    let Some(owned_decode_device) =
        owned_device_on_the_runtimes_gpu(VulkanVideoCodecDirection::Decode)
    else {
        return;
    };
    let mut encoder = owned_device_encoder(&owned_encode_device, codec);
    let mut decoder = owned_device_decoder(&owned_decode_device, codec);

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
        let pixel_buffer =
            reference_imported_onto_the_owned_device(&reference, &owned_encode_device);
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
                "{codec:?} {reference_name}: one access unit per frame"
            );
            if frame_of_reference == 0 {
                assert!(
                    packets[0].is_keyframe,
                    "{codec:?} {reference_name}: a reference run opens a new second, so a sync point"
                );
            }
            let mut decoded_pictures = decoder
                .feed(&packets[0].data)
                .expect("the access unit decodes on the owned device");
            assert_eq!(
                decoded_pictures.len(),
                1,
                "{codec:?} {reference_name}: one picture per access unit"
            );
            last_picture = decoded_pictures.pop();
        }
        let picture = last_picture.expect("a decoded picture");
        assert!(
            picture.is_rgba,
            "{codec:?} {reference_name}: read back as RGBA"
        );
        assert_eq!(
            (picture.width, picture.height),
            (REFERENCE_WIDTH, REFERENCE_HEIGHT),
            "{codec:?} {reference_name}: the decoder hands back the conformance window"
        );
        let scored = Yuv420PlanePsnr::between(&picture.data, &reference);
        eprintln!(
            "owned-device {codec:?} {reference_name}: Y {:.2} dB, U {:.2} dB, V {:.2} dB",
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
        "the owned devices' {codec:?} decodes below the {LUMA_PSNR_FAIL_FLOOR_DB} dB Y / \
         {CHROMA_PSNR_FAIL_FLOOR_DB} dB chroma bands: {failures:#?}"
    );
    let complex_pattern_chroma_db =
        complex_pattern_chroma_db.expect("complex_pattern is among the references");
    let lock_db = complex_pattern_chroma_lock_db(codec);
    assert!(
        (complex_pattern_chroma_db * 100.0).round() / 100.0 >= lock_db,
        "{codec:?}: complex_pattern's chroma is {complex_pattern_chroma_db:.3} dB, under the \
         {lock_db} dB lock"
    );
}
