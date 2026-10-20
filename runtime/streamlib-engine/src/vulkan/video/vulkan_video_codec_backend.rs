// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video codec seam's Vulkan Video arm: [`SimpleEncoder`] and
//! [`SimpleDecoder`] minted on the host device, behind the seam's session
//! shapes.

use crate::core::color::H273ColorVui;
use crate::core::context::{
    DecodedVideoPictureInPooledPixelBuffer, EncodedVideoAccessUnitFromSession,
    GpuContextFullAccess, GpuContextLimitedAccess, ProcessorOutputPixelBufferPoolOfOneNativeNode,
    VideoCodecBackend, VideoCodecElementaryStream, VideoDecodeSession, VideoDecodeSessionRequest,
    VideoEncodeKnobs, VideoEncodeSession, VideoEncodeSessionRequest, VideoEncodeSourceSurface,
};
use crate::core::rhi::{PixelFormat, VulkanLayout};
use crate::core::{Error, Result};

use super::decode::{SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig};
use super::encode::{Codec, Preset, SimpleEncoder, SimpleEncoderConfig};

/// Vulkan Video hardware encode and decode on the host device's video queues.
pub(crate) struct VulkanVideoCodecBackend;

impl VideoCodecBackend for VulkanVideoCodecBackend {
    fn backend_name(&self) -> &'static str {
        "vulkan-video"
    }

    /// Every knob maps onto the session config.
    fn refuse_encode_knobs_this_arm_does_not_honour(
        &self,
        _elementary_stream: VideoCodecElementaryStream,
        _knobs: &VideoEncodeKnobs,
    ) -> Result<()> {
        Ok(())
    }

    fn open_encode_session(
        &self,
        gpu_context: &GpuContextFullAccess,
        request: &VideoEncodeSessionRequest,
    ) -> Result<Box<dyn VideoEncodeSession>> {
        let encoder = gpu_context.create_encoder_session(simple_encoder_config_for(request))?;
        Ok(Box::new(VulkanVideoEncodeSession {
            encoder,
            gpu_context: gpu_context.host_inner().limited_access(),
        }))
    }

    fn open_decode_session(
        &self,
        gpu_context: &GpuContextFullAccess,
        request: &VideoDecodeSessionRequest,
    ) -> Result<Box<dyn VideoDecodeSession>> {
        // `0` is the session's spelling of "size the DPB from the first SPS".
        let (max_width, max_height) = request.maximum_coded_extent.map_or((0, 0), |extent| {
            (extent.max_coded_width, extent.max_coded_height)
        });
        let decoder = gpu_context.create_decoder_session(SimpleDecoderConfig {
            codec: session_codec_for(request.elementary_stream),
            max_width,
            max_height,
            // Decoded pictures come back RGBA via the GPU NV12→RGBA compute
            // stage, which is what the pooled `Rgba32` pixel buffers they are
            // staged into are sized and formatted for.
            rgba_output: true,
            ..SimpleDecoderConfig::default()
        })?;
        Ok(Box::new(VulkanVideoDecodeSession {
            decoder,
            processor_output_pixel_buffer_pool: gpu_context
                .create_processor_output_pixel_buffer_pool_for_a_native_node(
                    &request.processor_id_of_the_decoding_node,
                ),
        }))
    }
}

fn session_codec_for(elementary_stream: VideoCodecElementaryStream) -> Codec {
    match elementary_stream {
        VideoCodecElementaryStream::H264 => Codec::H264,
        VideoCodecElementaryStream::H265 => Codec::H265,
    }
}

fn simple_encoder_config_for(request: &VideoEncodeSessionRequest) -> SimpleEncoderConfig {
    SimpleEncoderConfig {
        width: request.width,
        height: request.height,
        fps: request.frames_per_second,
        codec: session_codec_for(request.elementary_stream),
        preset: Preset::Medium,
        qp: None,
        bitrate_bps: request.knobs.bitrate_bps,
        // The seam's streaming shape: no B-frames, periodic IDR, parameter
        // sets prepended to every IDR for mid-stream join.
        streaming: true,
        idr_interval_secs: request.knobs.keyframe_interval_seconds,
        prepend_header_to_idr: Some(true),
        effort_level: request.knobs.effort_level,
        color_vui: request.color_vui,
    }
}

