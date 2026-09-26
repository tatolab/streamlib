// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video codec seam's Vulkan Video arm: [`SimpleEncoder`] and
//! [`SimpleDecoder`] minted on the host device, behind the seam's session
//! shapes.

use crate::core::color::H273ColorVui;
use crate::core::context::{
    DecodedVideoPictureInPooledPixelBuffer, EncodedVideoAccessUnitFromSession,
    GpuContextFullAccess, GpuContextLimitedAccess, VideoCodecBackend, VideoCodecElementaryStream,
    VideoDecodeSession, VideoDecodeSessionRequest, VideoEncodeSession, VideoEncodeSessionRequest,
    VideoEncodeSourceSurface,
};
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};

use super::decode::{SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig};
use super::encode::{Codec, Preset, SimpleEncoder, SimpleEncoderConfig};

/// Vulkan Video hardware encode and decode on the host device's video queues.
pub(crate) struct VulkanVideoCodecBackend;

impl VideoCodecBackend for VulkanVideoCodecBackend {
    fn backend_name(&self) -> &'static str {
        "vulkan-video"
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
            gpu_context: gpu_context.host_inner().limited_access(),
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
        bitrate_bps: request.bitrate_bps,
        // The seam's streaming shape: no B-frames, periodic IDR, parameter
        // sets prepended to every IDR for mid-stream join.
        streaming: true,
        idr_interval_secs: request.keyframe_interval_seconds,
        prepend_header_to_idr: Some(true),
        effort_level: request.effort_level,
        color_vui: request.color_vui,
    }
}

struct VulkanVideoEncodeSession {
    encoder: SimpleEncoder,
    /// Resolves each source frame's published surface to the texture the
    /// encoder samples.
    gpu_context: GpuContextLimitedAccess,
}

impl VideoEncodeSession for VulkanVideoEncodeSession {
    fn coded_extent(&self) -> (u32, u32) {
        self.encoder.aligned_extent()
    }

    fn encode_published_surface(
        &mut self,
        source: &VideoEncodeSourceSurface<'_>,
    ) -> Result<Vec<EncodedVideoAccessUnitFromSession>> {
        // The registration's tracked layout is what the submit checks against
        // its sampling contract.
        let source_registration = self
            .gpu_context
            .resolve_texture_registration_by_surface_id(
                source.surface_id,
                source.texture_layout,
                source.width,
                source.height,
            )?;
        let packets = self.encoder.encode_source_texture(
            source_registration.texture(),
            source_registration.current_layout(),
            Some(source.timestamp_ns),
        )?;
        Ok(packets
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
    /// Acquires the pooled pixel buffers decoded pictures are staged into.
    gpu_context: GpuContextLimitedAccess,
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
                stage_decoded_frame_into_pooled_pixel_buffer(&self.gpu_context, decoded_frame)?,
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

/// Stage one read-back RGBA picture into a pooled pixel buffer whose pool id
/// becomes the frame's surface id. The extent is the session's own — already
/// the stream's conformance window, so an H.265 stream's CTU padding is gone
/// before a surface id ever names these pixels.
fn stage_decoded_frame_into_pooled_pixel_buffer(
    gpu_context: &GpuContextLimitedAccess,
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

    let (published_pixel_buffer_frame_id, pixel_buffer) =
        gpu_context.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;
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
            bitrate_bps: Some(4_000_000),
            keyframe_interval_seconds: 1,
            effort_level: Some(2),
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
}
