// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! H.264 and H.265 on the devices the codec modules own, scored as the rig
//! scores them: each checked-in PSNR reference is written into memory the
//! runtime's device exports, imported onto an owned Vulkan Video encode device
//! on the same GPU as a module receives a frame, encoded there, decoded back on
//! an owned decode device, and scored inside the bands — Y and both chroma
//! planes at 30 dB or better over BT.709 full-range 4:2:0. `complex_pattern`'s
//! chroma holds each codec's lock. Decode sessions in the video codec host
//! decode the same stream side by side on its one shared decode device, each
//! sent every picture buffer once.
//!
//! Rig tier — it needs Vulkan Video encode and decode queues, so it runs only
//! under `hardware-tests`.

#![cfg(target_os = "linux")]

#[path = "support/codec_round_trip_scoring.rs"]
mod codec_round_trip_scoring;
#[path = "../../streamlib-engine/tests/support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;

use std::collections::HashSet;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
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
use streamlib_owned_device_video_codec_nodes::video_codec_host::{
    VideoCodecDevicesOfTheHost, serve_one_video_codec_session,
};
use streamlib_owned_device_video_codec_nodes::video_codec_host_protocol::{
    MOST_PICTURE_BUFFERS_A_DECODE_SENDS, VIDEO_CODEC_HOST_PROTOCOL_VERSION,
    VideoAccessUnitToDecode, VideoCodecHostConnection, VideoCodecHostElementaryStream,
    VideoCodecHostReply, VideoCodecHostRequest, VideoDecodeGroupStart, VideoDecodeSessionRequest,
};
use streamlib_owned_device_video_codec_nodes::video_decoder_settings::VideoDecoderSettings;
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

/// `complex_pattern`'s chroma figure for `codec`, to two places, measured on an RTX 3090 on
/// NVIDIA driver 595.99.02 with the encoder's input rounded to the nearest 8-bit level (H.264
/// 33.53, H.265 33.42). The 33.52 H.264 lock predates the Slang port and was met by the GLSL
/// shader's `mediump`, whose fp16 arithmetic rounded luma up; the fp32 conversion is the exact
/// one.
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
    owned_device_encoder_at(owned_device, codec, None)
}

/// The encoder a codec module mints for a reference-sized frame, rate
/// controlled to `bitrate_bps` when it is set.
fn owned_device_encoder_at(
    owned_device: &Arc<OwnedVulkanVideoCodecDevice>,
    codec: Codec,
    bitrate_bps: Option<u32>,
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
                bitrate_bps,
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

#[allow(
    clippy::disallowed_macros,
    reason = "the measured scores are the run's evidence, printed for whoever ran it"
)]
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

/// Two rate-controlled bitrates far enough apart that a session which
/// applies its rate control codes the same pictures at very different sizes.
/// A still picture codes to well under a megabit a second, so the low one is
/// set far below that.
const LOW_BITRATE_BPS: u32 = 100_000;
const HIGH_BITRATE_BPS: u32 = 16_000_000;

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn owned_device_h264_encoder_with_a_bitrate_encodes_its_first_frame_at_that_bitrate() {
    encoder_with_a_bitrate_encodes_its_first_frame_at_that_bitrate(Codec::H264);
}

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn owned_device_h265_encoder_with_a_bitrate_encodes_its_first_frame_at_that_bitrate() {
    encoder_with_a_bitrate_encodes_its_first_frame_at_that_bitrate(Codec::H265);
}

