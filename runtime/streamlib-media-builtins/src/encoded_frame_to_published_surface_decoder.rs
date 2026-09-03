// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! The decode body both hardware video decoder built-ins are: encoded-frame
//! bags in, published video surfaces out, via the engine's Vulkan Video
//! session surface.
//!
//! The session mints in `setup()`, whose typestate is already Full, with its
//! coded dimensions auto-detected from the stream's first SPS. Decoded
//! pictures are read back as RGBA — already cropped to the stream's
//! conformance window by the session, so the CTU padding an H.265 stream
//! codes never reaches a consumer — and staged into pooled pixel buffers
//! whose pool id is the published `surface_id`, the same CPU→GPU hand-off
//! the camera uses.
//!
//! One session serves one coded extent. A producer that renegotiates — the
//! shipped encoders re-mint at a new extent and open the new stream at a
//! sync point without breaking `sequence_index` — is noticed by the extent on
//! the bag, which is why the convention carries it: the session resets and
//! the gate re-enters, rather than feeding new parameter sets into a session
//! configured for the old ones.
//!
//! A consumer of an encoded stream must bound loss, so this one enters the
//! stream at a sync point and discards back to one after a `sequence_index`
//! gap: it never hands a decoder slices whose reference frames it did not
//! see, and never publishes a picture built from them.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::engine::video::H273ColorVui;
use streamlib::sdk::engine::video::decode::{
    SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};

use crate::cumulative_count_report_threshold::CumulativeCountReportThreshold;
use crate::encoded_stream_ordering::{ArrivingEncodedBagDisposition, EncodedStreamSyncPointGate};
use crate::encoded_video_frame::{
    EncodedVideoCodec, EncodedVideoFrame, read_encoded_video_frame_bag,
};
use crate::h273_color_vui_translation::h273_color_vui_to_color_info;
use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;
use crate::pooled_rgba_frame_staging::stage_tightly_packed_rgba_into_pooled_pixel_buffer;
use crate::video_frame::{ColorInfo, VideoFrame};

/// Decode-progress log cadence, in frames.
const DECODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// Stream re-entries between reports. A healthy run enters once and says so;
/// a persistently lossy link re-enters at the producer's GOP cadence, and
/// saying that every time buries the run in the symptom.
const STREAM_RE_ENTRY_REPORT_INTERVAL: u64 = 20;

/// Configuration for the hardware video decoder built-ins. Both knobs cap the
/// extent the decoded picture buffer is allocated for, and both are optional:
/// absent, the extent is auto-detected from the stream's first SPS, which is
/// what a decoder fed by an unknown producer wants. The DPB's slot count is
/// the session surface's own and is not configurable here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HardwareVideoDecoderConfig {
    /// Upper bound on the coded width the DPB is allocated for. Absent:
    /// auto-detected from the first SPS.
    #[serde(default)]
    pub max_width: Option<u32>,
    /// Upper bound on the coded height, paired with [`Self::max_width`].
    #[serde(default)]
    pub max_height: Option<u32>,
}

/// One decoded frame the caller still has to write. The pooled pixel buffer
/// rides along so the pool cannot rotate the slot out between staging and the
/// write; it is released when the caller drops this.
pub struct DecodedFrameAwaitingPublication {
    pub frame: VideoFrame,
    _pooled_pixel_buffer_held_until_written: PixelBuffer,
}

/// The decode state machine, shared by every hardware video decoder built-in
/// and specialised only by its [`HardwareVideoCodecProcessorIdentity`].
pub struct EncodedFrameToPublishedSurfaceDecoder<Identity: HardwareVideoCodecProcessorIdentity> {
    decode_session: Option<SimpleDecoder>,
    gpu_context: Option<GpuContextLimitedAccess>,
    sync_point_gate: EncodedStreamSyncPointGate,
    stream_re_entry_report_schedule: Option<CumulativeCountReportThreshold>,
    /// The coded extent the minted session's parameter sets describe, learned
    /// from the first bag admitted and compared against every later one.
    session_coded_extent: Option<(u32, u32)>,
    /// Latched by an extent renegotiation, spent at the next re-entry: only
    /// then is the session's full reset owed. A plain gap re-enters with a
    /// parser flush alone — the full reset rebuilds session parameters, and
    /// that rebuild waits the whole device idle, which stalls every producer
    /// once per gap exactly when the decoder most needs to catch up.
    session_needs_full_reset: bool,
    frames_decoded: u64,
    /// The color the published frames carry, resolved once the stream's SPS
    /// has been parsed, and said once rather than per frame.
    published_color_already_reported: bool,
    codec_identity: PhantomData<Identity>,
}

