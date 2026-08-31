// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in H.264 encoder: published video surfaces in, encoded-frame bags
//! out, via the engine's Vulkan Video session surface.
//!
//! The session mints lazily inside a one-shot `escalate` window on the first
//! frame, so its dimensions track upstream; config width/height are
//! guardrails (mismatch warns, the frame wins). Per-frame submits ride the
//! session's own methods and never re-escalate. An upstream that changes
//! extent mid-run gets a fresh session at the new extent — new parameter
//! sets, opening at a sync point — rather than a refusal.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::engine::video::{
    Codec, EncodePacket, Preset, SimpleEncoder, SimpleEncoderConfig,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;

use crate::encoded_video_frame::{
    EncodedFrameOrderingPairCounter, EncodedVideoCodec, EncodedVideoFrame,
};
use crate::h273_color_vui_translation::color_info_to_h273_color_vui;
use crate::video_frame::{ColorInfo, VideoFrame};

/// Fallback frame rate when neither the frame nor the config states one.
const DEFAULT_ENCODE_FPS: u32 = 60;

/// Seconds between IDR sync points when the config states none.
const DEFAULT_IDR_INTERVAL_SECONDS: u32 = 2;

/// Encode-progress log cadence, in frames.
const ENCODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// Configuration for [`H264Encoder`]. Everything is optional: dimensions
/// and rate track the upstream frames, and the knobs below are the
/// guardrail set the session surface accepts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct H264EncoderConfig {
    /// Expected frame width — a guardrail, not a resize: a mismatching
    /// frame wins with a warning.
    #[serde(default)]
    pub width: Option<u32>,
    /// Expected frame height — a guardrail like `width`.
    #[serde(default)]
    pub height: Option<u32>,
    /// Frame rate used when the incoming frames carry none.
    #[serde(default)]
    pub fps: Option<u32>,
    /// Target bitrate in bits per second. Absent: constant-QP encoding at
    /// the session's balanced preset.
    #[serde(default)]
    pub bitrate_bps: Option<u32>,
    /// Seconds between IDR sync points. Absent: 2.
    #[serde(default)]
    pub keyframe_interval_seconds: Option<u32>,
    /// Vulkan encoder-effort index (driver analysis budget, not an H.264
    /// quality knob). Absent: the codec's default.
    #[serde(default)]
    pub effort_level: Option<u32>,
}

/// One minted encoder session plus the upstream facts it was minted from —
/// what a later frame is checked against to notice a renegotiated upstream.
struct H264EncodeSessionMintedFromUpstream {
    session: SimpleEncoder,
    /// The source extent the session was minted for.
    minted_width: u32,
    minted_height: u32,
    /// The codec-aligned coded extent, published on every bag as the extent
    /// before the conformance crop.
    coded_width: u32,
    coded_height: u32,
    /// The color the session's SPS VUI was minted from — what every bag of
    /// this session carries, because it describes the bitstream.
    minted_color: Option<ColorInfo>,
    /// A mid-session color change is warned once, not per frame.
    color_change_already_warned: bool,
}

#[streamlib::sdk::processor(
    description = "Encodes published video surfaces to H.264 Annex-B encoded-frame bags via Vulkan Video hardware encode",
    execution = reactive,
    scheduling = high,
    config = crate::h264_encoder::H264EncoderConfig,
    input("video", delivery_profile = "ordered", description = "Video frames to encode"),
    output("encoded_video", description = "H.264 encoded-frame bags"),
)]
pub struct H264Encoder {
    encode_session: Option<H264EncodeSessionMintedFromUpstream>,
    gpu_context: Option<GpuContextLimitedAccess>,
    ordering_pair_counter: EncodedFrameOrderingPairCounter,
    frames_encoded: u64,
}