/// The bytes two seconds of `complex_pattern` code to at `bitrate_bps`, each
/// frame encoded and the first a sync point.
fn bytes_of_two_seconds_at(
    owned_encode_device: &Arc<OwnedVulkanVideoCodecDevice>,
    codec: Codec,
    bitrate_bps: u32,
) -> usize {
    let mut encoder = owned_device_encoder_at(owned_encode_device, codec, Some(bitrate_bps));
    let reference_path = psnr_reference_paths()
        .into_iter()
        .find(|path| {
            path.file_stem()
                .is_some_and(|stem| stem == CHROMA_LOCK_REFERENCE)
        })
        .expect("complex_pattern is among the references");
    let reference = Rgba8Picture::read_png(&reference_path);
    let pixel_buffer = reference_imported_onto_the_owned_device(&reference, owned_encode_device);
    let mut bytes = 0;
    for frame_index in 0..2 * FRAMES_PER_REFERENCE as i64 {
        let packets = encoder
            .encode_rgba_pixel_buffer(
                &pixel_buffer,
                u64::from(REFERENCE_WIDTH) * 4,
                REFERENCE_WIDTH,
                REFERENCE_HEIGHT,
                1_000_000_000 + frame_index * FRAME_INTERVAL_NS,
            )
            .unwrap_or_else(|failure| {
                panic!("{codec:?} at {bitrate_bps} b/s: frame {frame_index} encodes: {failure}")
            });
        assert_eq!(packets.len(), 1, "{codec:?}: one access unit per frame");
        if frame_index == 0 {
            assert!(
                packets[0].is_keyframe,
                "{codec:?} at {bitrate_bps} b/s: the first frame is a sync point"
            );
        }
        bytes += packets[0].data.len();
    }
    bytes
}

#[allow(
    clippy::disallowed_macros,
    reason = "the measured sizes are the run's evidence, printed for whoever ran it"
)]
fn encoder_with_a_bitrate_encodes_its_first_frame_at_that_bitrate(codec: Codec) {
    let Some(owned_encode_device) =
        owned_device_on_the_runtimes_gpu(VulkanVideoCodecDirection::Encode)
    else {
        return;
    };
    let low_bytes = bytes_of_two_seconds_at(&owned_encode_device, codec, LOW_BITRATE_BPS);
    let high_bytes = bytes_of_two_seconds_at(&owned_encode_device, codec, HIGH_BITRATE_BPS);
    eprintln!(
        "owned-device {codec:?} complex_pattern, two seconds: {low_bytes} bytes at \
         {LOW_BITRATE_BPS} b/s, {high_bytes} bytes at {HIGH_BITRATE_BPS} b/s"
    );
    assert!(
        low_bytes * 2 < high_bytes,
        "{codec:?}: {low_bytes} bytes at {LOW_BITRATE_BPS} b/s and {high_bytes} bytes at \
         {HIGH_BITRATE_BPS} b/s, so the session's rate control was not applied"
    );
}

/// Decode sessions the host's shared decode device serves side by side.
const DECODE_SESSIONS_SHARING_ONE_DEVICE: usize = 4;

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn h264_decode_sessions_share_the_video_codec_hosts_decode_device() {
    decode_sessions_share_the_video_codec_hosts_decode_device(Codec::H264);
}

#[test]
#[cfg_attr(not(feature = "hardware-tests"), ignore)]
fn h265_decode_sessions_share_the_video_codec_hosts_decode_device() {
    decode_sessions_share_the_video_codec_hosts_decode_device(Codec::H265);
}

/// The access units of two seconds of the first reference, encoded on an
/// owned device.
fn two_seconds_of_access_units(
    owned_encode_device: &Arc<OwnedVulkanVideoCodecDevice>,
    codec: Codec,
) -> Vec<(Vec<u8>, bool)> {
    let mut encoder = owned_device_encoder(owned_encode_device, codec);
    let reference = Rgba8Picture::read_png(&psnr_reference_paths()[0]);
    let pixel_buffer = reference_imported_onto_the_owned_device(&reference, owned_encode_device);
    (0..2 * FRAMES_PER_REFERENCE as i64)
        .flat_map(|frame_index| {
            encoder
                .encode_rgba_pixel_buffer(
                    &pixel_buffer,
                    u64::from(REFERENCE_WIDTH) * 4,
                    REFERENCE_WIDTH,
                    REFERENCE_HEIGHT,
                    1_000_000_000 + frame_index * FRAME_INTERVAL_NS,
                )
                .expect("the frame encodes on the owned device")
                .into_iter()
                .map(|packet| (packet.data, packet.is_keyframe))
        })
        .collect()
}

