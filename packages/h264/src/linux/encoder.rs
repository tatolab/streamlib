// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Encoder Processor
//
// Thin wrapper around streamlib::sdk::engine::video::SimpleEncoder using the shared RHI
// HostVulkanDevice. The encoder shares streamlib's Vulkan device, VMA allocator,
// and queues — no separate device creation, no NVIDIA dual-device crash.
//
// The encoder is constructed lazily on the first VideoFrame so its session
// dimensions track the upstream frame size. Config width/height become
// guardrails (mismatch logs a warning, frame wins) mirroring how `frame.fps`
// flows through `mp4_writer`. Privileged resource construction runs inside
// `GpuContextLimitedAccess::escalate(|full| …)` so the processor-setup mutex
// and `device_wait_idle` order it against the rest of the GPU work.
//
// The camera's GPU-resident textures are on the same device, so encode_image()
// accepts them directly (zero-copy).


use crate::_generated_::{EncodedVideoFrame, VideoFrame};
use crate::linux::color_vui_translate::color_info_to_h273;
use streamlib::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::engine::HostTextureExt;
use streamlib::sdk::error::{Error, Result};

use streamlib::sdk::engine::video::{Codec, Preset, SimpleEncoder, SimpleEncoderConfig};

// ============================================================================
// PROCESSOR
// ============================================================================

#[streamlib::sdk::processor("H264Encoder")]
pub struct H264EncoderProcessor {
    /// Vulkan Video hardware encoder (built lazily from the first frame).
    encoder: Option<SimpleEncoder>,

    /// GPU context for resolving VideoFrame textures and escalating to
    /// full access for the one-shot lazy encoder construction.
    gpu_context: Option<GpuContextLimitedAccess>,

    /// Frames encoded counter.
    frames_encoded: u64,
}

impl streamlib::sdk::processors::ReactiveProcessor for H264EncoderProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        tracing::info!(
            "[H264Encoder] Setup complete (encoder construction deferred to first frame)"
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "[H264Encoder] Shutting down"
        );
        self.encoder.take();
        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Runtime("GPU context not initialized".into()))?;

        if self.encoder.is_none() {
            let encoder = build_encoder_lazily(gpu_ctx, &self.config, &frame)?;
            self.encoder = Some(encoder);
        }

        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| Error::Runtime("H.264 encoder not initialized".into()))?;

        let texture = gpu_ctx.resolve_texture_by_surface_id(
            &frame.surface_id,
            frame.texture_layout,
            frame.width,
            frame.height,
        )?;
        // Cdylib-safe: `Texture::vulkan_inner()` reaches `host_inner()` which
        // panics under `host_callbacks().is_some()`. Route through the
        // `host_vulkan_texture_arc` FullAccess slot so cdylib callers don't
        // trip the panic guard.
        let host_texture = texture.host_vulkan_texture_arc().map_err(|e| {
            Error::GpuError(format!("Failed to acquire host texture for encode: {e}"))
        })?;
        let image_view = host_texture.image_view().map_err(|e| {
            Error::GpuError(format!("Failed to get image view: {e}"))
        })?;

        let timestamp_ns: Option<i64> = frame.timestamp_ns.parse().ok();
        let frame_fps = frame.fps;
        // Pass color metadata through input → encoded so the muxer /
        // downstream consumer can populate VUI / colr without re-
        // deriving from the bitstream. VUI write-back from encoder
        // config lands in a follow-up.
        let frame_color_info = frame.color_info.clone();
        let frame_mastering_display = frame.mastering_display.clone();
        let frame_content_light = frame.content_light.clone();

        let packets = encoder.encode_image(image_view, timestamp_ns).map_err(|e| {
            Error::Runtime(format!("H.264 encode failed: {e}"))
        })?;

        for packet in packets {
            let encoded = EncodedVideoFrame {
                data: packet.data,
                fps: frame_fps,
                is_keyframe: packet.is_keyframe,
                timestamp_ns: packet.timestamp_ns.unwrap_or(0).to_string(),
                frame_number: self.frames_encoded.to_string(),
                color_info: frame_color_info.clone(),
                mastering_display: frame_mastering_display.clone(),
                content_light: frame_content_light.clone(),
            };
            self.outputs.write("encoded_video_out", &encoded)?;
        }

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("[H264Encoder] First frame encoded");
        } else if self.frames_encoded % 300 == 0 {
            tracing::info!(frames = self.frames_encoded, "[H264Encoder] Encode progress");
        }

        Ok(())
    }
}

