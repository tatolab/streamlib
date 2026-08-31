// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in H.264 decoder: encoded-frame bags in, published video surfaces
//! out, via the engine's Vulkan Video session surface.
//!
//! The session mints in `setup()`, whose typestate is already Full, with its
//! coded dimensions auto-detected from the stream's first SPS. Decoded
//! pictures are read back as RGBA and staged into pooled pixel buffers whose
//! pool id is the published `surface_id` — the same CPU→GPU hand-off the
//! camera uses.
//!
//! One session serves one coded extent. A producer that renegotiates — the
//! shipped encoder re-mints at a new extent and opens the new stream at a
//! sync point without breaking `sequence_index` — is noticed by the extent on
//! the bag, which is why the convention carries it: the session resets and
//! the gate re-enters, rather than feeding new parameter sets into a session
//! configured for the old ones.
//!
//! A consumer of an encoded stream must bound loss, so this one enters the
//! stream at a sync point and discards back to one after a `sequence_index`
//! gap: it never hands a decoder slices whose reference frames it did not
//! see, and never publishes a picture built from them.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::engine::video::decode::{
    SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig,
};
use streamlib::sdk::engine::video::{Codec, H273ColorVui};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;
use streamlib::sdk::rhi::PixelFormat;

use crate::cumulative_count_report_threshold::CumulativeCountReportThreshold;
use crate::encoded_video_frame::{
    ArrivingEncodedFrameDisposition, EncodedStreamSyncPointGate, EncodedVideoCodec,
    EncodedVideoFrame, read_encoded_video_frame_bag,
};
use crate::h273_color_vui_translation::h273_color_vui_to_color_info;
use crate::pooled_rgba_frame_staging::stage_tightly_packed_rgba_into_pooled_pixel_buffer;
use crate::video_frame::{ColorInfo, VideoFrame};

/// Decode-progress log cadence, in frames.
const DECODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// Stream re-entries between reports. A healthy run enters once and says so;
/// a persistently lossy link re-enters at the producer's GOP cadence, and
/// saying that every time buries the run in the symptom.
const STREAM_RE_ENTRY_REPORT_INTERVAL: u64 = 20;

/// Configuration for [`H264Decoder`]. Both knobs cap the extent the decoded
/// picture buffer is allocated for, and both are optional: absent, the extent
/// is auto-detected from the stream's first SPS, which is what a decoder fed
/// by an unknown producer wants. The DPB's slot count is the session
/// surface's own and is not configurable here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct H264DecoderConfig {
    /// Upper bound on the coded width the DPB is allocated for. Absent:
    /// auto-detected from the first SPS.
    #[serde(default)]
    pub max_width: Option<u32>,
    /// Upper bound on the coded height, paired with [`Self::max_width`].
    #[serde(default)]
    pub max_height: Option<u32>,
}

#[streamlib::sdk::processor(
    description = "Decodes H.264 Annex-B encoded-frame bags to published video surfaces via Vulkan Video hardware decode",
    execution = reactive,
    scheduling = high,
    config = crate::h264_decoder::H264DecoderConfig,
    input(
        "encoded_video",
        delivery_profile = "ordered",
        description = "H.264 encoded-frame bags to decode"
    ),
    output("video", description = "Decoded video frames"),
)]
pub struct H264Decoder {
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
}