// As on the encode side: the identity is a marker with no value, so a derived
// `Default` would demand it be constructible.
impl<Identity: HardwareVideoCodecProcessorIdentity> Default
    for EncodedFrameToPublishedSurfaceDecoder<Identity>
{
    fn default() -> Self {
        Self {
            decode_session: None,
            gpu_context: None,
            sync_point_gate: EncodedStreamSyncPointGate::default(),
            stream_re_entry_report_schedule: None,
            session_coded_extent: None,
            session_needs_full_reset: false,
            frames_decoded: 0,
            published_color_already_reported: false,
            codec_identity: PhantomData,
        }
    }
}

impl<Identity: HardwareVideoCodecProcessorIdentity>
    EncodedFrameToPublishedSurfaceDecoder<Identity>
{
    pub fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
        config: &HardwareVideoDecoderConfig,
    ) -> Result<()> {
        let (max_width, max_height) =
            resolve_decoded_picture_buffer_dimension_caps(Identity::PROCESSOR_NAME, config);
        let session = ctx
            .gpu_full_access()
            .create_decoder_session(SimpleDecoderConfig {
                codec: Identity::VIDEO_SESSION_CODEC,
                max_width,
                max_height,
                // Decoded pictures come back RGBA via the engine's GPU
                // NV12→RGBA compute stage, which is what the pooled
                // `Rgba32` pixel buffer below is sized and formatted for.
                rgba_output: true,
                ..SimpleDecoderConfig::default()
            })
            .map_err(|mint_failure| {
                Error::Runtime(format!(
                    "{}: failed to mint the decoder session: {mint_failure}",
                    Identity::PROCESSOR_NAME
                ))
            })?;

        self.stream_re_entry_report_schedule = Some(
            CumulativeCountReportThreshold::reporting_every(STREAM_RE_ENTRY_REPORT_INTERVAL),
        );
        self.decode_session = Some(session);
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        tracing::info!(
            max_width,
            max_height,
            "{}: session minted; entering the stream at its next sync point",
            Identity::PROCESSOR_NAME
        );
        Ok(())
    }

    pub fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            frames_lost_to_gaps = self.sync_point_gate.bags_lost_to_gaps(),
            frames_discarded_awaiting_a_sync_point =
                self.sync_point_gate.bags_discarded_awaiting_a_sync_point(),
            "{}: teardown",
            Identity::PROCESSOR_NAME
        );
        self.decode_session.take();
        self.gpu_context.take();
        self.session_coded_extent = None;
        self.session_needs_full_reset = false;
        Ok(())
    }

    /// The context handle the per-bag decode needs. Taken once per tick that
    /// carries work, not per frame: the decode below needs `&mut self` for
    /// the session, the gate and the counters while it is live, and a handle
    /// clone is cheaper than a signature that hands borrowck six disjoint
    /// fields.
    pub fn gpu_context_for_this_tick(&self) -> Result<GpuContextLimitedAccess> {
        self.gpu_context.as_ref().cloned().ok_or_else(|| {
            Error::Runtime(format!(
                "{}: GPU context not initialized",
                Identity::PROCESSOR_NAME
            ))
        })
    }

    /// Read one arriving bag through the convention's own reader, apply the
    /// loss doctrine to it, and hand back whatever pictures it completed.
    /// Frames are pushed onto `staged` as they are staged, so a failure part
    /// way through a batch leaves the caller holding — and writing — every
    /// picture that did complete. Discarding those because a later one failed
    /// would lose frames the decoder had already reconstructed.
    pub fn decode_one_arriving_bag(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        bag_bytes: &[u8],
        frame_header_timestamp_ns: i64,
        staged: &mut Vec<DecodedFrameAwaitingPublication>,
    ) -> Result<()> {
        let encoded_frame = read_encoded_video_frame_bag(bag_bytes).map_err(|refusal| {
            Error::Runtime(format!("{}: {refusal}", Identity::PROCESSOR_NAME))
        })?;
        if let Some(refusal) = why_this_decoder_cannot_decode::<Identity>(encoded_frame.codec) {
            return Err(Error::Runtime(refusal));
        }
        self.break_continuity_if_the_producer_renegotiated_its_extent(&encoded_frame);

        match self
            .sync_point_gate
            .admit(encoded_frame.sequence_index, encoded_frame.is_sync_point)
        {
            ArrivingEncodedBagDisposition::Decode => {}
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint => {
                self.reset_parser_state_before_re_entering(&encoded_frame)?;
            }
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint => return Ok(()),
        }

        let (decoded_frames, published_color) = {
            let session = self.decode_session.as_mut().ok_or_else(|| {
                Error::Runtime(format!(
                    "{}: decoder session not initialized",
                    Identity::PROCESSOR_NAME
                ))
            })?;
            let decoded_frames = session
                .feed(&encoded_frame.annex_b_access_unit_bytes)
                .map_err(|decode_failure| {
                    Error::Runtime(format!(
                        "{}: decode failed: {decode_failure}",
                        Identity::PROCESSOR_NAME
                    ))
                })?;
            // The bitstream's own VUI outranks the producer's attestation:
            // it survives a muxer round trip that re-spelled the bag field,
            // and it is what the pictures were actually reconstructed under.
            let published_color =
                resolve_published_color(session.current_color_vui(), encoded_frame.color.as_ref());
            (decoded_frames, published_color)
        };

        for decoded_frame in decoded_frames {
            staged.push(self.stage_decoded_frame(
                gpu_context,
                decoded_frame,
                published_color.clone(),
                frame_header_timestamp_ns,
            )?);
        }
        Ok(())
    }

    /// A producer that renegotiated its extent publishes new parameter sets
    /// at a sync point without breaking `sequence_index` — the ordering pair
    /// cannot show it, so the coded extent on the bag is what does. The
    /// session must reconfigure from the new SPS rather than decode into a
    /// DPB sized for the old one.
    fn break_continuity_if_the_producer_renegotiated_its_extent(
        &mut self,
        encoded_frame: &EncodedVideoFrame,
    ) {
        let arriving_extent = (encoded_frame.width, encoded_frame.height);
        match self.session_coded_extent {
            Some(session_extent) if session_extent != arriving_extent => {
                tracing::info!(
                    session_width = session_extent.0,
                    session_height = session_extent.1,
                    arriving_width = arriving_extent.0,
                    arriving_height = arriving_extent.1,
                    "{}: the producer renegotiated its coded extent — reconfiguring from the \
                     next sync point",
                    Identity::PROCESSOR_NAME
                );
                self.session_coded_extent = Some(arriving_extent);
                self.session_needs_full_reset = true;
                self.sync_point_gate.break_continuity();
            }
            Some(_) => {}
            None => self.session_coded_extent = Some(arriving_extent),
        }
    }

    /// Drop the decode state a broken stream left behind, so the sync point
    /// about to be fed re-enters against nothing stale. A plain gap needs
    /// only the parser flush — the same parameter sets ride the incoming
    /// sync point. The full reset, whose session-parameter rebuild waits the
    /// whole device idle, is owed only when the extent renegotiated and new
    /// parameter sets are actually coming.
    fn reset_parser_state_before_re_entering(
        &mut self,
        encoded_frame: &EncodedVideoFrame,
    ) -> Result<()> {
        let session = self.decode_session.as_mut().ok_or_else(|| {
            Error::Runtime(format!(
                "{}: decoder session not initialized",
                Identity::PROCESSOR_NAME
            ))
        })?;
        if self.session_needs_full_reset {
            session.reset();
            self.session_needs_full_reset = false;
        } else {
            session.feed_discontinuity();
        }
        self.session_coded_extent = Some((encoded_frame.width, encoded_frame.height));

        let stream_re_entries = self.sync_point_gate.sync_points_entered_at();
        let worth_reporting = self
            .stream_re_entry_report_schedule
            .as_mut()
            .is_some_and(|schedule| schedule.count_is_worth_reporting(stream_re_entries));
        if worth_reporting {
            tracing::info!(
                sequence_index = encoded_frame.sequence_index,
                group_index = encoded_frame.group_index,
                stream_re_entries,
                frames_lost_to_gaps = self.sync_point_gate.bags_lost_to_gaps(),
                frames_discarded_awaiting_a_sync_point =
                    self.sync_point_gate.bags_discarded_awaiting_a_sync_point(),
                "{}: entering the stream at a sync point",
                Identity::PROCESSOR_NAME
            );
        }
        Ok(())
    }

    /// Stage one decoded picture into a pooled pixel buffer whose pool id
    /// becomes the frame's surface id. The extent is the session's own —
    /// already the stream's conformance window, so an H.265 stream's CTU
    /// padding is gone before a surface id ever names these pixels.
    fn stage_decoded_frame(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        decoded_frame: SimpleDecodedFrame,
        color_info: Option<ColorInfo>,
        frame_header_timestamp_ns: i64,
    ) -> Result<DecodedFrameAwaitingPublication> {
        if !decoded_frame.is_rgba {
            return Err(Error::Runtime(format!(
                "{}: the session handed back an NV12 picture though it was minted for RGBA \
                 output — the pooled pixel buffer is sized and formatted for RGBA",
                Identity::PROCESSOR_NAME
            )));
        }
        let width = decoded_frame.width;
        let height = decoded_frame.height;

        let (published_frame_id, pixel_buffer) =
            gpu_context.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;
        stage_tightly_packed_rgba_into_pooled_pixel_buffer(
            &pixel_buffer,
            &decoded_frame.data,
            width,
            height,
        )
        .map_err(|staging_failure| {
            Error::Runtime(format!("{}: {staging_failure}", Identity::PROCESSOR_NAME))
        })?;

        let frame = VideoFrame {
            surface_id: published_frame_id.to_string(),
            width,
            height,
            timestamp_ns: frame_header_timestamp_ns,
            color_info,
            // Rate, HDR sidecars and a per-frame layout override are not
            // things a decoded elementary stream knows.
            fps: None,
            content_light: None,
            mastering_display: None,
            texture_layout: None,
        };
        if !self.published_color_already_reported {
            tracing::info!(
                width,
                height,
                color_info = ?frame.color_info,
                "{}: first frame decoded",
                Identity::PROCESSOR_NAME
            );
            self.published_color_already_reported = true;
        }

        self.frames_decoded += 1;
        if self
            .frames_decoded
            .is_multiple_of(DECODE_PROGRESS_LOG_INTERVAL_FRAMES)
        {
            tracing::info!(
                frames_decoded = self.frames_decoded,
                "{}: decode progress",
                Identity::PROCESSOR_NAME
            );
        }
        Ok(DecodedFrameAwaitingPublication {
            frame,
            _pooled_pixel_buffer_held_until_written: pixel_buffer,
        })
    }
}

