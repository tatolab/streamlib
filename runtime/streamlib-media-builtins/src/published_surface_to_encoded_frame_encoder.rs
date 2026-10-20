// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The encode body both hardware video encoder built-ins are: published
//! video surfaces in, media frames out — hang's legacy container around one
//! Annex-B access unit, a sync point opening a group — via the video codec
//! seam.
//!
//! The session mints lazily inside a one-shot `escalate` window on the first
//! frame, so its dimensions track upstream; config width/height are
//! guardrails (mismatch warns, the frame wins). Per-frame submits ride the
//! session's own methods and never re-escalate. An upstream that changes
//! extent mid-run gets a fresh session at the new extent — new parameter
//! sets, opening at a sync point — rather than a refusal.
//!
//! Ports live on the processors that own this body, which hand it the write.
//! The port's catalog entry is derived from the first sync point a session
//! emits: the codec string its SPS spells and the session's coded extent.
//!
//! A write the channel ceiling refuses loses an access unit the next ones
//! reference, so the access units after it are dropped up to the next sync
//! point and the next frame is encoded as one.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    EncodedVideoAccessUnitFromSession, GpuContextLimitedAccess, RuntimeContextFullAccess,
    VideoEncodeKnobs, VideoEncodeSession, VideoEncodeSessionRequest, VideoEncodeSourceSurface,
    probe_video_codec_backend,
};
use streamlib::sdk::descriptors::VideoEntry;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::hang_legacy_container_media_frame::write_hang_legacy_container_media_frame;
use streamlib::sdk::iceoryx2::{MessageGroupBoundary, OutputWriter};
use streamlib::sdk::schemars::JsonSchema;
use streamlib::sdk::video_bitstream::parameter_sets_of_annex_b_access_unit;

use crate::cumulative_count_report_threshold::CumulativeCountReportThreshold;
use crate::h273_color_vui_translation::color_info_to_h273_color_vui;
use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;
use crate::video_frame::{ColorInfo, VideoFrame};

/// Fallback frame rate when neither the frame nor the config states one.
const DEFAULT_ENCODE_FPS: u32 = 60;

/// Seconds between IDR sync points when the config states none.
const DEFAULT_IDR_INTERVAL_SECONDS: u32 = 2;

/// Encode-progress log cadence, in frames.
const ENCODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// Refused writes between reports; the port's refused-write count in `graph`
/// carries every one.
const REFUSED_WRITE_REPORT_INTERVAL: u64 = 20;

/// Configuration for the hardware video encoder built-ins. Everything is
/// optional: dimensions and rate track the upstream frames, and the knobs
/// below are the guardrail set the codec seam accepts. Both codecs take
/// exactly these, because the seam takes exactly these.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct HardwareVideoEncoderConfig {
    /// Expected frame width — a guardrail, not a resize: a mismatching
    /// frame wins with a warning.
    #[serde(default)]
    pub width: Option<u32>,
    /// Expected frame height — a guardrail like `width`.
    #[serde(default)]
    pub height: Option<u32>,
    /// Frame rate used when the incoming frames carry none. Absent: 60.
    #[serde(default)]
    pub fps: Option<u32>,
    /// Target bitrate in bits per second. Absent: constant-QP encoding at
    /// the session's balanced preset.
    #[serde(default)]
    pub bitrate_bps: Option<u32>,
    /// Seconds between IDR sync points. Absent: 2. On macOS, 0 is refused by
    /// name at `setup()`.
    #[serde(default)]
    pub keyframe_interval_seconds: Option<u32>,
    /// Encoder-effort index (the hardware encoder's analysis budget, not a
    /// codec quality knob). Absent: the codec's default. VideoToolbox has no
    /// effort index, so on macOS any value is refused by name at `setup()`.
    #[serde(default)]
    pub effort_level: Option<u32>,
}

/// One access unit the caller still has to publish, and the timestamp it
/// rides under — the packet's own when it carries one, the source frame's
/// otherwise.
pub struct EncodedAccessUnitAwaitingPublication {
    pub annex_b_access_unit_bytes: Vec<u8>,
    /// A sync point opens a group; every other access unit continues one.
    pub group_boundary: MessageGroupBoundary,
    pub timestamp_ns: i64,
}

