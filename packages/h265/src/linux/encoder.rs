// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.265 Encoder Processor
//
// Thin wrapper around the engine-free plugin SDK's hardware
// `EncoderSession` PluginAbiObject. The session is minted host-side via
// `GpuContextLimitedAccess::escalate(|full| full.create_encoder_session(..))`
// on the first VideoFrame, so its dimensions track the upstream frame size.
// Config width/height become guardrails (mismatch logs a warning, frame
// wins) mirroring how `frame.fps` flows through `mp4_writer`. The escalate
// window ends after the one-shot mint; per-frame `submit_texture` / `drain_packet`
// ride the session's own scope-free methods — never re-escalate per frame.
//
// The camera's GPU-resident texture is resolved by `surface_id` and handed
// to `submit_texture`, which resolves the encode-src image view host-side —
// no `host_vulkan_texture_arc` / raw-view bridge in the cdylib.


use crate::_generated_::{EncodedVideoFrame, VideoFrame};
use crate::linux::color_vui_translate::color_info_to_h273_repr;
use streamlib_plugin_sdk::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{EncoderSession, VulkanLayout};

use streamlib_plugin_abi::{
    VideoCodecRepr, VideoEncoderPresetRepr, VideoEncoderSessionDescriptorRepr,
};

// ============================================================================
// PROCESSOR
// ============================================================================

#[streamlib_plugin_sdk::sdk::processor("H265Encoder")]
pub struct H265EncoderProcessor {
    /// Vulkan Video hardware encoder session (minted lazily from the first
    /// frame). `!Clone` — owns exclusive Vulkan Video session / DPB /
    /// command resources.
    session: Option<EncoderSession>,

    /// GPU context for resolving VideoFrame textures and escalating to
    /// full access for the one-shot lazy encoder-session mint.
    gpu_context: Option<GpuContextLimitedAccess>,

    /// Frames encoded counter.
    frames_encoded: u64,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for H265EncoderProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        tracing::info!(
            "[H265Encoder] Setup complete (encoder-session mint deferred to first frame)"
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "[H265Encoder] Shutting down"
        );
        self.session.take();
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
            .ok_or_else(|| Error::Runtime("GPU context not initialized".into()))?
            .clone();

        if self.session.is_none() {
            let session = build_encoder_session_lazily(&gpu_ctx, &self.config, &frame)?;
            self.session = Some(session);
        }

        // Resolve the incoming frame's GPU texture; `submit_texture` resolves
        // the encode-src view host-side and requires the source in
        // SHADER_READ_ONLY_OPTIMAL (the layout resolved textures are left in).
        let texture = gpu_ctx.resolve_texture_by_surface_id(
            &frame.surface_id,
            frame.texture_layout,
            frame.width,
            frame.height,
        )?;

        let timestamp_ns: Option<i64> = frame.timestamp_ns.parse().ok();
        let frame_fps = frame.fps;
        // Pass color metadata through input → encoded so the muxer /
        // downstream consumer can populate VUI / colr without re-deriving
        // from the bitstream.
        let frame_color_info = frame.color_info.clone();
        let frame_mastering_display = frame.mastering_display.clone();
        let frame_content_light = frame.content_light.clone();

        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Runtime("H.265 encoder session not initialized".into()))?;

        let packet_count = session
            .submit_texture(&texture, VulkanLayout::SHADER_READ_ONLY_OPTIMAL, timestamp_ns)
            .map_err(|e| Error::Runtime(format!("H.265 encode failed: {e}")))?;

        for index in 0..packet_count {
            let packet = session
                .drain_packet(index)
                .map_err(|e| Error::Runtime(format!("H.265 drain packet failed: {e}")))?;
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
            tracing::info!("[H265Encoder] First frame encoded");
        } else if self.frames_encoded % 300 == 0 {
            tracing::info!(frames = self.frames_encoded, "[H265Encoder] Encode progress");
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
                "[H265Encoder] Config width does not match incoming frame width; using frame width"
            );
        }
    }
    if let Some(ch) = config_height {
        if ch != frame_height {
            tracing::warn!(
                config_height = ch,
                frame_height,
                "[H265Encoder] Config height does not match incoming frame height; using frame height"
            );
        }
    }
    let fps = frame_fps.unwrap_or_else(|| config_fps.unwrap_or(60));
    (frame_width, frame_height, fps)
}