/// Resolve the DPB allocation caps from config. `0` is the session
/// surface's spelling of "auto-detect from the first SPS"; a half-specified
/// pair caps nothing a DPB can be sized from, so it warns and auto-detects
/// rather than allocating against one axis.
fn resolve_decoded_picture_buffer_dimension_caps(
    processor_name: &'static str,
    config: &HardwareVideoDecoderConfig,
) -> (u32, u32) {
    match (config.max_width, config.max_height) {
        (Some(max_width), Some(max_height)) => (max_width, max_height),
        (None, None) => (0, 0),
        (max_width, max_height) => {
            tracing::warn!(
                ?max_width,
                ?max_height,
                "{processor_name}: max_width and max_height cap the DPB together or not at \
                 all; auto-detecting both from the first SPS"
            );
            (0, 0)
        }
    }
}

/// Why this decoder cannot decode a bag naming `codec`, or `None` when it
/// can. Refusal names both spellings rather than reshaping an elementary
/// stream it does not know into one it does.
fn why_this_decoder_cannot_decode<Identity: HardwareVideoCodecProcessorIdentity>(
    codec: EncodedVideoCodec,
) -> Option<String> {
    (codec != Identity::ENCODED_VIDEO_CODEC).then(|| {
        format!(
            "the bag names codec `\"{}\"`, which this decoder cannot decode — it decodes \
             `\"{}\"`",
            codec.as_wire_str(),
            Identity::ENCODED_VIDEO_CODEC.as_wire_str(),
        )
    })
}

