// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! The encode body both hardware video encoder built-ins are: published
//! video surfaces in, encoded-frame bags out, via the engine's Vulkan Video
//! session surface.
//!
//! The session mints lazily inside a one-shot `escalate` window on the first
//! frame, so its dimensions track upstream; config width/height are
//! guardrails (mismatch warns, the frame wins). Per-frame submits ride the
//! session's own methods and never re-escalate. An upstream that changes
//! extent mid-run gets a fresh session at the new extent — new parameter
//! sets, opening at a sync point — rather than a refusal.
//!
//! Ports live on the processors that own this body; nothing here reads or
//! writes a link. What it hands back is a list of bags to publish, and the
//! timestamp each one rides under.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::engine::video::{EncodePacket, Preset, SimpleEncoder, SimpleEncoderConfig};
use streamlib::sdk::error::{Error, Result};

use crate::encoded_stream_ordering::EncodedStreamOrderingPairCounter;
use crate::encoded_video_frame::EncodedVideoFrame;
use crate::h273_color_vui_translation::color_info_to_h273_color_vui;
use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;
use crate::video_frame::{ColorInfo, VideoFrame};

/// Fallback frame rate when neither the frame nor the config states one.
const DEFAULT_ENCODE_FPS: u32 = 60;

/// Seconds between IDR sync points when the config states none.
const DEFAULT_IDR_INTERVAL_SECONDS: u32 = 2;

/// Encode-progress log cadence, in frames.
const ENCODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// Configuration for the hardware video encoder built-ins. Everything is
/// optional: dimensions and rate track the upstream frames, and the knobs
/// below are the guardrail set the session surface accepts. Both codecs take
/// exactly these, because the session surface takes exactly these.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HardwareVideoEncoderConfig {
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
    /// Vulkan encoder-effort index (driver analysis budget, not a codec
    /// quality knob). Absent: the codec's default.
    #[serde(default)]
    pub effort_level: Option<u32>,
}

/// One encoded frame the caller still has to write, and the timestamp it
/// rides under — the packet's own when it carries one, the source frame's
/// otherwise. The timestamp rides the frame header, never a bag field.
pub struct EncodedFrameAwaitingPublication {
    pub frame: EncodedVideoFrame,
    pub timestamp_ns: i64,
}

/// One minted encoder session plus the upstream facts it was minted from —
/// what a later frame is checked against to notice a renegotiated upstream.
struct EncodeSessionMintedFromUpstream {
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

/// The per-bag facts read off the minted session, split out so the publish
/// helper can borrow them while the session stays mutably borrowed elsewhere.
struct MintedSessionBagFields {
    coded_width: u32,
    coded_height: u32,
    color: Option<ColorInfo>,
}

/// The encode state machine, shared by every hardware video encoder built-in
/// and specialised only by its [`HardwareVideoCodecProcessorIdentity`].
pub struct PublishedSurfaceToEncodedFrameEncoder<Identity: HardwareVideoCodecProcessorIdentity> {
    encode_session: Option<EncodeSessionMintedFromUpstream>,
    /// Latched on the first failed mint: this machine will not grow a
    /// Vulkan Video encode queue mid-run, and retrying would re-take the
    /// escalate gate — and its device-idle drain — on every camera frame.
    session_mint_already_failed: bool,
    gpu_context: Option<GpuContextLimitedAccess>,
    ordering_pair_counter: EncodedStreamOrderingPairCounter,
    frames_encoded: u64,
    frames_that_failed_to_encode: u64,
    codec_identity: PhantomData<Identity>,
}

// The identity is a marker with no value, so the body's empty state does not
// depend on it being constructible; a derived `Default` would demand it.
impl<Identity: HardwareVideoCodecProcessorIdentity> Default
    for PublishedSurfaceToEncodedFrameEncoder<Identity>
{
    fn default() -> Self {
        Self {
            encode_session: None,
            session_mint_already_failed: false,
            gpu_context: None,
            ordering_pair_counter: EncodedStreamOrderingPairCounter::default(),
            frames_encoded: 0,
            frames_that_failed_to_encode: 0,
            codec_identity: PhantomData,
        }
    }
}

impl<Identity: HardwareVideoCodecProcessorIdentity>
    PublishedSurfaceToEncodedFrameEncoder<Identity>
{
    /// The encoder-session mint is deferred to the first frame so its
    /// dimensions track upstream; setup only keeps the context handle the
    /// mint and the per-frame resolve need.
    pub fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        Ok(())
    }