struct VulkanVideoEncodeSession {
    encoder: SimpleEncoder,
    /// Resolves each source frame's published surface to the texture the
    /// encoder samples.
    gpu_context: GpuContextLimitedAccess,
}

impl VulkanVideoEncodeSession {
    /// A source imported from another process resolves to a registration of
    /// its own on every call, so the layout the encoder barriered it into
    /// reaches the next holder only through the surface-share record. A
    /// same-process source the service does not hold is already republished
    /// on its shared registration, and the service's refusal is expected.
    fn publish_the_layout_the_encoder_left_a_cross_process_source_in(
        &self,
        surface_id: &str,
        layout_the_encoder_left_the_source_in: VulkanLayout,
    ) {
        let Some(surface_store) = self.gpu_context.surface_store() else {
            return;
        };
        if let Err(publish_failure) =
            surface_store.update_image_layout(surface_id, layout_the_encoder_left_the_source_in)
        {
            tracing::debug!(
                surface_id,
                %publish_failure,
                "the encoder's source layout was not published to surface-share"
            );
        }
    }
}

impl VideoEncodeSession for VulkanVideoEncodeSession {
    fn coded_extent(&self) -> (u32, u32) {
        self.encoder.aligned_extent()
    }

    fn encode_published_surface(
        &mut self,
        source: &VideoEncodeSourceSurface<'_>,
    ) -> Result<Vec<EncodedVideoAccessUnitFromSession>> {
        let source_registration = self
            .gpu_context
            .resolve_texture_registration_by_surface_id(
                source.surface_id,
                source.texture_layout,
                source.width,
                source.height,
            )?;
        if source.force_sync_point {
            self.encoder.force_idr();
        }
        let layout_the_source_was_published_in = source_registration.current_layout();
        let encoded = self
            .encoder
            .encode_source_texture(&source_registration, Some(source.timestamp_ns));
        let layout_the_encoder_left_the_source_in = source_registration.current_layout();
        if layout_the_encoder_left_the_source_in != layout_the_source_was_published_in {
            self.publish_the_layout_the_encoder_left_a_cross_process_source_in(
                source.surface_id,
                layout_the_encoder_left_the_source_in,
            );
        }
        Ok(encoded?
            .into_iter()
            .map(|packet| EncodedVideoAccessUnitFromSession {
                annex_b_access_unit_bytes: packet.data,
                is_sync_point: packet.is_keyframe,
                timestamp_ns: packet.timestamp_ns,
            })
            .collect())
    }
}

struct VulkanVideoDecodeSession {
    decoder: SimpleDecoder,
    /// The decoding node's own output pool, which the pixel buffers decoded
    /// pictures are staged into come from.
    processor_output_pixel_buffer_pool: ProcessorOutputPixelBufferPoolOfOneNativeNode,
}

impl VideoDecodeSession for VulkanVideoDecodeSession {
    fn decode_annex_b_access_unit(
        &mut self,
        annex_b_access_unit_bytes: &[u8],
        decoded_pictures_in_completion_order: &mut Vec<DecodedVideoPictureInPooledPixelBuffer>,
    ) -> Result<()> {
        let decoded_frames = self.decoder.feed(annex_b_access_unit_bytes)?;
        for decoded_frame in decoded_frames {
            decoded_pictures_in_completion_order.push(
                stage_decoded_frame_into_pooled_pixel_buffer(
                    &mut self.processor_output_pixel_buffer_pool,
                    decoded_frame,
                )?,
            );
        }
        Ok(())
    }