impl ReactiveProcessor for H264Encoder::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // The encoder-session mint is deferred to the first frame so its
        // dimensions track upstream; setup only keeps the context handle
        // the mint and the per-frame resolve need.
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "H264Encoder: teardown"
        );
        self.encode_session.take();
        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video")?;

        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Runtime("H264Encoder: GPU context not initialized".into()))?
            .clone();

        let session_is_stale = self
            .encode_session
            .as_ref()
            .is_some_and(|minted| !minted.matches_source_extent(frame.width, frame.height));
        if session_is_stale {
            let minted = self.encode_session.take().expect("checked above");
            tracing::info!(
                minted_width = minted.minted_width,
                minted_height = minted.minted_height,
                frame_width = frame.width,
                frame_height = frame.height,
                "H264Encoder: upstream extent changed — re-minting the encoder session"
            );
        }
        if self.encode_session.is_none() {
            self.encode_session = Some(mint_encode_session_from_first_frame(
                &gpu_context,
                &self.config,
                &frame,
            )?);
        }
        let (packets, session_bag_fields) = {
            let minted = self.encode_session.as_mut().expect("minted above");
            minted.warn_once_on_color_change(frame.color_info.as_ref());

            // Resolve the frame's published surface; the registration's
            // tracked layout is what the submit checks against its sampling
            // contract.
            let source_registration = gpu_context.resolve_texture_registration_by_surface_id(
                &frame.surface_id,
                frame.texture_layout,
                frame.width,
                frame.height,
            )?;
            let packets = minted
                .session
                .encode_source_texture(
                    source_registration.texture(),
                    source_registration.current_layout(),
                    Some(frame.timestamp_ns),
                )
                .map_err(|e| Error::Runtime(format!("H264Encoder: encode failed: {e}")))?;
            (packets, minted_bag_fields(minted))
        };

        for packet in packets {
            self.publish_encoded_frame_bag(&session_bag_fields, packet, frame.timestamp_ns)?;
        }

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("H264Encoder: first frame encoded");
        } else if self
            .frames_encoded
            .is_multiple_of(ENCODE_PROGRESS_LOG_INTERVAL_FRAMES)
        {
            tracing::info!(
                frames_encoded = self.frames_encoded,
                "H264Encoder: encode progress"
            );
        }
        Ok(())
    }
}

/// The per-bag facts read off the minted session, split out so the publish
/// helper can borrow them while the session stays mutably borrowed elsewhere.
struct MintedSessionBagFields {
    coded_width: u32,
    coded_height: u32,
    color: Option<ColorInfo>,
}

fn minted_bag_fields(minted: &H264EncodeSessionMintedFromUpstream) -> MintedSessionBagFields {
    MintedSessionBagFields {
        coded_width: minted.coded_width,
        coded_height: minted.coded_height,
        color: minted.minted_color.clone(),
    }
}

impl H264Encoder::Processor {
    /// Publish one encoded packet as an encoded-frame bag. The timestamp
    /// rides the frame header — the packet's own when it carries one, the
    /// source frame's otherwise — never a bag field.
    fn publish_encoded_frame_bag(
        &mut self,
        session_fields: &MintedSessionBagFields,
        packet: EncodePacket,
        source_frame_timestamp_ns: i64,
    ) -> Result<()> {
        let ordering_pair = self
            .ordering_pair_counter
            .account_published_frame(packet.is_keyframe);
        let encoded_frame = EncodedVideoFrame {
            codec: EncodedVideoCodec::H264,
            annex_b_access_unit_bytes: packet.data,
            is_sync_point: packet.is_keyframe,
            group_index: ordering_pair.group_index,
            sequence_index: ordering_pair.sequence_index,
            width: session_fields.coded_width,
            height: session_fields.coded_height,
            color: session_fields.color.clone(),
        };
        self.outputs.write_with_timestamp(
            "encoded_video",
            &encoded_frame,
            packet.timestamp_ns.unwrap_or(source_frame_timestamp_ns),
        )
    }
}

impl H264EncodeSessionMintedFromUpstream {
    fn matches_source_extent(&self, frame_width: u32, frame_height: u32) -> bool {
        self.minted_width == frame_width && self.minted_height == frame_height
    }

    /// A frame whose color disagrees with the minted SPS VUI is encoded
    /// anyway — switching colorimetry needs new parameter sets the session
    /// does not re-emit — and the disagreement is said once, not per frame.
    fn warn_once_on_color_change(&mut self, frame_color: Option<&ColorInfo>) {
        if self.color_change_already_warned {
            return;
        }
        if frame_color != self.minted_color.as_ref() {
            tracing::warn!(
                minted_color = ?self.minted_color,
                frame_color = ?frame_color,
                "H264Encoder: frame color differs from the color the session's SPS VUI \
                 was minted from; the bitstream keeps the minted color"
            );
            self.color_change_already_warned = true;
        }
    }
}

