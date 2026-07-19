// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Decoder Processor
//
// Thin wrapper around the engine-free plugin SDK's hardware
// `DecoderSession` PluginAbiObject. The session is minted host-side in
// `setup()` via `GpuContextFullAccess::create_decoder_session` (a FullAccess
// lifecycle body already holds the gate — no escalate); coded dimensions
// auto-detect from the first SPS. Per bitstream chunk, `feed` decodes +
// stages `0..N` RGBA frames, pulled via `drain_frame`.
//
// Each decoded RGBA frame is staged into a pooled host-visible pixel buffer;
// the pool id doubles as the output `VideoFrame.surface_id`. Downstream
// consumers resolve that surface_id, at which point the host uploads the
// buffer to a GPU texture (SHADER_READ_ONLY_OPTIMAL) — the same CPU→GPU
// hand-off the camera uses. No engine-only `TextureRing` /
// `copy_pixel_buffer_to_slot` reach from the cdylib.


use crate::_generated_::{EncodedVideoFrame, VideoFrame};
use crate::linux::color_vui_translate::decoded_vui_to_color_info;
use streamlib_plugin_sdk::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{DecoderSession, PixelFormat};

use streamlib_plugin_abi::{VideoCodecRepr, VideoDecoderSessionDescriptorRepr};

// ============================================================================
// PROCESSOR
// ============================================================================

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/h264/H264Decoder",
    description = "Decodes EncodedVideoFrame (H.264) to VideoFrame via Vulkan Video",
    execution = reactive,
    scheduling = high,
    config = crate::_generated_::H264DecoderConfig,
    input("encoded_video_in", "@tatolab/core/EncodedVideoFrame", read_mode = "read_next_in_order", buffer_size = 16, description = "H.264 encoded video frames to decode"),
    output("video_out", "@tatolab/core/VideoFrame", description = "Decoded video frames"),
)]
pub struct H264DecoderProcessor {
    /// Vulkan Video hardware decoder session (minted in `setup`). `!Clone` —
    /// owns exclusive Vulkan Video session / DPB / command resources.
    session: Option<DecoderSession>,

    /// GPU context for staging decoded frames into pooled pixel buffers.
    gpu_context: Option<GpuContextLimitedAccess>,

    /// Frames decoded counter.
    frames_decoded: u64,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for H264DecoderProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());

        // Decoder dimensions come from the H.264 SPS — leaving `max_width` /
        // `max_height` at zero tells the host to size the DPB and video
        // session from the first parsed SPS rather than pre-allocating for a
        // hard-coded resolution cap. `rgba_output = 1`: drained frames are
        // GPU NV12→RGBA converted host-side.
        let descriptor = VideoDecoderSessionDescriptorRepr {
            codec: VideoCodecRepr::H264 as u32,
            rgba_output: 1,
            ..Default::default()
        };

        // FullAccess lifecycle body: call the create slot directly — the
        // dispatcher already holds the gate, so no `escalate` here.
        let session = ctx
            .gpu_full_access()
            .create_decoder_session(&descriptor)
            .map_err(|e| Error::Runtime(format!("Failed to create H.264 decoder session: {e}")))?;

        tracing::info!("[H264Decoder] Session minted (Vulkan Video hardware)");

        self.session = Some(session);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            "[H264Decoder] Shutting down"
        );
        self.session.take();
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
            .ok_or_else(|| Error::Runtime("GPU context not initialized".into()))?
            .clone();

        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Runtime("H.264 decoder session not initialized".into()))?;

        let frame_count = session
            .feed(&encoded.data)
            .map_err(|e| Error::Runtime(format!("H.264 decode failed: {e}")))?;

        // Color info: prefer the parsed bitstream VUI (self-describing,
        // survives muxer round-trips that re-encode `EncodedVideoFrame.
        // color_info`) over the producer's attestation. Falls back to the
        // passthrough when the bitstream didn't carry a VUI. The VUI is
        // best-effort metadata — a query error (e.g. a null methods vtable)
        // must not drop otherwise-valid decoded frames, so warn + fall back.
        let parsed_vui = match session.current_color_vui() {
            Ok(vui) => vui,
            Err(e) => {
                tracing::warn!(error = %e, "[H264Decoder] color VUI query failed; using passthrough");
                None
            }
        };
        let color_info_source = if parsed_vui.is_some() {
            "bitstream"
        } else {
            "encoded_passthrough"
        };
        let color_info = parsed_vui
            .map(|vui| decoded_vui_to_color_info(&vui))
            .or_else(|| encoded.color_info.clone());

        for index in 0..frame_count {
            let decoded = session
                .drain_frame(index)
                .map_err(|e| Error::Runtime(format!("H.264 drain frame failed: {e}")))?;
            let width = decoded.width;
            let height = decoded.height;

            // Decoded frames come back as RGBA (host GPU NV12→RGBA convert).
            let rgba_size = (width * height * 4) as usize;
            let src = &decoded.data[..rgba_size.min(decoded.data.len())];

            // Stage RGBA into a pooled host-visible pixel buffer. The pool id
            // is the output surface_id: downstream `resolve_texture_by_surface_id`
            // triggers the host to upload this buffer into a GPU texture. The
            // `pixel_buffer` handle stays live through the `outputs.write`
            // below so the pool can't rotate this slot out mid-flight (the
            // pool skips buffers whose Arc is still held).
            let (pool_id, pixel_buffer) =
                gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;
            let dst_ptr = pixel_buffer.plane_base_address(0);
            if dst_ptr.is_null() {
                return Err(Error::Runtime(
                    "H.264 decoder: pixel buffer plane base address is null".into(),
                ));
            }
            let copy_len = src.len().min(pixel_buffer.plane_size(0) as usize);
            // SAFETY: `dst_ptr` is the mapped host-visible base of a pixel
            // buffer sized (width, height, Rgba32) = `width*height*4` bytes;
            // `copy_len` is clamped to both the RGBA source and the plane
            // size, and the regions do not overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst_ptr, copy_len);
            }
            let surface_id = pool_id.to_string();

            let video_frame = VideoFrame {
                surface_id,
                width,
                height,
                timestamp_ns: encoded.timestamp_ns.clone(),
                fps: encoded.fps,
                // Per-frame override is opt-in; per-surface
                // `current_image_layout` from surface-share is the default.
                texture_layout: None,
                color_info: color_info.clone(),
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
                    "[H264Decoder] First frame decoded — surfaced color_info"
                );
            }
            // `pixel_buffer` drops here (after the write) — matches the
            // camera's in-flight hold.
        }

        if self.frames_decoded % 300 == 0 && self.frames_decoded > 0 {
            tracing::info!(frames = self.frames_decoded, "[H264Decoder] Decode progress");
        }

        Ok(())
    }
}