impl ReactiveProcessor for H264Decoder::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let (max_width, max_height) = resolve_decoded_picture_buffer_dimension_caps(&self.config);
        let session = ctx
            .gpu_full_access()
            .create_decoder_session(SimpleDecoderConfig {
                codec: Codec::H264,
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
                    "H264Decoder: failed to mint the decoder session: {mint_failure}"
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
            "H264Decoder: session minted; entering the stream at its next sync point"
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            frames_lost_to_gaps = self.sync_point_gate.frames_lost_to_gaps(),
            frames_discarded_awaiting_a_sync_point = self
                .sync_point_gate
                .frames_discarded_awaiting_a_sync_point(),
            "H264Decoder: teardown"
        );
        self.decode_session.take();
        self.gpu_context.take();
        self.session_coded_extent = None;
        self.session_needs_full_reset = false;
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("encoded_video") {
            return Ok(());
        }
        // Cloned once per tick that carries work, not per frame: the publish
        // below needs `&mut self` for the session, the gate and the outputs
        // while this is live, and a handle clone is cheaper than a signature
        // that hands borrowck six disjoint fields.
        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Runtime("H264Decoder: GPU context not initialized".into()))?
            .clone();

        while let Some((bag_bytes, frame_header_timestamp_ns)) =
            self.inputs.read_raw("encoded_video")?
        {
            self.decode_one_arriving_bag(&gpu_context, &bag_bytes, frame_header_timestamp_ns)?;
        }
        Ok(())
    }
}

impl H264Decoder::Processor {
    /// Read one arriving bag through the convention's own reader, apply the
    /// loss doctrine to it, and publish whatever pictures it completed.
    fn decode_one_arriving_bag(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        bag_bytes: &[u8],
        frame_header_timestamp_ns: i64,
    ) -> Result<()> {
        let encoded_frame = read_encoded_video_frame_bag(bag_bytes)
            .map_err(|refusal| Error::Runtime(format!("H264Decoder: {refusal}")))?;
        if let Some(refusal) = why_this_decoder_cannot_decode(encoded_frame.codec) {
            return Err(Error::Runtime(refusal));
        }
        self.break_continuity_if_the_producer_renegotiated_its_extent(&encoded_frame);

        match self
            .sync_point_gate
            .admit(encoded_frame.sequence_index, encoded_frame.is_sync_point)
        {
            ArrivingEncodedFrameDisposition::Decode => {}
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint => {
                self.reset_parser_state_before_re_entering(&encoded_frame)?;
            }
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint => return Ok(()),
        }

        let (decoded_frames, published_color) = {
            let session = self.decode_session.as_mut().ok_or_else(|| {
                Error::Runtime("H264Decoder: decoder session not initialized".into())
            })?;
            let decoded_frames = session
                .feed(&encoded_frame.annex_b_access_unit_bytes)
                .map_err(|decode_failure| {
                    Error::Runtime(format!("H264Decoder: decode failed: {decode_failure}"))
                })?;
            // The bitstream's own VUI outranks the producer's attestation:
            // it survives a muxer round trip that re-spelled the bag field,
            // and it is what the pictures were actually reconstructed under.
            let published_color =
                resolve_published_color(session.current_color_vui(), encoded_frame.color.as_ref());
            (decoded_frames, published_color)
        };

        for decoded_frame in decoded_frames {
            self.publish_decoded_frame(
                gpu_context,
                decoded_frame,
                published_color.clone(),
                frame_header_timestamp_ns,
            )?;
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
                    "H264Decoder: the producer renegotiated its coded extent — reconfiguring \
                     from the next sync point"
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
        let session = self
            .decode_session
            .as_mut()
            .ok_or_else(|| Error::Runtime("H264Decoder: decoder session not initialized".into()))?;
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
                frames_lost_to_gaps = self.sync_point_gate.frames_lost_to_gaps(),
                frames_discarded_awaiting_a_sync_point = self
                    .sync_point_gate
                    .frames_discarded_awaiting_a_sync_point(),
                "H264Decoder: entering the stream at a sync point"
            );
        }
        Ok(())
    }

    /// Stage one decoded picture into a pooled pixel buffer and publish its
    /// pool id as the frame's surface id. The pixel buffer is held across
    /// the write so the pool cannot rotate the slot out mid-flight.
    fn publish_decoded_frame(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        decoded_frame: SimpleDecodedFrame,
        color_info: Option<ColorInfo>,
        frame_header_timestamp_ns: i64,
    ) -> Result<()> {
        if !decoded_frame.is_rgba {
            return Err(Error::Runtime(
                "H264Decoder: the session handed back an NV12 picture though it was minted for \
                 RGBA output — the pooled pixel buffer is sized and formatted for RGBA"
                    .into(),
            ));
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
        .map_err(|staging_failure| Error::Runtime(format!("H264Decoder: {staging_failure}")))?;

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
                "H264Decoder: first frame decoded"
            );
            self.published_color_already_reported = true;
        }
        self.outputs
            .write_with_timestamp("video", &frame, frame_header_timestamp_ns)?;
        drop(pixel_buffer);

        self.frames_decoded += 1;
        if self
            .frames_decoded
            .is_multiple_of(DECODE_PROGRESS_LOG_INTERVAL_FRAMES)
        {
            tracing::info!(
                frames_decoded = self.frames_decoded,
                "H264Decoder: decode progress"
            );
        }
        Ok(())
    }
}