/// Resolve the session's (width, height, fps) from the first frame, with
/// config values as guardrails: a mismatching frame wins with a warning,
/// and fps falls back frame → config → default.
fn resolve_encode_dimensions_from_first_frame(
    config: &H264EncoderConfig,
    frame_width: u32,
    frame_height: u32,
    frame_fps: Option<u32>,
) -> (u32, u32, u32) {
    if let Some(config_width) = config.width
        && config_width != frame_width
    {
        tracing::warn!(
            config_width,
            frame_width,
            "H264Encoder: config width does not match the incoming frame; using the frame's"
        );
    }
    if let Some(config_height) = config.height
        && config_height != frame_height
    {
        tracing::warn!(
            config_height,
            frame_height,
            "H264Encoder: config height does not match the incoming frame; using the frame's"
        );
    }
    let fps = frame_fps.unwrap_or_else(|| config.fps.unwrap_or(DEFAULT_ENCODE_FPS));
    (frame_width, frame_height, fps)
}

/// Mint the encoder session from the first frame inside a one-shot escalate
/// window; per-frame submits ride the session's own methods afterwards.
fn mint_encode_session_from_first_frame(
    gpu_context: &GpuContextLimitedAccess,
    config: &H264EncoderConfig,
    frame: &VideoFrame,
) -> Result<H264EncodeSessionMintedFromUpstream> {
    let (width, height, fps) =
        resolve_encode_dimensions_from_first_frame(config, frame.width, frame.height, frame.fps);

    // The first frame's color drives the session-level SPS VUI; an absent
    // color emits no colour_description block rather than an empty one.
    let color_vui = frame
        .color_info
        .as_ref()
        .map(color_info_to_h273_color_vui)
        .filter(|vui| vui.is_video_signal_type_block_needed());

    let session_config = SimpleEncoderConfig {
        width,
        height,
        fps,
        codec: Codec::H264,
        preset: Preset::Medium,
        qp: None,
        bitrate_bps: config.bitrate_bps,
        // Streaming shape: no B-frames, periodic IDR, parameter sets
        // prepended to every IDR for mid-stream join — what makes every
        // `is_sync_point` bag a self-sufficient decode entry point.
        streaming: true,
        idr_interval_secs: config
            .keyframe_interval_seconds
            .unwrap_or(DEFAULT_IDR_INTERVAL_SECONDS),
        prepend_header_to_idr: Some(true),
        effort_level: config.effort_level,
        color_vui,
    };

    // One-shot mint: the escalate scope-end drains the device, so this runs
    // once per session, never per frame. `true` pre-allocates the RGB→NV12
    // converter so the first submit skips its allocation latency.
    let session = gpu_context
        .escalate(|full| full.create_encoder_session(session_config, true))
        .map_err(|e| {
            Error::Runtime(format!(
                "H264Encoder: failed to mint the encoder session: {e}"
            ))
        })?;

    let (coded_width, coded_height) = session.aligned_extent();
    tracing::info!(
        width,
        height,
        fps,
        coded_width,
        coded_height,
        "H264Encoder: session minted lazily from the first frame"
    );
    Ok(H264EncodeSessionMintedFromUpstream {
        session,
        minted_width: frame.width,
        minted_height: frame.height,
        coded_width,
        coded_height,
        minted_color: frame.color_info.clone(),
        color_change_already_warned: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_dimensions_win_over_config_guardrails() {
        let config = H264EncoderConfig {
            width: Some(1920),
            height: Some(1080),
            fps: Some(60),
            ..H264EncoderConfig::default()
        };
        let (width, height, fps) =
            resolve_encode_dimensions_from_first_frame(&config, 1280, 720, Some(30));
        assert_eq!((width, height, fps), (1280, 720, 30));
    }

    #[test]
    fn fps_falls_back_frame_then_config_then_default() {
        let config_with_fps = H264EncoderConfig {
            fps: Some(24),
            ..H264EncoderConfig::default()
        };
        let (_, _, fps_from_config) =
            resolve_encode_dimensions_from_first_frame(&config_with_fps, 1280, 720, None);
        assert_eq!(fps_from_config, 24);

        let (_, _, fps_default) = resolve_encode_dimensions_from_first_frame(
            &H264EncoderConfig::default(),
            1280,
            720,
            None,
        );
        assert_eq!(fps_default, DEFAULT_ENCODE_FPS);
    }

    #[test]
    fn an_empty_config_still_uses_the_frame_dimensions() {
        let (width, height, _) = resolve_encode_dimensions_from_first_frame(
            &H264EncoderConfig::default(),
            3840,
            2160,
            Some(30),
        );
        assert_eq!((width, height), (3840, 2160));
    }

    /// The config map is open and fully optional — `rt.add(H264Encoder)`
    /// with no config at all must deserialize.
    #[test]
    fn an_all_absent_config_deserializes_to_defaults() {
        let config: H264EncoderConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config, H264EncoderConfig::default());
    }
}
