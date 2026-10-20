// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Vulkan Video encoder is a consumer of published frames like any other:
//! a frame a graphics kernel drew, published in `COLOR_ATTACHMENT_OPTIMAL`, is
//! barriered into the layout the encoder samples it in, encodes, decodes back
//! inside the PSNR bands, and its registration then names the layout it was
//! left in. A frame published in `UNDEFINED` is refused by name.
//!
//! Rig tier — it needs Vulkan Video encode and decode queues, so Cargo builds
//! it only under `hardware-tests`.

#![cfg(target_os = "linux")]

#[path = "support/codec_round_trip_scoring.rs"]
mod codec_round_trip_scoring;
#[path = "support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;

use std::sync::OnceLock;

use codec_round_trip_scoring::{
    CHROMA_PSNR_FAIL_FLOOR_DB, LUMA_PSNR_FAIL_FLOOR_DB, Rgba8Picture, Yuv420PlanePsnr,
    cross_floor_clip_reference_picture,
};
use streamlib::sdk::context::{
    GpuContext, TextureRegistration, VideoCodecElementaryStream, VideoDecodeSessionRequest,
    VideoEncodeKnobs, VideoEncodeSession, VideoEncodeSessionRequest, VideoEncodeSourceSurface,
    probe_video_codec_backend,
};
use streamlib::sdk::engine::HostGpuDeviceExt as _;
use streamlib::sdk::engine::host_rhi::{VulkanAccess, VulkanStage};
use streamlib::sdk::rhi::{
    PixelFormat, TextureDescriptor, TextureFormat, TextureUsages, VulkanLayout,
};

const FRAMES_PER_SECOND: u32 = 10;
const KEYFRAME_INTERVAL_SECONDS: u32 = 1;
const FRAMES_ENCODED: usize = 3;

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