    pub fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            frames_that_failed_to_encode = self.frames_that_failed_to_encode,
            "{}: teardown",
            Identity::PROCESSOR_NAME
        );
        self.encode_session.take();
        self.gpu_context.take();
        Ok(())
    }

    /// Encode one published surface, handing back every bag it completed.
    ///
    /// A frame arriving after a failed mint is drained and discarded, so
    /// upstream still sees a live consumer and the escalate gate is not
    /// re-taken at camera cadence on a machine with no encode queue.
    pub fn encode_one_published_surface(
        &mut self,
        config: &HardwareVideoEncoderConfig,
        frame: &VideoFrame,
        staged: &mut Vec<EncodedFrameAwaitingPublication>,
    ) -> Result<()> {
        if self.encode_session.is_none() && self.session_mint_already_failed {
            return Ok(());
        }
        let gpu_context = self.gpu_context.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "{}: GPU context not initialized",
                Identity::PROCESSOR_NAME
            ))
        })?;

        if let Some(stale_session) = self
            .encode_session
            .take_if(|minted| !minted.matches_source_extent(frame.width, frame.height))
        {
            tracing::info!(
                minted_width = stale_session.minted_width,
                minted_height = stale_session.minted_height,
                frame_width = frame.width,
                frame_height = frame.height,
                "{}: upstream extent changed — re-minting the encoder session",
                Identity::PROCESSOR_NAME
            );
        }
        let (packets, session_bag_fields) = {
            let minted = match &mut self.encode_session {
                Some(minted) => minted,
                empty_session_slot => {
                    match mint_encode_session_from_first_frame::<Identity>(
                        gpu_context,
                        config,
                        frame,
                    ) {
                        Ok(minted) => empty_session_slot.insert(minted),
                        Err(mint_failure) => {
                            self.session_mint_already_failed = true;
                            tracing::error!(
                                "{}: the encoder session could not be minted; every later \
                                 frame is discarded: {mint_failure}",
                                Identity::PROCESSOR_NAME
                            );
                            return Err(mint_failure);
                        }
                    }
                }
            };
            minted.warn_once_on_color_change(Identity::PROCESSOR_NAME, frame.color_info.as_ref());

            // Resolve the frame's published surface; the registration's
            // tracked layout is what the submit checks against its sampling
            // contract.
            let source_registration = gpu_context.resolve_texture_registration_by_surface_id(
                &frame.surface_id,
                frame.texture_layout,
                frame.width,
                frame.height,
            )?;
            let packets = match minted.session.encode_source_texture(
                source_registration.texture(),
                source_registration.current_layout(),
                Some(frame.timestamp_ns),
            ) {
                Ok(packets) => packets,
                Err(encode_failure) => {
                    self.frames_that_failed_to_encode += 1;
                    return Err(Error::Runtime(format!(
                        "{}: encode failed: {encode_failure}",
                        Identity::PROCESSOR_NAME
                    )));
                }
            };
            (packets, minted.fields_every_published_bag_carries())
        };

        staged.extend(packets.into_iter().map(|packet| {
            self.encoded_frame_bag_for(&session_bag_fields, packet, frame.timestamp_ns)
        }));

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("{}: first frame encoded", Identity::PROCESSOR_NAME);
        } else if self
            .frames_encoded
            .is_multiple_of(ENCODE_PROGRESS_LOG_INTERVAL_FRAMES)
        {
            tracing::info!(
                frames_encoded = self.frames_encoded,
                "{}: encode progress",
                Identity::PROCESSOR_NAME
            );
        }
        Ok(())
    }

    /// Turn one encoded packet into an encoded-frame bag with its ordering
    /// pair accounted.
    fn encoded_frame_bag_for(
        &mut self,
        session_fields: &MintedSessionBagFields,
        packet: EncodePacket,
        source_frame_timestamp_ns: i64,
    ) -> EncodedFrameAwaitingPublication {
        let ordering_pair = self
            .ordering_pair_counter
            .account_published_bag(packet.is_keyframe);
        EncodedFrameAwaitingPublication {
            timestamp_ns: packet.timestamp_ns.unwrap_or(source_frame_timestamp_ns),
            frame: EncodedVideoFrame {
                codec: Identity::ENCODED_VIDEO_CODEC,
                annex_b_access_unit_bytes: packet.data,
                is_sync_point: packet.is_keyframe,
                group_index: ordering_pair.group_index,
                sequence_index: ordering_pair.sequence_index,
                width: session_fields.coded_width,
                height: session_fields.coded_height,
                color: session_fields.color.clone(),
            },
        }
    }
}

impl EncodeSessionMintedFromUpstream {
    fn fields_every_published_bag_carries(&self) -> MintedSessionBagFields {
        MintedSessionBagFields {
            coded_width: self.coded_width,
            coded_height: self.coded_height,
            color: self.minted_color.clone(),
        }
    }

    fn matches_source_extent(&self, frame_width: u32, frame_height: u32) -> bool {
        self.minted_width == frame_width && self.minted_height == frame_height
    }

    /// A frame whose color disagrees with the minted SPS VUI is encoded
    /// anyway — switching colorimetry needs new parameter sets the session
    /// does not re-emit — and the disagreement is said once, not per frame.
    fn warn_once_on_color_change(
        &mut self,
        processor_name: &'static str,
        frame_color: Option<&ColorInfo>,
    ) {
        if self.color_change_already_warned {
            return;
        }
        if frame_color != self.minted_color.as_ref() {
            tracing::warn!(
                minted_color = ?self.minted_color,
                frame_color = ?frame_color,
                "{}: frame color differs from the color the session's SPS VUI was minted \
                 from; the bitstream keeps the minted color",
                processor_name
            );
            self.color_change_already_warned = true;
        }
    }
}