fn decode_sessions_share_the_video_codec_hosts_decode_device(codec: Codec) {
    let Some(owned_encode_device) =
        owned_device_on_the_runtimes_gpu(VulkanVideoCodecDirection::Encode)
    else {
        return;
    };
    let access_units = two_seconds_of_access_units(&owned_encode_device, codec);
    let vulkan_device_uuid = gpu_context()
        .device()
        .vulkan_device()
        .physical_device_uuid();
    let elementary_stream = match codec {
        Codec::H264 => VideoCodecHostElementaryStream::H264,
        Codec::H265 => VideoCodecHostElementaryStream::H265,
    };
    let devices = VideoCodecDevicesOfTheHost::default();
    std::thread::scope(|scope| {
        let sessions: Vec<_> = (0..DECODE_SESSIONS_SHARING_ONE_DEVICE)
            .map(|_| {
                let (node_end, host_end) = UnixStream::pair().expect("a socket pair");
                let devices = &devices;
                scope.spawn(move || serve_one_video_codec_session(host_end, devices));
                let mut node = VideoCodecHostConnection::over(node_end);
                node.send(&VideoCodecHostRequest::OpenDecodeSession(
                    VideoDecodeSessionRequest {
                        protocol_version: VIDEO_CODEC_HOST_PROTOCOL_VERSION,
                        elementary_stream,
                        settings: VideoDecoderSettings::default(),
                        vulkan_device_uuid,
                    },
                ))
                .expect("the opening is sent");
                let opened = node
                    .receive::<VideoCodecHostReply>()
                    .expect("the opening is answered");
                assert!(
                    matches!(opened, Some(VideoCodecHostReply::SessionOpened { .. })),
                    "{codec:?}: {opened:?}"
                );
                node
            })
            .collect();
        let decoding: Vec<_> = sessions
            .into_iter()
            .map(|mut node| {
                let access_units = &access_units;
                scope.spawn(move || {
                    let mut picture_buffers_sent = HashSet::new();
                    let mut pictures = 0;
                    for (index, (annex_b_access_unit, is_keyframe)) in
                        access_units.iter().enumerate()
                    {
                        let group_start = is_keyframe.then_some(if index == 0 {
                            VideoDecodeGroupStart::EnteringTheStream
                        } else {
                            VideoDecodeGroupStart::ContinuingTheStream
                        });
                        node.send(&VideoCodecHostRequest::DecodeAccessUnit(
                            VideoAccessUnitToDecode {
                                annex_b_access_unit: annex_b_access_unit.clone(),
                                group_start,
                            },
                        ))
                        .expect("the access unit is sent");
                        let (reply, fds) = node
                            .receive_with_fds::<VideoCodecHostReply>(
                                MOST_PICTURE_BUFFERS_A_DECODE_SENDS,
                            )
                            .expect("the decode is answered")
                            .expect("the host keeps the session open");
                        let decoded = reply.into_the_decode().expect("the access unit decodes");
                        assert_eq!(fds.len(), decoded.picture_buffers_sent());
                        for picture in &decoded.pictures {
                            assert_eq!(
                                (picture.width, picture.height),
                                (REFERENCE_WIDTH, REFERENCE_HEIGHT),
                                "{codec:?}: the display window"
                            );
                            let first_time = picture_buffers_sent.insert(picture.picture_buffer);
                            assert_eq!(
                                first_time, picture.picture_buffer_is_sent_with_it,
                                "{codec:?}: a picture buffer is sent once, the first time"
                            );
                        }
                        pictures += decoded.pictures.len();
                    }
                    (pictures, picture_buffers_sent.len())
                })
            })
            .collect();
        for session in decoding {
            let (pictures, picture_buffers) = session.join().expect("a decoding session");
            assert_eq!(
                pictures,
                access_units.len(),
                "{codec:?}: one picture per access unit"
            );
            assert!(
                picture_buffers < pictures,
                "{codec:?}: {picture_buffers} buffers for {pictures} pictures, so buffers are                  reused"
            );
        }
    });
}