/// Resolve the DPB allocation caps from config. `0` is the session
/// surface's spelling of "auto-detect from the first SPS"; a half-specified
/// pair caps nothing a DPB can be sized from, so it warns and auto-detects
/// rather than allocating against one axis.
fn resolve_decoded_picture_buffer_dimension_caps(config: &H264DecoderConfig) -> (u32, u32) {
    match (config.max_width, config.max_height) {
        (Some(max_width), Some(max_height)) => (max_width, max_height),
        (None, None) => (0, 0),
        (max_width, max_height) => {
            tracing::warn!(
                ?max_width,
                ?max_height,
                "H264Decoder: max_width and max_height cap the DPB together or not at all; \
                 auto-detecting both from the first SPS"
            );
            (0, 0)
        }
    }
}

/// Why this decoder cannot decode a bag naming `codec`, or `None` when it
/// can. Refusal names both spellings rather than reshaping an elementary
/// stream it does not know into one it does.
fn why_this_decoder_cannot_decode(codec: EncodedVideoCodec) -> Option<String> {
    (codec != EncodedVideoCodec::H264).then(|| {
        format!(
            "the bag names codec `\"{}\"`, which this decoder cannot decode — it decodes \
             `\"{}\"`",
            codec.as_wire_str(),
            EncodedVideoCodec::H264.as_wire_str(),
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
    use crate::video_frame::{Matrix, Primaries, Range, Transfer};

    #[test]
    fn an_absent_config_auto_detects_both_dimensions_from_the_first_sps() {
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps(&H264DecoderConfig::default()),
            (0, 0)
        );
    }

    #[test]
    fn a_fully_specified_config_caps_the_dpb_at_what_it_states() {
        let config = H264DecoderConfig {
            max_width: Some(1920),
            max_height: Some(1080),
        };
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps(&config),
            (1920, 1080)
        );
    }

    /// A cap on one axis alone describes no allocation, so it must fall all
    /// the way back to auto-detection rather than half-applying.
    #[test]
    fn a_half_specified_cap_falls_back_to_auto_detecting_both() {
        let width_only = H264DecoderConfig {
            max_width: Some(1920),
            max_height: None,
        };
        let height_only = H264DecoderConfig {
            max_width: None,
            max_height: Some(1080),
        };
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps(&width_only),
            (0, 0)
        );
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps(&height_only),
            (0, 0)
        );
    }

    /// `rt.add(H264Decoder)` with no config at all must deserialize.
    #[test]
    fn an_all_absent_decoder_config_deserializes_to_defaults() {
        let config: H264DecoderConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config, H264DecoderConfig::default());
    }

    #[test]
    fn an_h265_bag_reaching_this_decoder_is_refused_naming_both_codecs() {
        let refusal = why_this_decoder_cannot_decode(EncodedVideoCodec::H265)
            .expect("an H.265 bag must be refused");
        assert!(
            refusal.contains("h265") && refusal.contains("h264"),
            "{refusal}"
        );
        assert_eq!(
            why_this_decoder_cannot_decode(EncodedVideoCodec::H264),
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