/// Resolve the session's (width, height, fps) from the first frame, with
/// config values as guardrails: a mismatching frame wins with a warning,
/// and fps falls back frame → config → default.
fn resolve_encode_dimensions_from_first_frame(
    processor_name: &'static str,
    config: &HardwareVideoEncoderConfig,
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
            "{processor_name}: config width does not match the incoming frame; using the \
             frame's"
        );
    }
    if let Some(config_height) = config.height
        && config_height != frame_height
    {
        tracing::warn!(
            config_height,
            frame_height,
            "{processor_name}: config height does not match the incoming frame; using the \
             frame's"
        );
    }
    let fps = frame_fps.unwrap_or_else(|| config.fps.unwrap_or(DEFAULT_ENCODE_FPS));
    (frame_width, frame_height, fps)
}

/// Mint the encoder session from the first frame inside a one-shot escalate
/// window; per-frame submits ride the session's own methods afterwards.
fn mint_encode_session_from_first_frame<Identity: HardwareVideoCodecProcessorIdentity>(
    gpu_context: &GpuContextLimitedAccess,
    config: &HardwareVideoEncoderConfig,
    frame: &VideoFrame,
) -> Result<EncodeSessionMintedFromUpstream> {
    let (width, height, fps) = resolve_encode_dimensions_from_first_frame(
        Identity::PROCESSOR_NAME,
        config,
        frame.width,
        frame.height,
        frame.fps,
    );

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
        codec: Identity::VIDEO_SESSION_CODEC,
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
                "{}: failed to mint the encoder session: {e}",
                Identity::PROCESSOR_NAME
            ))
        })?;

    let (coded_width, coded_height) = session.aligned_extent();
    tracing::info!(
        width,
        height,
        fps,
        coded_width,
        coded_height,
        "{}: session minted lazily from the first frame",
        Identity::PROCESSOR_NAME
    );
    Ok(EncodeSessionMintedFromUpstream {
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
    use crate::h264_encoder::H264EncoderCodecIdentity;
    use crate::h265_encoder::H265EncoderCodecIdentity;
    use streamlib::sdk::engine::video::Codec;

    #[test]
    fn frame_dimensions_win_over_config_guardrails() {
        let config = HardwareVideoEncoderConfig {
            width: Some(1920),
            height: Some(1080),
            fps: Some(60),
            ..HardwareVideoEncoderConfig::default()
        };
        let (width, height, fps) =
            resolve_encode_dimensions_from_first_frame("H264Encoder", &config, 1280, 720, Some(30));
        assert_eq!((width, height, fps), (1280, 720, 30));
    }

    #[test]
    fn fps_falls_back_frame_then_config_then_default() {
        let config_with_fps = HardwareVideoEncoderConfig {
            fps: Some(24),
            ..HardwareVideoEncoderConfig::default()
        };
        let (_, _, fps_from_config) = resolve_encode_dimensions_from_first_frame(
            "H265Encoder",
            &config_with_fps,
            1280,
            720,
            None,
        );
        assert_eq!(fps_from_config, 24);

        let (_, _, fps_default) = resolve_encode_dimensions_from_first_frame(
            "H265Encoder",
            &HardwareVideoEncoderConfig::default(),
            1280,
            720,
            None,
        );
        assert_eq!(fps_default, DEFAULT_ENCODE_FPS);
    }

    #[test]
    fn an_empty_config_still_uses_the_frame_dimensions() {
        let (width, height, _) = resolve_encode_dimensions_from_first_frame(
            "H265Encoder",
            &HardwareVideoEncoderConfig::default(),
            3840,
            2160,
            Some(30),
        );
        assert_eq!((width, height), (3840, 2160));
    }

    /// The config map is open and fully optional — `rt.add(H265Encoder)`
    /// with no config at all must deserialize.
    #[test]
    fn an_all_absent_config_deserializes_to_defaults() {
        let config: HardwareVideoEncoderConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config, HardwareVideoEncoderConfig::default());
    }

    /// The whole point of the shared body: each built-in's identity is what
    /// picks the session codec and the bag's `codec`, and mixing the two up
    /// would produce a bitstream whose bag lies about it.
    #[test]
    fn each_encoders_identity_names_one_codec_on_both_the_session_and_the_bag() {
        assert_eq!(H264EncoderCodecIdentity::VIDEO_SESSION_CODEC, Codec::H264);
        assert_eq!(
            H264EncoderCodecIdentity::ENCODED_VIDEO_CODEC.as_wire_str(),
            "h264"
        );
        assert_eq!(H265EncoderCodecIdentity::VIDEO_SESSION_CODEC, Codec::H265);
        assert_eq!(
            H265EncoderCodecIdentity::ENCODED_VIDEO_CODEC.as_wire_str(),
            "h265"
        );
    }
}