    fn parsed_parameter_set_color_vui(&self) -> Option<H273ColorVui> {
        self.decoder.current_color_vui()
    }

    fn discard_in_flight_state_after_a_gap(&mut self) {
        self.decoder.feed_discontinuity();
    }

    fn reset_for_new_parameter_sets(&mut self) {
        self.decoder.reset();
    }
}

/// Stage one read-back RGBA picture into the next pixel buffer of the decoding
/// node's output pool, whose published id becomes the frame's surface id. The
/// extent is the session's own — already the stream's conformance window, so
/// an H.265 stream's CTU padding is gone before a surface id ever names these
/// pixels.
fn stage_decoded_frame_into_pooled_pixel_buffer(
    processor_output_pixel_buffer_pool: &mut ProcessorOutputPixelBufferPoolOfOneNativeNode,
    decoded_frame: SimpleDecodedFrame,
) -> Result<DecodedVideoPictureInPooledPixelBuffer> {
    if !decoded_frame.is_rgba {
        return Err(Error::GpuError(
            "the session handed back an NV12 picture though it was minted for RGBA output — \
             the pooled pixel buffer is sized and formatted for RGBA"
                .into(),
        ));
    }
    let width = decoded_frame.width;
    let height = decoded_frame.height;
    let tightly_packed_rgba_byte_count = (width as usize) * (height as usize) * 4;
    let tightly_packed_rgba = decoded_frame
        .data
        .get(..tightly_packed_rgba_byte_count)
        .ok_or_else(|| {
            Error::GpuError(format!(
                "cannot stage a {width}x{height} RGBA picture from {} read-back bytes; it \
                 needs {tightly_packed_rgba_byte_count}",
                decoded_frame.data.len()
            ))
        })?;

    let (published_pixel_buffer_frame_id, pixel_buffer) = processor_output_pixel_buffer_pool
        .acquire_the_next_output_pixel_buffer(width, height, PixelFormat::Rgba32)?;
    pixel_buffer.write_this_plane_from(0, tightly_packed_rgba)?;
    Ok(DecodedVideoPictureInPooledPixelBuffer {
        published_pixel_buffer_frame_id,
        pixel_buffer,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_elementary_stream_mints_the_session_codec_of_the_same_name() {
        assert_eq!(
            session_codec_for(VideoCodecElementaryStream::H264),
            Codec::H264
        );
        assert_eq!(
            session_codec_for(VideoCodecElementaryStream::H265),
            Codec::H265
        );
    }

    #[test]
    fn an_encode_request_mints_the_streaming_shape_every_sync_point_can_be_joined_at() {
        let config = simple_encoder_config_for(&VideoEncodeSessionRequest {
            elementary_stream: VideoCodecElementaryStream::H265,
            width: 1920,
            height: 1080,
            frames_per_second: 30,
            knobs: VideoEncodeKnobs {
                bitrate_bps: Some(4_000_000),
                keyframe_interval_seconds: 1,
                effort_level: Some(2),
            },
            color_vui: None,
        });
        assert!(config.streaming);
        assert_eq!(config.prepend_header_to_idr, Some(true));
        assert_eq!(config.idr_interval_secs, 1);
        assert_eq!(config.codec, Codec::H265);
        assert_eq!((config.width, config.height, config.fps), (1920, 1080, 30));
        assert_eq!(config.bitrate_bps, Some(4_000_000));
        assert_eq!(config.effort_level, Some(2));
        assert_eq!(config.qp, None);
        assert_eq!(config.preset, Preset::Medium);
    }

    /// A source imported from another process is barriered into the layout
    /// the encoder samples it in, and that layout reaches the surface-share
    /// record the next import's acquire barrier starts from.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_cross_process_source_the_encoder_barriered_publishes_its_new_layout_to_surface_share() {
        use crate::core::context::GpuContext;
        use crate::core::rhi::TextureFormat;
        use crate::core::test_support::LiveSurfaceShareServiceForATest;
        use crate::core::test_support::rig_only_capability_skip::{
            RigOnlyCapability, skip_unless_this_machine_has,
        };
        use crate::host_rhi::{HostGpuDeviceExt as _, HostSurfaceStoreExt as _};
        use crate::vulkan::rhi::{VulkanAccess, VulkanStage};

        const SOURCE_WIDTH: u32 = 320;
        const SOURCE_HEIGHT: u32 = 180;
        const SURFACE_ID: &str = "cross-process-colour-attachment-source";

        let Ok(gpu) = GpuContext::init_for_platform_sync() else {
            println!("Skipping - no GPU device available");
            return;
        };
        let vulkan_device = gpu.device().vulkan_device();
        if skip_unless_this_machine_has(
            RigOnlyCapability::VulkanVideo,
            vulkan_device.supports_video_encode(),
        ) || skip_unless_this_machine_has(
            RigOnlyCapability::DmaBufModifiers,
            vulkan_device.has_render_target_modifier_for_texture_format(TextureFormat::Rgba8Unorm),
        ) {
            return;
        }
        let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
        let sandbox = GpuContextLimitedAccess::new(gpu);

        let (mut encode_session, _render_target) = sandbox
            .escalate(|full| {
                let render_target = full.acquire_render_target_dma_buf_image(
                    SOURCE_WIDTH,
                    SOURCE_HEIGHT,
                    TextureFormat::Rgba8Unorm,
                )?;
                let mut recorder = full.create_command_recorder("a draw's end state")?;
                recorder.begin()?;
                recorder.record_image_barrier(
                    &render_target,
                    VulkanLayout::UNDEFINED,
                    VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
                    VulkanStage::ALL_COMMANDS,
                    VulkanStage::COLOR_ATTACHMENT_OUTPUT,
                    VulkanAccess::NONE,
                    VulkanAccess::COLOR_ATTACHMENT_WRITE,
                )?;
                recorder.submit_and_wait()?;
                full.surface_store()
                    .expect("the live service's store is installed")
                    .register_texture(
                        SURFACE_ID,
                        &render_target,
                        None,
                        None,
                        VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
                    )?;
                let encode_session = VulkanVideoCodecBackend.open_encode_session(
                    full,
                    &VideoEncodeSessionRequest {
                        elementary_stream: VideoCodecElementaryStream::H264,
                        width: SOURCE_WIDTH,
                        height: SOURCE_HEIGHT,
                        frames_per_second: 10,
                        knobs: VideoEncodeKnobs {
                            bitrate_bps: None,
                            keyframe_interval_seconds: 1,
                            effort_level: None,
                        },
                        color_vui: None,
                    },
                )?;
                Ok((encode_session, render_target))
            })
            .expect("a registered cross-process source and an encode session");

        let layout_the_service_holds = || {
            live_service
                .state
                .get_surfaces()
                .into_iter()
                .find(|surface| surface.surface_id == SURFACE_ID)
                .map(|surface| {
                    surface
                        .current_image_layout
                        .load(std::sync::atomic::Ordering::Acquire)
                })
                .expect("the service holds the source")
        };
        assert_eq!(
            layout_the_service_holds(),
            VulkanLayout::COLOR_ATTACHMENT_OPTIMAL.0
        );

        let access_units = encode_session
            .encode_published_surface(&VideoEncodeSourceSurface {
                surface_id: SURFACE_ID,
                texture_layout: None,
                width: SOURCE_WIDTH,
                height: SOURCE_HEIGHT,
                timestamp_ns: 1_000_000_000,
                force_sync_point: false,
            })
            .expect("a cross-process source left in COLOR_ATTACHMENT_OPTIMAL encodes");
        assert_eq!(access_units.len(), 1);
        assert_eq!(
            layout_the_service_holds(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0,
            "the surface-share record names the layout the encoder left the source in"
        );
        drop(encode_session);
        live_service.stop();
    }
}