fn open_h264_encode_session(width: u32, height: u32) -> Box<dyn VideoEncodeSession> {
    let backend = probe_video_codec_backend();
    gpu_context()
        .limited_access()
        .escalate(|full| {
            backend.open_encode_session(
                full,
                &VideoEncodeSessionRequest {
                    elementary_stream: VideoCodecElementaryStream::H264,
                    width,
                    height,
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
        .expect("a hardware encode session")
}

/// A colour-attachment-capable texture holding `picture`, registered under a
/// fresh surface id the way a graphics kernel's output is.
fn register_a_render_target_holding(picture: &Rgba8Picture) -> String {
    let gpu = gpu_context();
    let texture = gpu
        .device()
        .create_texture_local(
            &TextureDescriptor::new(picture.width, picture.height, TextureFormat::Rgba8Unorm)
                .with_usage(
                    TextureUsages::RENDER_ATTACHMENT
                        | TextureUsages::TEXTURE_BINDING
                        | TextureUsages::COPY_DST
                        | TextureUsages::COPY_SRC,
                ),
        )
        .expect("a render target");
    let surface_id = uuid::Uuid::new_v4().to_string();
    gpu.register_texture_with_layout(&surface_id, texture.clone(), VulkanLayout::UNDEFINED);
    let (_pool_id, staging_pixel_buffer) = gpu
        .acquire_pixel_buffer(picture.width, picture.height, PixelFormat::Rgba32)
        .expect("an upload source");
    staging_pixel_buffer
        .write_this_plane_from(0, &picture.rgba)
        .expect("the picture staged");
    gpu.copy_pixel_buffer_to_texture(
        &staging_pixel_buffer,
        &texture,
        &surface_id,
        picture.width,
        picture.height,
    )
    .expect("the picture uploaded into the render target");
    surface_id
}

fn resolve_registration(surface_id: &str, picture: &Rgba8Picture) -> TextureRegistration {
    gpu_context()
        .resolve_texture_registration_by_surface_id(surface_id, None, picture.width, picture.height)
        .expect("the render target resolves")
}

/// Leave the texture where a graphics kernel's draw leaves its colour
/// targets, and publish that layout on its registration.
fn leave_it_in_color_attachment_optimal_as_a_draw_does(registration: &TextureRegistration) {
    let mut recorder = gpu_context()
        .limited_access()
        .escalate(|full| full.create_command_recorder("the draw a graphics kernel ends with"))
        .expect("a command recorder");
    recorder.begin().expect("recording begins");
    recorder
        .record_image_barrier(
            registration.texture(),
            registration.current_layout(),
            VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::COLOR_ATTACHMENT_OUTPUT,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::COLOR_ATTACHMENT_WRITE,
        )
        .expect("the barrier records");
    recorder.submit_and_wait().expect("the barrier runs");
    registration.update_layout(VulkanLayout::COLOR_ATTACHMENT_OPTIMAL);
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video encode and decode queues; see docs/testing-hardware.md"
)]
fn a_frame_published_in_color_attachment_optimal_encodes_and_decodes_inside_the_bands() {
    if skip_unless_the_device_encodes_and_decodes_vulkan_video() {
        return;
    }
    let reference = cross_floor_clip_reference_picture();
    let surface_id = register_a_render_target_holding(&reference);
    let registration = resolve_registration(&surface_id, &reference);

    let mut encode_session = open_h264_encode_session(reference.width, reference.height);
    let backend = probe_video_codec_backend();
    let mut decode_session = gpu_context()
        .limited_access()
        .escalate(|full| {
            backend.open_decode_session(
                full,
                &VideoDecodeSessionRequest {
                    elementary_stream: VideoCodecElementaryStream::H264,
                    maximum_coded_extent: None,
                    processor_id_of_the_decoding_node: "color-attachment-source-decoder"
                        .to_string(),
                },
            )
        })
        .expect("a decode session");

    let mut last_decoded_picture = None;
    for frame_index in 0..FRAMES_ENCODED {
        leave_it_in_color_attachment_optimal_as_a_draw_does(&registration);
        let access_units = encode_session
            .encode_published_surface(&VideoEncodeSourceSurface {
                surface_id: &surface_id,
                texture_layout: None,
                width: reference.width,
                height: reference.height,
                timestamp_ns: 1_000_000_000 + frame_index as i64 * 100_000_000,
                force_sync_point: false,
            })
            .expect("a frame published in COLOR_ATTACHMENT_OPTIMAL encodes");
        assert_eq!(
            registration.current_layout(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            "the encoder republishes the layout it barriered the source into"
        );
        assert_eq!(
            access_units.len(),
            1,
            "with reordering off, every frame completes one access unit"
        );
        let mut decoded_pictures = Vec::new();
        decode_session
            .decode_annex_b_access_unit(
                &access_units[0].annex_b_access_unit_bytes,
                &mut decoded_pictures,
            )
            .expect("the access unit decodes");
        assert_eq!(decoded_pictures.len(), 1, "one picture per access unit");
        last_decoded_picture = decoded_pictures.pop();
    }

    let picture = last_decoded_picture.expect("a decoded picture");
    assert_eq!(
        (picture.width, picture.height),
        (reference.width, reference.height)
    );
    // SAFETY: the pooled picture is held, host-readable once decode hands it
    // back, and exactly `width × height × 4` bytes.
    let decoded_rgba = unsafe {
        std::slice::from_raw_parts(
            picture.pixel_buffer.plane_base_address(0),
            reference.width as usize * reference.height as usize * 4,
        )
    };
    let scored = Yuv420PlanePsnr::between(decoded_rgba, &reference);
    assert!(
        !scored.fails_the_bands(),
        "a frame encoded out of COLOR_ATTACHMENT_OPTIMAL decodes below the \
         {LUMA_PSNR_FAIL_FLOOR_DB} dB Y / {CHROMA_PSNR_FAIL_FLOOR_DB} dB chroma bands: {scored:?}"
    );
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video encode queues; see docs/testing-hardware.md"
)]
fn a_frame_published_in_undefined_is_refused_by_name() {
    if skip_unless_the_device_encodes_and_decodes_vulkan_video() {
        return;
    }
    let reference = cross_floor_clip_reference_picture();
    let surface_id = register_a_render_target_holding(&reference);
    resolve_registration(&surface_id, &reference).update_layout(VulkanLayout::UNDEFINED);

    let refusal = open_h264_encode_session(reference.width, reference.height)
        .encode_published_surface(&VideoEncodeSourceSurface {
            surface_id: &surface_id,
            texture_layout: None,
            width: reference.width,
            height: reference.height,
            timestamp_ns: 1_000_000_000,
            force_sync_point: false,
        })
        .err()
        .expect("a frame no producer published a layout for is refused");
    assert!(refusal.to_string().contains("UNDEFINED"), "{refusal}");
}