/// One minted encoder session plus the upstream facts it was minted from —
/// what a later frame is checked against to notice a renegotiated upstream.
struct EncodeSessionMintedFromUpstream {
    session: Box<dyn VideoEncodeSession>,
    /// The source extent the session was minted for.
    minted_width: u32,
    minted_height: u32,
    /// The codec-aligned coded extent, the catalog entry's `codedWidth` and
    /// `codedHeight`.
    coded_width: u32,
    coded_height: u32,
    /// The color the session's SPS VUI was minted from.
    minted_color: Option<ColorInfo>,
    /// A mid-session color change is warned once, not per frame.
    color_change_already_warned: bool,
    /// Whether this session's first sync point has been read for the catalog
    /// entry.
    video_entry_derived: bool,
}

/// The encode state machine, shared by every hardware video encoder built-in
/// and specialised only by its [`HardwareVideoCodecProcessorIdentity`].
pub struct PublishedSurfaceToEncodedFrameEncoder<Identity: HardwareVideoCodecProcessorIdentity> {
    encode_session: Option<EncodeSessionMintedFromUpstream>,
    /// Latched on the first failed mint: this machine will not grow a
    /// hardware encoder mid-run, and retrying would re-take the
    /// escalate gate — and its device-idle drain — on every camera frame.
    session_mint_already_failed: bool,
    gpu_context: Option<GpuContextLimitedAccess>,
    /// The entry the port carries.
    video_entry_on_the_port: Option<VideoEntry>,
    /// An entry a mint derived that the port does not carry yet.
    video_entry_owed_to_the_port: Option<VideoEntry>,
    /// One media frame's bytes, reused across every write.
    media_frame_scratch: Vec<u8>,
    /// Set by a refused write, spent by the next encode.
    next_frame_must_be_a_sync_point: bool,
    /// Set by a refused write, cleared by the next sync point written.
    dropping_access_units_until_the_next_sync_point: bool,
    access_units_dropped_after_a_refused_write: u64,
    refused_write_report_schedule: CumulativeCountReportThreshold,
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
            video_entry_on_the_port: None,
            video_entry_owed_to_the_port: None,
            media_frame_scratch: Vec::new(),
            next_frame_must_be_a_sync_point: false,
            dropping_access_units_until_the_next_sync_point: false,
            access_units_dropped_after_a_refused_write: 0,
            refused_write_report_schedule: CumulativeCountReportThreshold::reporting_every(
                REFUSED_WRITE_REPORT_INTERVAL,
            ),
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
    /// dimensions track upstream; setup keeps the context handle the mint and
    /// the per-frame resolve need, and refuses a knob the platform's codec
    /// arm would not honour before any frame arrives.
    pub fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
        config: &HardwareVideoEncoderConfig,
    ) -> Result<()> {
        probe_video_codec_backend().refuse_encode_knobs_this_arm_does_not_honour(
            Identity::VIDEO_CODEC_ELEMENTARY_STREAM,
            &encode_knobs_from(config),
        )?;
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        Ok(())
    }

    pub fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            frames_that_failed_to_encode = self.frames_that_failed_to_encode,
            access_units_dropped_after_a_refused_write =
                self.access_units_dropped_after_a_refused_write,
            "{}: teardown",
            Identity::PROCESSOR_NAME
        );
        self.encode_session.take();
        self.gpu_context.take();
        Ok(())
    }

    /// Encode one published surface, handing back every access unit it
    /// completed.
    ///
    /// The frame is claimed against its producer's reuse from the read
    /// through the encode, so a session mint outlasting the producer's
    /// rotation never loses it. A frame arriving after a failed mint is
    /// drained and discarded, so upstream still sees a live consumer and the
    /// escalate gate is not re-taken at camera cadence on a machine with no
    /// encode queue.
    pub fn encode_one_published_surface(
        &mut self,
        config: &HardwareVideoEncoderConfig,
        frame: &VideoFrame,
        staged: &mut Vec<EncodedAccessUnitAwaitingPublication>,
    ) -> Result<()> {
        if self.encode_session.is_none() && self.session_mint_already_failed {
            return Ok(());
        }
        let claimed = self
            .gpu_context
            .as_ref()
            .ok_or_else(gpu_context_not_initialized::<Identity>)?
            .claim_surface_against_producer_reuse(&frame.surface_id);
        let _frame_claimed_against_producer_reuse =
            claimed.map_err(|refusal| self.count_an_encode_failure(refusal))?;
        self.encode_one_claimed_surface(config, frame, staged)
    }

    fn count_an_encode_failure(&mut self, cause: impl std::fmt::Display) -> Error {
        self.frames_that_failed_to_encode += 1;
        encode_failure_naming_the_encoder::<Identity>(cause)
    }

    fn encode_one_claimed_surface(
        &mut self,
        config: &HardwareVideoEncoderConfig,
        frame: &VideoFrame,
        staged: &mut Vec<EncodedAccessUnitAwaitingPublication>,
    ) -> Result<()> {
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
        let packets = {
            let minted = match &mut self.encode_session {
                Some(minted) => minted,
                empty_session_slot => {
                    let gpu_context = self
                        .gpu_context
                        .as_ref()
                        .ok_or_else(gpu_context_not_initialized::<Identity>)?;
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

            let packets = match minted
                .session
                .encode_published_surface(&VideoEncodeSourceSurface {
                    surface_id: &frame.surface_id,
                    texture_layout: frame.texture_layout,
                    width: frame.width,
                    height: frame.height,
                    timestamp_ns: frame.timestamp_ns,
                    force_sync_point: self.next_frame_must_be_a_sync_point,
                }) {
                Ok(packets) => packets,
                Err(encode_failure) => {
                    self.frames_that_failed_to_encode += 1;
                    return Err(encode_failure_naming_the_encoder::<Identity>(
                        encode_failure,
                    ));
                }
            };
            self.next_frame_must_be_a_sync_point = false;
            if let Some(video_entry) =
                minted.video_entry_from_its_first_sync_point::<Identity>(&packets)
                && self.video_entry_on_the_port.as_ref() != Some(&video_entry)
            {
                self.video_entry_owed_to_the_port = Some(video_entry);
            }
            packets
        };

        staged.extend(
            packets
                .into_iter()
                .map(|packet| access_unit_awaiting_publication(packet, frame.timestamp_ns)),
        );

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

    /// Publish staged access units on `output_port`, setting the catalog
    /// entry a mint owes the port first so it lands ahead of the first media
    /// frame it describes. A refused entry drops the staged access units, so
    /// the next frame is encoded as a sync point.
    pub fn publish_to_output_port(
        &mut self,
        outputs: &OutputWriter,
        output_port: &str,
        staged: Vec<EncodedAccessUnitAwaitingPublication>,
    ) -> Result<()> {
        if let Some(video_entry) = &self.video_entry_owed_to_the_port {
            if let Err(entry_refusal) = outputs.set_catalog_entry(output_port, video_entry.clone())
            {
                self.next_frame_must_be_a_sync_point = true;
                self.dropping_access_units_until_the_next_sync_point = true;
                return Err(entry_refusal);
            }
            self.video_entry_on_the_port = self.video_entry_owed_to_the_port.take();
        }
        self.publish_staged_access_units(
            staged,
            |media_frame_bytes, timestamp_ns, group_boundary| {
                outputs.write_bytes_with_timestamp(
                    output_port,
                    media_frame_bytes,
                    timestamp_ns,
                    group_boundary,
                )
            },
        )
    }

    /// Publish staged access units as media frames through `write_media_frame`,
    /// a sync point opening a group. A write the channel ceiling refuses
    /// drops what follows up to the next sync point and makes the next frame
    /// one; any other failure is returned.
    fn publish_staged_access_units(
        &mut self,
        staged: Vec<EncodedAccessUnitAwaitingPublication>,
        mut write_media_frame: impl FnMut(&[u8], i64, MessageGroupBoundary) -> Result<()>,
    ) -> Result<()> {
        for access_unit in staged {
            if self.dropping_access_units_until_the_next_sync_point
                && access_unit.group_boundary == MessageGroupBoundary::ContinuesGroup
            {
                self.access_units_dropped_after_a_refused_write += 1;
                continue;
            }
            write_hang_legacy_container_media_frame(
                &mut self.media_frame_scratch,
                access_unit.timestamp_ns,
                &access_unit.annex_b_access_unit_bytes,
            )
            .map_err(|refusal| {
                Error::Runtime(format!("{}: {refusal}", Identity::PROCESSOR_NAME))
            })?;
            match write_media_frame(
                &self.media_frame_scratch,
                access_unit.timestamp_ns,
                access_unit.group_boundary,
            ) {
                Ok(()) => self.dropping_access_units_until_the_next_sync_point = false,
                Err(Error::PayloadExceedsChannelCeiling {
                    channel,
                    payload_bytes,
                    largest_admitted_frame_bytes,
                    refused_bags_on_the_output_port,
                    ..
                }) => {
                    self.next_frame_must_be_a_sync_point = true;
                    self.dropping_access_units_until_the_next_sync_point = true;
                    if self
                        .refused_write_report_schedule
                        .count_is_worth_reporting(refused_bags_on_the_output_port)
                    {
                        tracing::warn!(
                            channel,
                            payload_bytes,
                            largest_admitted_frame_bytes,
                            refused_bags_on_the_output_port,
                            "{}: the channel ceiling refused an access unit — dropping to the \
                             next sync point and encoding the next frame as one",
                            Identity::PROCESSOR_NAME
                        );
                    }
                }
                // The rest of the batch is lost with this one, and what the
                // session encodes next references it.
                Err(write_failure) => {
                    self.next_frame_must_be_a_sync_point = true;
                    self.dropping_access_units_until_the_next_sync_point = true;
                    return Err(write_failure);
                }
            }
        }
        Ok(())
    }
}

fn gpu_context_not_initialized<Identity: HardwareVideoCodecProcessorIdentity>() -> Error {
    Error::Runtime(format!(
        "{}: GPU context not initialized",
        Identity::PROCESSOR_NAME
    ))
}

fn encode_failure_naming_the_encoder<Identity: HardwareVideoCodecProcessorIdentity>(
    cause: impl std::fmt::Display,
) -> Error {
    Error::Runtime(format!(
        "{}: encode failed: {cause}",
        Identity::PROCESSOR_NAME
    ))
}

/// One access unit, stamped with its packet's own timestamp when the arm
/// carried one through and the source frame's otherwise.
fn access_unit_awaiting_publication(
    packet: EncodedVideoAccessUnitFromSession,
    source_frame_timestamp_ns: i64,
) -> EncodedAccessUnitAwaitingPublication {
    EncodedAccessUnitAwaitingPublication {
        timestamp_ns: packet.timestamp_ns.unwrap_or(source_frame_timestamp_ns),
        annex_b_access_unit_bytes: packet.annex_b_access_unit_bytes,
        group_boundary: if packet.is_sync_point {
            MessageGroupBoundary::StartsGroup
        } else {
            MessageGroupBoundary::ContinuesGroup
        },
    }
}

impl EncodeSessionMintedFromUpstream {
    /// The catalog entry this session's first sync point describes, once:
    /// the codec string its SPS spells and the session's coded extent, no
    /// `description` because the parameter sets ride inline.
    fn video_entry_from_its_first_sync_point<Identity: HardwareVideoCodecProcessorIdentity>(
        &mut self,
        packets: &[EncodedVideoAccessUnitFromSession],
    ) -> Option<VideoEntry> {
        if self.video_entry_derived {
            return None;
        }
        let first_sync_point = packets.iter().find(|packet| packet.is_sync_point)?;
        self.video_entry_derived = true;
        let Some(codec) = parameter_sets_of_annex_b_access_unit(
            &first_sync_point.annex_b_access_unit_bytes,
            Identity::VIDEO_CODEC_ELEMENTARY_STREAM,
        )
        .codec_string_of_the_sequence_parameter_set(Identity::VIDEO_CODEC_ELEMENTARY_STREAM) else {
            tracing::warn!(
                "{}: the session's first sync point carries no readable SPS, so the port \
                 publishes no catalog entry for this session",
                Identity::PROCESSOR_NAME
            );
            return None;
        };
        Some(VideoEntry {
            codec,
            coded_width: Some(self.coded_width),
            coded_height: Some(self.coded_height),
            description: None,
        })
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

/// The knobs `config` sets, with the keyframe interval's default filled in.
fn encode_knobs_from(config: &HardwareVideoEncoderConfig) -> VideoEncodeKnobs {
    VideoEncodeKnobs {
        bitrate_bps: config.bitrate_bps,
        keyframe_interval_seconds: config
            .keyframe_interval_seconds
            .unwrap_or(DEFAULT_IDR_INTERVAL_SECONDS),
        effort_level: config.effort_level,
    }
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

    let session_request = VideoEncodeSessionRequest {
        elementary_stream: Identity::VIDEO_CODEC_ELEMENTARY_STREAM,
        width,
        height,
        frames_per_second: fps,
        knobs: encode_knobs_from(config),
        color_vui,
    };

    // One-shot mint: the escalate scope-end drains the device, so this runs
    // once per session, never per frame.
    let backend = probe_video_codec_backend();
    let session = gpu_context
        .escalate(|full| backend.open_encode_session(full, &session_request))
        .map_err(|e| {
            Error::Runtime(format!(
                "{}: failed to mint the encoder session: {e}",
                Identity::PROCESSOR_NAME
            ))
        })?;

    let (coded_width, coded_height) = session.coded_extent();
    tracing::info!(
        video_codec_backend = backend.backend_name(),
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
        video_entry_derived: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h264_encoder::H264EncoderCodecIdentity;
    use crate::h265_encoder::H265EncoderCodecIdentity;
    use streamlib::sdk::context::VideoCodecElementaryStream;

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

    /// The config map is open and fully optional — `stream_builder.add(H265Encoder)`
    /// with no config at all must deserialize.
    #[test]
    fn an_all_absent_config_deserializes_to_defaults() {
        let config: HardwareVideoEncoderConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config, HardwareVideoEncoderConfig::default());
    }

    #[test]
    fn each_encoders_identity_opens_a_session_for_its_own_codec() {
        assert_eq!(
            H264EncoderCodecIdentity::VIDEO_CODEC_ELEMENTARY_STREAM,
            VideoCodecElementaryStream::H264
        );
        assert_eq!(
            H265EncoderCodecIdentity::VIDEO_CODEC_ELEMENTARY_STREAM,
            VideoCodecElementaryStream::H265
        );
    }

    fn access_unit(
        group_boundary: MessageGroupBoundary,
        timestamp_ns: i64,
    ) -> EncodedAccessUnitAwaitingPublication {
        EncodedAccessUnitAwaitingPublication {
            annex_b_access_unit_bytes: vec![0x00, 0x00, 0x00, 0x01, 0x65],
            group_boundary,
            timestamp_ns,
        }
    }

    fn channel_ceiling_refusal() -> Error {
        Error::PayloadExceedsChannelCeiling {
            channel: "encoder.encoded_video".to_owned(),
            payload_bytes: 9_000_000,
            largest_admitted_frame_bytes: 8_000_000,
            chunk_ceiling_bytes: 8_388_608,
            tier: streamlib::sdk::error::ChannelTrustTierLabel::Trusted,
            refused_bags_on_the_output_port: 1,
        }
    }

    #[test]
    fn each_access_unit_is_written_as_a_media_frame_a_sync_point_opening_a_group() {
        let mut encode_body =
            PublishedSurfaceToEncodedFrameEncoder::<H264EncoderCodecIdentity>::default();
        let mut written = Vec::new();
        encode_body
            .publish_staged_access_units(
                vec![
                    access_unit(MessageGroupBoundary::StartsGroup, 1_000_000),
                    access_unit(MessageGroupBoundary::ContinuesGroup, 34_000_000),
                ],
                |media_frame_bytes, timestamp_ns, group_boundary| {
                    written.push((media_frame_bytes.to_vec(), timestamp_ns, group_boundary));
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(
            written,
            vec![
                (
                    vec![0x43, 0xE8, 0x00, 0x00, 0x00, 0x01, 0x65],
                    1_000_000,
                    MessageGroupBoundary::StartsGroup
                ),
                (
                    vec![0x80, 0x00, 0x84, 0xD0, 0x00, 0x00, 0x00, 0x01, 0x65],
                    34_000_000,
                    MessageGroupBoundary::ContinuesGroup
                ),
            ]
        );
    }

    /// The access units after a refused one reference it, so they are
    /// dropped rather than handed to a decoder, and the next frame asks the
    /// session for a sync point.
    #[test]
    fn a_refused_write_drops_to_the_next_sync_point_and_forces_one() {
        let mut encode_body =
            PublishedSurfaceToEncodedFrameEncoder::<H265EncoderCodecIdentity>::default();
        let mut written_stamps = Vec::new();
        encode_body
            .publish_staged_access_units(
                vec![
                    access_unit(MessageGroupBoundary::ContinuesGroup, 1),
                    access_unit(MessageGroupBoundary::ContinuesGroup, 2_000),
                    access_unit(MessageGroupBoundary::ContinuesGroup, 3_000),
                ],
                |_, timestamp_ns, _| {
                    if timestamp_ns == 1 {
                        return Err(channel_ceiling_refusal());
                    }
                    written_stamps.push(timestamp_ns);
                    Ok(())
                },
            )
            .expect("a ceiling refusal is survived");
        assert!(written_stamps.is_empty());
        assert!(encode_body.next_frame_must_be_a_sync_point);
        assert_eq!(encode_body.access_units_dropped_after_a_refused_write, 2);

        encode_body
            .publish_staged_access_units(
                vec![
                    access_unit(MessageGroupBoundary::StartsGroup, 4_000),
                    access_unit(MessageGroupBoundary::ContinuesGroup, 5_000),
                ],
                |_, timestamp_ns, _| {
                    written_stamps.push(timestamp_ns);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(written_stamps, vec![4_000, 5_000]);
    }

    #[test]
    fn a_write_failing_for_any_other_reason_is_returned_and_forces_the_next_sync_point() {
        let mut encode_body =
            PublishedSurfaceToEncodedFrameEncoder::<H264EncoderCodecIdentity>::default();
        let failure = encode_body
            .publish_staged_access_units(
                vec![access_unit(MessageGroupBoundary::StartsGroup, 1_000)],
                |_, _, _| Err(Error::Link("the port is not wired".to_owned())),
            )
            .expect_err("a link failure is not a ceiling refusal");
        assert!(matches!(failure, Error::Link(_)));
        assert!(
            encode_body.next_frame_must_be_a_sync_point,
            "the access units after a failed write reference it, so the next frame is a sync point"
        );
    }

    /// A session that records what each encode asked of it and hands back
    /// one access unit, a sync point exactly when one was asked for.
    struct SessionRecordingForcedSyncPoints {
        force_sync_point_by_encode: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
    }

    impl VideoEncodeSession for SessionRecordingForcedSyncPoints {
        fn coded_extent(&self) -> (u32, u32) {
            (1280, 720)
        }

        fn encode_published_surface(
            &mut self,
            source: &VideoEncodeSourceSurface<'_>,
        ) -> Result<Vec<EncodedVideoAccessUnitFromSession>> {
            self.force_sync_point_by_encode
                .lock()
                .unwrap()
                .push(source.force_sync_point);
            Ok(vec![EncodedVideoAccessUnitFromSession {
                annex_b_access_unit_bytes: vec![0x00, 0x00, 0x00, 0x01, 0x41],
                is_sync_point: source.force_sync_point,
                timestamp_ns: Some(source.timestamp_ns),
            }])
        }
    }

    fn a_720p_frame(timestamp_ns: i64) -> VideoFrame {
        VideoFrame {
            surface_id: "surface".to_owned(),
            width: 1280,
            height: 720,
            timestamp_ns,
            color_info: None,
            fps: None,
            content_light: None,
            mastering_display: None,
            texture_layout: None,
        }
    }

    /// The refusal reaches the session: the frame after a refused write is
    /// encoded with a forced sync point, and only that frame.
    #[test]
    fn the_frame_after_a_refused_write_asks_the_session_for_a_sync_point() {
        let force_sync_point_by_encode = std::sync::Arc::default();
        let mut encode_body = PublishedSurfaceToEncodedFrameEncoder::<H264EncoderCodecIdentity> {
            encode_session: Some(EncodeSessionMintedFromUpstream {
                session: Box::new(SessionRecordingForcedSyncPoints {
                    force_sync_point_by_encode: std::sync::Arc::clone(&force_sync_point_by_encode),
                }),
                minted_width: 1280,
                minted_height: 720,
                coded_width: 1280,
                coded_height: 720,
                minted_color: None,
                color_change_already_warned: false,
                video_entry_derived: true,
            }),
            ..Default::default()
        };
        let config = HardwareVideoEncoderConfig::default();

        let mut refused_frame_access_units = Vec::new();
        encode_body
            .encode_one_claimed_surface(
                &config,
                &a_720p_frame(1_000),
                &mut refused_frame_access_units,
            )
            .unwrap();
        encode_body
            .publish_staged_access_units(refused_frame_access_units, |_, _, _| {
                Err(channel_ceiling_refusal())
            })
            .unwrap();
        for timestamp_ns in [2_000, 3_000] {
            let mut staged = Vec::new();
            encode_body
                .encode_one_claimed_surface(&config, &a_720p_frame(timestamp_ns), &mut staged)
                .unwrap();
            encode_body
                .publish_staged_access_units(staged, |_, _, _| Ok(()))
                .unwrap();
        }

        assert_eq!(
            *force_sync_point_by_encode.lock().unwrap(),
            vec![false, true, false]
        );
    }
}

#[cfg(test)]
mod frame_claimed_from_the_read_through_the_encode_tests {
    use std::sync::{Arc, Mutex};

    use streamlib::sdk::context::GpuContext;
    use streamlib::sdk::rhi::{PixelFormat, pool_slot_key_of_surface_id};

    use super::*;
    use crate::h264_encoder::H264EncoderCodecIdentity;

    const FRAME_EXTENT: u32 = 64;
    const FRAMES_THE_PRODUCER_PUBLISHES_DURING_ONE_ENCODE: usize = 4;
    const PUBLISHES_A_SLOT_COMES_ROUND_WITHIN: usize = 16;

    /// A session standing in for an encode the producer outpaces: while it
    /// encodes, the producer publishes more frames of the source's shape, and
    /// the session records the slot each lands in.
    struct SessionTheProducerOutpaces {
        gpu_context: GpuContextLimitedAccess,
        slot_of_each_frame_published_during_the_encode: Arc<Mutex<Vec<String>>>,
    }

    impl VideoEncodeSession for SessionTheProducerOutpaces {
        fn coded_extent(&self) -> (u32, u32) {
            (FRAME_EXTENT, FRAME_EXTENT)
        }

        fn encode_published_surface(
            &mut self,
            _source: &VideoEncodeSourceSurface<'_>,
        ) -> Result<Vec<EncodedVideoAccessUnitFromSession>> {
            for _ in 0..FRAMES_THE_PRODUCER_PUBLISHES_DURING_ONE_ENCODE {
                let (published, _released_at_once) = self.gpu_context.acquire_pixel_buffer(
                    FRAME_EXTENT,
                    FRAME_EXTENT,
                    PixelFormat::Rgba32,
                )?;
                self.slot_of_each_frame_published_during_the_encode
                    .lock()
                    .unwrap()
                    .push(pool_slot_key_of_surface_id(&published.to_string()).to_owned());
            }
            Ok(Vec::new())
        }
    }

    fn a_frame_nothing_but_the_encoder_holds(gpu_context: &GpuContextLimitedAccess) -> VideoFrame {
        let (published, released_at_once) = gpu_context
            .acquire_pixel_buffer(FRAME_EXTENT, FRAME_EXTENT, PixelFormat::Rgba32)
            .expect("a pixel buffer");
        drop(released_at_once);
        VideoFrame {
            surface_id: published.to_string(),
            width: FRAME_EXTENT,
            height: FRAME_EXTENT,
            timestamp_ns: 1_000,
            color_info: None,
            fps: None,
            content_light: None,
            mastering_display: None,
            texture_layout: None,
        }
    }

    fn encode_body_over(
        gpu_context: &GpuContextLimitedAccess,
        slot_of_each_frame_published_during_the_encode: &Arc<Mutex<Vec<String>>>,
    ) -> PublishedSurfaceToEncodedFrameEncoder<H264EncoderCodecIdentity> {
        PublishedSurfaceToEncodedFrameEncoder {
            encode_session: Some(EncodeSessionMintedFromUpstream {
                session: Box::new(SessionTheProducerOutpaces {
                    gpu_context: gpu_context.clone(),
                    slot_of_each_frame_published_during_the_encode: Arc::clone(
                        slot_of_each_frame_published_during_the_encode,
                    ),
                }),
                minted_width: FRAME_EXTENT,
                minted_height: FRAME_EXTENT,
                coded_width: FRAME_EXTENT,
                coded_height: FRAME_EXTENT,
                minted_color: None,
                color_change_already_warned: false,
                video_entry_derived: true,
            }),
            gpu_context: Some(gpu_context.clone()),
            ..Default::default()
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "needs a Vulkan device (lavapipe will do). Run with --features streamlib-media-builtins/hardware-tests. See docs/testing-hardware.md"
    )]
    fn the_producer_never_republishes_the_slot_of_the_frame_being_encoded() {
        let gpu_context = GpuContext::init_for_platform_sync()
            .expect("a GPU device")
            .limited_access();
        let slot_of_each_frame_published_during_the_encode = Arc::default();
        let mut encode_body = encode_body_over(
            &gpu_context,
            &slot_of_each_frame_published_during_the_encode,
        );
        let frame = a_frame_nothing_but_the_encoder_holds(&gpu_context);

        encode_body
            .encode_one_published_surface(
                &HardwareVideoEncoderConfig::default(),
                &frame,
                &mut Vec::new(),
            )
            .expect("the frame encodes");

        let slot_being_encoded = pool_slot_key_of_surface_id(&frame.surface_id);
        let published_during_the_encode = slot_of_each_frame_published_during_the_encode
            .lock()
            .unwrap();
        assert_eq!(
            published_during_the_encode.len(),
            FRAMES_THE_PRODUCER_PUBLISHES_DURING_ONE_ENCODE
        );
        assert!(
            published_during_the_encode
                .iter()
                .all(|published_slot| published_slot != slot_being_encoded),
            "the producer republished {slot_being_encoded} while it was encoded: \
             {published_during_the_encode:?}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "needs a Vulkan device (lavapipe will do). Run with --features streamlib-media-builtins/hardware-tests. See docs/testing-hardware.md"
    )]
    fn a_frame_recycled_before_its_read_is_refused_naming_the_recycling_and_counted() {
        let gpu_context = GpuContext::init_for_platform_sync()
            .expect("a GPU device")
            .limited_access();
        let slot_of_each_frame_published_during_the_encode = Arc::default();
        let mut encode_body = encode_body_over(
            &gpu_context,
            &slot_of_each_frame_published_during_the_encode,
        );
        let recycled_frame = a_frame_nothing_but_the_encoder_holds(&gpu_context);
        let recycled_slot = pool_slot_key_of_surface_id(&recycled_frame.surface_id);
        let the_slot_came_round = (0..PUBLISHES_A_SLOT_COMES_ROUND_WITHIN).any(|_| {
            pool_slot_key_of_surface_id(
                &a_frame_nothing_but_the_encoder_holds(&gpu_context).surface_id,
            ) == recycled_slot
        });
        assert!(
            the_slot_came_round,
            "the pool never republished {recycled_slot}"
        );

        let refusal = encode_body
            .encode_one_published_surface(
                &HardwareVideoEncoderConfig::default(),
                &recycled_frame,
                &mut Vec::new(),
            )
            .expect_err("a recycled frame is not encoded");

        assert!(
            refusal.to_string().contains("names a recycled frame"),
            "got: {refusal}"
        );
        assert_eq!(encode_body.frames_that_failed_to_encode, 1);
        assert!(
            slot_of_each_frame_published_during_the_encode
                .lock()
                .unwrap()
                .is_empty(),
            "the session was asked to encode a recycled frame"
        );
    }
}