/// Build the frozen encoder-session descriptor from config + first frame,
/// then mint the host `EncoderSession` inside a one-shot escalate window.
fn build_encoder_session_lazily(
    gpu_ctx: &GpuContextLimitedAccess,
    config: &crate::_generated_::H265EncoderConfig,
    frame: &VideoFrame,
) -> Result<EncoderSession> {
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
    // new SPS, which the encoder doesn't re-emit per frame. An all-absent
    // repr (every `*_present` byte 0) reads host-side as "no VUI".
    let color_vui = frame
        .color_info
        .as_ref()
        .map(color_info_to_h273_repr)
        .unwrap_or_default();

    let (has_bitrate, bitrate_bps) = match config.bitrate_bps {
        Some(bps) => (1, bps),
        None => (0, 0),
    };
    let (has_effort_level, effort_level) = match config.effort_level {
        Some(level) => (1, level),
        None => (0, 0),
    };
    let idr_interval_secs = config.keyframe_interval_seconds.unwrap_or(2.0) as u32;

    let descriptor = VideoEncoderSessionDescriptorRepr {
        width,
        height,
        fps,
        codec: VideoCodecRepr::H265 as u32,
        preset: VideoEncoderPresetRepr::Medium as u32,
        bitrate_bps,
        has_bitrate,
        idr_interval_secs,
        effort_level,
        has_effort_level,
        // `streaming = true`: header prepended to each IDR for mid-stream join.
        streaming: 1,
        prepend_header_present: 1,
        prepend_header: 1,
        // Texture input: keep the eager `prepare_gpu_encode_resources` the
        // host folds in when this byte is 0.
        disable_gpu_input_prealloc: 0,
        color_vui,
        ..Default::default()
    };

    // Mint the session in a one-shot escalate window — the scope-end drains
    // the device, so this runs at most once per session (first frame). Per-
    // frame `submit_texture` / `drain_packet` ride the session's own
    // scope-free methods. `escalate` returns `Result<Result<..>>`: the outer
    // is the escalate machinery, the inner is the mint.
    let session = match gpu_ctx.escalate(|full| full.create_encoder_session(&descriptor)) {
        Ok(Ok(session)) => session,
        Ok(Err(e)) => {
            return Err(Error::Runtime(format!(
                "Failed to create H.265 encoder session: {e}"
            )));
        }
        Err(e) => {
            return Err(Error::Runtime(format!(
                "escalate for H.265 encoder-session mint failed: {e}"
            )));
        }
    };

    let (aligned_width, aligned_height) = session.aligned_extent();
    tracing::info!(
        aligned_width,
        aligned_height,
        "[H265Encoder] Session minted lazily ({}x{}, {}fps, Vulkan Video hardware)",
        width,
        height,
        fps
    );
    // Debug: emit the cached VPS+SPS+PPS bytes as hex once at construction so
    // E2E flows can `ffprobe` the parameter sets without saving a full MP4.
    // One-shot trace; encoded packets at frame rate are not logged.
    match session.header() {
        Ok(header) => tracing::debug!(
            header_hex = %hex_encode(&header),
            header_len = header.len(),
            "[H265Encoder] Cached VPS+SPS+PPS header"
        ),
        Err(e) => tracing::debug!(error = %e, "[H265Encoder] Header fetch failed (non-fatal)"),
    }

    Ok(session)
}

/// Lowercase hex encoder for the one-shot VPS+SPS+PPS debug log. Returns an
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