/// The color a decoded frame is published with: the stream's own parsed VUI
/// when the bitstream carried one, the producer's bag attestation otherwise.
fn resolve_published_color(
    parsed_sps_color_vui: Option<H273ColorVui>,
    producer_attested_color: Option<&ColorInfo>,
) -> Option<ColorInfo> {
    parsed_sps_color_vui
        .as_ref()
        .and_then(h273_color_vui_to_color_info)
        .or_else(|| producer_attested_color.cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h264_decoder::H264DecoderCodecIdentity;
    use crate::h265_decoder::H265DecoderCodecIdentity;
    use crate::video_frame::{Matrix, Primaries, Range, Transfer};

    #[test]
    fn an_absent_config_auto_detects_both_dimensions_from_the_first_sps() {
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps(
                "H265Decoder",
                &HardwareVideoDecoderConfig::default()
            ),
            (0, 0)
        );
    }

    #[test]
    fn a_fully_specified_config_caps_the_dpb_at_what_it_states() {
        let config = HardwareVideoDecoderConfig {
            max_width: Some(1920),
            max_height: Some(1080),
        };
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps("H265Decoder", &config),
            (1920, 1080)
        );
    }

    /// A cap on one axis alone describes no allocation, so it must fall all
    /// the way back to auto-detection rather than half-applying.
    #[test]
    fn a_half_specified_cap_falls_back_to_auto_detecting_both() {
        let width_only = HardwareVideoDecoderConfig {
            max_width: Some(1920),
            max_height: None,
        };
        let height_only = HardwareVideoDecoderConfig {
            max_width: None,
            max_height: Some(1080),
        };
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps("H265Decoder", &width_only),
            (0, 0)
        );
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps("H265Decoder", &height_only),
            (0, 0)
        );
    }

    /// `rt.add(H265Decoder)` with no config at all must deserialize.
    #[test]
    fn an_all_absent_decoder_config_deserializes_to_defaults() {
        let config: HardwareVideoDecoderConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config, HardwareVideoDecoderConfig::default());
    }

    /// Each decoder decodes exactly one elementary stream and says so by
    /// name. The shared body makes this the one place the two built-ins can
    /// diverge, so it is the one place worth asserting both directions.
    #[test]
    fn a_bag_of_the_other_codec_is_refused_naming_both_codecs() {
        let refused_by_h264 =
            why_this_decoder_cannot_decode::<H264DecoderCodecIdentity>(EncodedVideoCodec::H265)
                .expect("an H.265 bag must be refused by the H.264 decoder");
        assert!(
            refused_by_h264.contains("h265") && refused_by_h264.contains("h264"),
            "{refused_by_h264}"
        );
        let refused_by_h265 =
            why_this_decoder_cannot_decode::<H265DecoderCodecIdentity>(EncodedVideoCodec::H264)
                .expect("an H.264 bag must be refused by the H.265 decoder");
        assert!(
            refused_by_h265.contains("h264") && refused_by_h265.contains("h265"),
            "{refused_by_h265}"
        );

        assert_eq!(
            why_this_decoder_cannot_decode::<H264DecoderCodecIdentity>(EncodedVideoCodec::H264),
            None
        );
        assert_eq!(
            why_this_decoder_cannot_decode::<H265DecoderCodecIdentity>(EncodedVideoCodec::H265),
            None
        );
    }

    #[test]
    fn a_parsed_bitstream_vui_outranks_the_producers_bag_attestation() {
        let attested = ColorInfo {
            primaries: Some(Primaries::Bt2020),
            transfer: Some(Transfer::Smpte2084),
            matrix: Some(Matrix::Bt2020Ncl),
            range: Some(Range::Full),
        };
        let parsed = H273ColorVui {
            primaries: Some(1),
            transfer: Some(13),
            matrix: Some(1),
            full_range: Some(false),
        };
        assert_eq!(
            resolve_published_color(Some(parsed), Some(&attested)),
            Some(ColorInfo {
                primaries: Some(Primaries::Bt709),
                transfer: Some(Transfer::Srgb),
                matrix: Some(Matrix::Bt709),
                range: Some(Range::Limited),
            })
        );
    }

    #[test]
    fn a_bitstream_carrying_no_vui_falls_back_to_the_producers_attestation() {
        let attested = ColorInfo {
            primaries: Some(Primaries::Bt709),
            transfer: Some(Transfer::Srgb),
            matrix: None,
            range: Some(Range::Full),
        };
        assert_eq!(
            resolve_published_color(None, Some(&attested)),
            Some(attested.clone())
        );
        assert_eq!(
            resolve_published_color(Some(H273ColorVui::default()), Some(&attested)),
            Some(attested)
        );
        assert_eq!(resolve_published_color(None, None), None);
    }
}
