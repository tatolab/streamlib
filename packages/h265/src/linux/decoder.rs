// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.265 Decoder Processor
//
// Thin wrapper around streamlib::sdk::engine::video::SimpleDecoder using the shared RHI
// HostVulkanDevice. Decoded NV12 frames are written to pixel buffers for output.


use crate::_generated_::{EncodedVideoFrame, VideoFrame};
use crate::linux::color_vui_translate::h273_to_color_info;
use streamlib::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess, TextureRing,
};
use streamlib::sdk::engine::HostPixelBufferRefExt;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::{PixelFormat, TextureFormat, TextureUsages};

use streamlib::sdk::engine::video::{Codec, SimpleDecoder, SimpleDecoderConfig};

/// Pre-allocated output texture ring depth. Matches `MAX_FRAMES_IN_FLIGHT`
/// — one slot for the GPU's in-flight upload, one for the next decoded
/// frame the processor is staging. See `docs/learnings/vulkan-frames-in-flight.md`.
const RING_DEPTH: usize = 2;

// ============================================================================
// PROCESSOR
// ============================================================================

#[streamlib::sdk::processor("H265Decoder")]
pub struct H265DecoderProcessor {
    /// Vulkan Video hardware decoder (shares RHI device).
    decoder: Option<SimpleDecoder>,

    /// GPU context for creating pixel buffers for decoded frames.
    gpu_context: Option<GpuContextLimitedAccess>,

    /// Pre-allocated output texture ring. Built lazily on the first
    /// decoded frame (dimensions come from the SPS), rebuilt only on
    /// mid-stream resolution change.
    texture_ring: Option<TextureRing>,

    /// Frames decoded counter.
    frames_decoded: u64,
}

impl streamlib::sdk::processors::ReactiveProcessor for H265DecoderProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());

        // Decoder dimensions come from H.265 SPS — leaving `max_width` /
        // `max_height` at zero tells `SimpleDecoder` to size the DPB and
        // video session from the first parsed SPS rather than pre-allocating
        // for a hard-coded resolution cap.
        let decoder_config = SimpleDecoderConfig {
            codec: Codec::H265,
            rgba_output: true,
            max_width: 0,
            max_height: 0,
            ..Default::default()
        };

        let decoder = SimpleDecoder::from_full_access(ctx.gpu_full_access(), decoder_config)
            .map_err(|e| Error::Runtime(format!("Failed to create H.265 decoder: {e}")))?;

        // Session creation, DPB allocation, and the NV12→RGBA converter are
        // built lazily inside `SimpleDecoder::feed()` once the first SPS
        // arrives — at that point the actual coded extent is known and
        // sized to match. The processor-setup mutex inside `escalate` and
        // the RHI queue submitter coordinate device-side ordering, so the
        // historical pre-swapchain pre-init is no longer required.

        tracing::info!("[H265Decoder] Initialized (shared RHI device, Vulkan Video hardware)");

        self.decoder = Some(decoder);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            "[H265Decoder] Shutting down"
        );
        self.decoder.take();
        self.texture_ring.take();
        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("encoded_video_in") {
            return Ok(());
        }
        let encoded: EncodedVideoFrame = self.inputs.read("encoded_video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Runtime("GPU context not initialized".into()))?;

        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| Error::Runtime("H.265 decoder not initialized".into()))?;

        let decoded_frames = decoder.feed(&encoded.data).map_err(|e| {
            Error::Runtime(format!("H.265 decode failed: {e}"))
        })?;

        for decoded in decoded_frames {
            let width = decoded.width;
            let height = decoded.height;

            // Decoded frames come back as RGBA (GPU NV12→RGBA via Nv12ToRgbConverter).
            let rgba_size = (width * height * 4) as usize;
            let src = &decoded.data[..rgba_size.min(decoded.data.len())];

            // Acquire (or build, on first frame / resolution change) the
            // output texture ring. SPS-driven dimensions are stable within
            // a session, so the escalate path runs at most once per stream
            // and steady-state decode stays Limited-only.
            let need_rebuild = match self.texture_ring.as_ref() {
                Some(ring) => ring.width() != width || ring.height() != height,
                None => true,
            };
            if need_rebuild {
                self.texture_ring = Some(gpu_ctx.escalate(|full| {
                    full.create_texture_ring(
                        width,
                        height,
                        TextureFormat::Rgba8Unorm,
                        TextureUsages::COPY_DST
                            | TextureUsages::TEXTURE_BINDING
                            | TextureUsages::STORAGE_BINDING,
                        RING_DEPTH,
                    )
                })?);
            }
            let ring = self.texture_ring.as_ref().unwrap();
            let slot = ring.acquire_next();

            // Stage RGBA into a host-visible pixel buffer, then copy into
            // the ring slot's pre-allocated DEVICE_LOCAL texture via the
            // ring's amortized upload primitive — no per-frame escalation
            // AND no per-frame vkCreateCommandPool / vkAllocateCommandBuffers
            // / vkCreateFence (the slot's command pool + cb + fence are
            // pre-allocated by `create_texture_ring`, reset+reused per call).
            let (_pool_id, pixel_buffer) =
                gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;
            let dst_ptr = pixel_buffer.buffer_ref().vulkan_inner().mapped_ptr();
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst_ptr, src.len());
            }
            ring.copy_pixel_buffer_to_slot(&slot, &pixel_buffer, width, height)?;
            let surface_id = slot.surface_id().to_string();

            let timestamp_ns = encoded.timestamp_ns.clone();

            // Color info: prefer the parsed bitstream VUI (self-describing,
            // survives muxer round-trips that re-encode `EncodedVideoFrame.
            // color_info`) over the producer's attestation. Falls back to the
            // passthrough when the bitstream didn't carry a VUI.
            let parsed_vui = decoder.current_color_vui();
            let color_info_source = if parsed_vui.is_some() {
                "bitstream"
            } else {
                "encoded_passthrough"
            };
            let color_info = parsed_vui
                .map(|vui| h273_to_color_info(&vui))
                .or_else(|| encoded.color_info.clone());

            // Carry the encoder's input-frame index (`frame_number`) through to the
            // decoded output so downstream consumers (PSNR rig, display PNG sampler)
            // can pair each decoded frame with the reference input.
            let video_frame = VideoFrame {
                surface_id,
                width,
                height,
                timestamp_ns,
                frame_index: encoded.frame_number.clone(),
                fps: encoded.fps,
                // Per-frame override is opt-in; per-surface
                // `current_image_layout` from surface-share is the default.
                texture_layout: None,
                color_info,
                mastering_display: encoded.mastering_display.clone(),
                content_light: encoded.content_light.clone(),
            };

            let log_color = self.frames_decoded == 0;
            self.outputs.write("video_out", &video_frame)?;
            self.frames_decoded += 1;
            if log_color {
                tracing::info!(
                    color_info = ?video_frame.color_info,
                    source = color_info_source,
                    "[H265Decoder] First frame decoded — surfaced color_info"
                );
            }
        }

        if self.frames_decoded % 300 == 0 && self.frames_decoded > 0 {
            tracing::info!(frames = self.frames_decoded, "[H265Decoder] Decode progress");
        }

        Ok(())
    }
}