/// Resolve the encoder's (width, height, fps) from the first frame, treating
/// `config.width` / `config.height` / `config.fps` as guardrails. Frame wins on
/// mismatch (mirrors `frame.fps.unwrap_or(self.config.fps)` in `mp4_writer`).
fn select_encoder_dims(
    config_width: Option<u32>,
    config_height: Option<u32>,
    config_fps: Option<u32>,
    frame_width: u32,
    frame_height: u32,
    frame_fps: Option<u32>,
) -> (u32, u32, u32) {
    if let Some(cw) = config_width {
        if cw != frame_width {
            tracing::warn!(
                config_width = cw,
                frame_width,
                "[H264Encoder] Config width does not match incoming frame width; using frame width"
            );
        }
    }
    if let Some(ch) = config_height {
        if ch != frame_height {
            tracing::warn!(
                config_height = ch,
                frame_height,
                "[H264Encoder] Config height does not match incoming frame height; using frame height"
            );
        }
    }
    let fps = frame_fps.unwrap_or_else(|| config_fps.unwrap_or(60));
    (frame_width, frame_height, fps)
}

fn build_encoder_lazily(
    gpu_ctx: &GpuContextLimitedAccess,
    config: &crate::_generated_::H264EncoderConfig,
    frame: &VideoFrame,
) -> Result<SimpleEncoder> {
    let (width, height, fps) = select_encoder_dims(
        config.width,
        config.height,
        config.fps,
        frame.width,
        frame.height,
        frame.fps,
    );

    // First-frame color info drives the session-level SPS VUI. Mid-stream
    // ColorInfo changes are not honored — switching colorimetry requires a
    // new SPS, which the encoder doesn't re-emit per frame.
    let color_vui = frame
        .color_info
        .as_ref()
        .and_then(color_info_to_h273);

    let encoder_config = SimpleEncoderConfig {
        width,
        height,
        fps,
        codec: Codec::H264,
        preset: Preset::Medium,
        streaming: true,
        idr_interval_secs: config.keyframe_interval_seconds.unwrap_or(2.0) as u32,
        bitrate_bps: config.bitrate_bps,
        prepend_header_to_idr: Some(true),
        effort_level: config.effort_level,
        color_vui,
        ..Default::default()
    };

    let encoder = gpu_ctx.escalate(|full| {
        let mut encoder = SimpleEncoder::from_full_access(full, encoder_config)
            .map_err(|e| Error::Runtime(format!("Failed to create H.264 encoder: {e}")))?;

        encoder
            .prepare_gpu_encode_resources()
            .map_err(|e| Error::Runtime(format!("Failed to pre-allocate H.264 encode resources: {e}")))?;

        Ok(encoder)
    })?;

    tracing::info!(
        "[H264Encoder] Initialized lazily ({}x{}, {}fps, shared RHI device, Vulkan Video hardware)",
        width,
        height,
        fps
    );
    tracing::info!(
        color_vui = ?color_vui,
        "[H264Encoder] SPS VUI color metadata chained from first-frame color_info"
    );
    // Debug: emit the cached SPS+PPS bytes as hex once at construction so
    // E2E flows can `ffprobe` the SPS VUI without saving a full MP4. This
    // is a one-shot trace; encoded packets at frame rate are not logged.
    tracing::debug!(
        header_hex = %hex_encode(encoder.header()),
        header_len = encoder.header().len(),
        "[H264Encoder] Cached SPS+PPS header"
    );

    Ok(encoder)
}

/// Lowercase hex encoder for the one-shot SPS+PPS debug log. Returns an
/// empty string on empty input.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::select_encoder_dims;

    #[test]
    fn frame_dimensions_win_over_config() {
        let (w, h, fps) =
            select_encoder_dims(Some(1920), Some(1080), Some(60), 1280, 720, Some(30));
        assert_eq!((w, h, fps), (1280, 720, 30));
    }

    #[test]
    fn frame_fps_falls_back_to_config_then_default() {
        let (_, _, fps_from_config) =
            select_encoder_dims(None, None, Some(24), 1280, 720, None);
        assert_eq!(fps_from_config, 24);

        let (_, _, fps_default) = select_encoder_dims(None, None, None, 1280, 720, None);
        assert_eq!(fps_default, 60);
    }

    #[test]
    fn missing_config_dims_still_use_frame_dims() {
        let (w, h, _) = select_encoder_dims(None, None, None, 3840, 2160, Some(30));
        assert_eq!((w, h), (3840, 2160));
    }
}
