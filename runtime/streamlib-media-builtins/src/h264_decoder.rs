// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in H.264 decoder: encoded-frame bags in, published video surfaces
//! out, via the engine's Vulkan Video session surface.
//!
//! The session mints in `setup()`, whose typestate is already Full, with the
//! DPB auto-sized from the stream's first SPS. Decoded pictures are read back
//! as RGBA and staged into pooled pixel buffers whose pool id is the
//! published `surface_id` — the same CPU→GPU hand-off the camera uses.
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

use crate::encoded_video_frame::{
    EncodedVideoCodec, EncodedVideoFrame, read_encoded_video_frame_bag,
};
use crate::h273_color_vui_translation::h273_color_vui_to_color_info;
use crate::video_frame::{ColorInfo, VideoFrame};

/// Decode-progress log cadence, in frames.
const DECODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// Configuration for [`H264Decoder`]. Both knobs are DPB-allocation
/// guardrails and both are optional: absent, the coded dimensions and the
/// DPB size are auto-detected from the stream's first SPS, which is what a
/// decoder fed by an unknown producer wants.
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

/// What the loss doctrine says to do with one arriving encoded frame, given
/// everything the gate has seen on this link before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivingEncodedFrameDisposition {
    /// Feed it: it continues a stream whose continuity is intact.
    Decode,
    /// Reset the decoder's parser state, then feed it: this frame is the
    /// sync point that re-enters a stream whose continuity was broken.
    ReEnterAtThisSyncPoint,
    /// Discard it: the stream's continuity is broken and this frame is not
    /// a re-entry point, so its reference frames were never seen.
    DiscardUntilTheNextSyncPoint,
}

/// Per-link gate applying the decided loss doctrine to an encoded stream: a
/// consumer that sees a `sequence_index` gap discards until the producer's
/// next sync point, and never forwards a stream it knows is broken.
///
/// It opens broken, because the first bag a subscriber receives is not
/// necessarily the first bag the producer published — an attach mid-GOP
/// hands over slices whose IDR is already gone, and feeding those is exactly
/// how a decoder ends a run having decoded nothing.
#[derive(Debug, Default)]
pub struct EncodedStreamSyncPointGate {
    /// `None` until the first frame arrives; afterwards the newest
    /// `sequence_index` seen, decoded or discarded.
    newest_sequence_index_seen: Option<u64>,
    awaiting_a_sync_point: bool,
    frames_lost_to_gaps: u64,
    frames_discarded_awaiting_a_sync_point: u64,
}

impl EncodedStreamSyncPointGate {
    /// Open a gate that has seen nothing and is therefore waiting for a
    /// sync point to enter the stream at.
    pub fn opening_at_the_next_sync_point() -> Self {
        Self {
            newest_sequence_index_seen: None,
            awaiting_a_sync_point: true,
            frames_lost_to_gaps: 0,
            frames_discarded_awaiting_a_sync_point: 0,
        }
    }

    /// Admit one arriving frame, accounting the gap it exposes.
    pub fn admit(
        &mut self,
        sequence_index: u64,
        is_sync_point: bool,
    ) -> ArrivingEncodedFrameDisposition {
        if let Some(newest_seen) = self.newest_sequence_index_seen
            && sequence_index != newest_seen + 1
        {
            // Any step other than exactly one breaks continuity: a forward
            // jump is loss, and a repeat or a step backwards is a producer
            // this reader's decode state cannot describe either way.
            self.frames_lost_to_gaps += sequence_index.saturating_sub(newest_seen + 1);
            self.awaiting_a_sync_point = true;
        }
        self.newest_sequence_index_seen = Some(sequence_index);

        if !self.awaiting_a_sync_point {
            return ArrivingEncodedFrameDisposition::Decode;
        }
        if is_sync_point {
            self.awaiting_a_sync_point = false;
            return ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint;
        }
        self.frames_discarded_awaiting_a_sync_point += 1;
        ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
    }

    /// How many frames the `sequence_index` gaps say the link lost.
    pub fn frames_lost_to_gaps(&self) -> u64 {
        self.frames_lost_to_gaps
    }

    /// How many arriving frames were discarded because they were not a
    /// re-entry point into a broken stream.
    pub fn frames_discarded_awaiting_a_sync_point(&self) -> u64 {
        self.frames_discarded_awaiting_a_sync_point
    }
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
    frames_decoded: u64,
    /// The color the published frames carry, resolved once the stream's SPS
    /// has been parsed, and said once rather than per frame.
    published_color_already_reported: bool,
}

impl ReactiveProcessor for H264Decoder::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let (max_width, max_height) = resolve_decode_pool_dimension_caps(&self.config);
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

        self.sync_point_gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
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
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
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
        if encoded_frame.codec != EncodedVideoCodec::H264 {
            return Err(Error::Runtime(format!(
                "H264Decoder: the bag names codec `\"{}\"`, which this decoder cannot decode — \
                 it decodes `\"{}\"`",
                encoded_frame.codec.as_wire_str(),
                EncodedVideoCodec::H264.as_wire_str(),
            )));
        }

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

    /// Drop the decode state a broken stream left behind, so the sync point
    /// about to be fed re-enters against nothing stale.
    fn reset_parser_state_before_re_entering(
        &mut self,
        encoded_frame: &EncodedVideoFrame,
    ) -> Result<()> {
        let session = self
            .decode_session
            .as_mut()
            .ok_or_else(|| Error::Runtime("H264Decoder: decoder session not initialized".into()))?;
        session.feed_discontinuity();
        tracing::info!(
            sequence_index = encoded_frame.sequence_index,
            group_index = encoded_frame.group_index,
            frames_lost_to_gaps = self.sync_point_gate.frames_lost_to_gaps(),
            frames_discarded_awaiting_a_sync_point = self
                .sync_point_gate
                .frames_discarded_awaiting_a_sync_point(),
            "H264Decoder: entering the stream at a sync point"
        );
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
        let rgba_byte_count = (width as usize) * (height as usize) * 4;
        if decoded_frame.data.len() < rgba_byte_count {
            return Err(Error::Runtime(format!(
                "H264Decoder: the decoded picture is {} bytes, short of the {rgba_byte_count} \
                 its {width}x{height} RGBA extent needs",
                decoded_frame.data.len()
            )));
        }

        let (published_frame_id, pixel_buffer) =
            gpu_context.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;
        let plane_pointer = pixel_buffer.plane_base_address(0);
        let plane_size = pixel_buffer.plane_size(0) as usize;
        if plane_pointer.is_null() || plane_size < rgba_byte_count {
            return Err(Error::Runtime(format!(
                "H264Decoder: pixel-buffer plane unusable (ptr null: {}, size {plane_size} < \
                 expected {rgba_byte_count})",
                plane_pointer.is_null()
            )));
        }
        // SAFETY: `plane_pointer` is the mapped host-visible base of plane 0
        // of a freshly-acquired `Rgba32` pixel buffer, valid for `plane_size`
        // bytes and checked above to be at least `rgba_byte_count`; the
        // source is a distinct owned buffer of at least that length, so the
        // regions cannot overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(
                decoded_frame.data.as_ptr(),
                plane_pointer,
                rgba_byte_count,
            );
        }

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
fn resolve_decode_pool_dimension_caps(config: &H264DecoderConfig) -> (u32, u32) {
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
            resolve_decode_pool_dimension_caps(&H264DecoderConfig::default()),
            (0, 0)
        );
    }

    #[test]
    fn a_fully_specified_config_caps_the_dpb_at_what_it_states() {
        let config = H264DecoderConfig {
            max_width: Some(1920),
            max_height: Some(1080),
        };
        assert_eq!(resolve_decode_pool_dimension_caps(&config), (1920, 1080));
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
        assert_eq!(resolve_decode_pool_dimension_caps(&width_only), (0, 0));
        assert_eq!(resolve_decode_pool_dimension_caps(&height_only), (0, 0));
    }

    /// `rt.add(H264Decoder)` with no config at all must deserialize.
    #[test]
    fn an_all_absent_decoder_config_deserializes_to_defaults() {
        let config: H264DecoderConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config, H264DecoderConfig::default());
    }

    /// The #1077 shape, as logic: a subscriber that attaches mid-GOP is
    /// handed slices whose IDR is already gone. Feeding those is what ends
    /// a run at `frames_decoded = 0`; the gate discards them and enters at
    /// the producer's next sync point instead.
    #[test]
    fn a_stream_joined_mid_group_is_discarded_until_its_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();

        assert_eq!(
            gate.admit(7, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(8, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(9, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(10, false),
            ArrivingEncodedFrameDisposition::Decode
        );
        assert_eq!(gate.frames_discarded_awaiting_a_sync_point(), 2);
        // Contiguous arrivals are not loss, however late the join was.
        assert_eq!(gate.frames_lost_to_gaps(), 0);
    }

    /// The decided loss doctrine: a `sequence_index` gap breaks the stream,
    /// and every frame until the producer's next sync point is discarded
    /// rather than decoded against reference frames that were never seen.
    #[test]
    fn a_sequence_index_gap_discards_until_the_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(1, false),
            ArrivingEncodedFrameDisposition::Decode
        );

        // 2 and 3 were overwritten in the ring; 4 is a non-sync-point.
        assert_eq!(
            gate.admit(4, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(5, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(6, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(7, false),
            ArrivingEncodedFrameDisposition::Decode
        );

        assert_eq!(gate.frames_lost_to_gaps(), 2);
        assert_eq!(gate.frames_discarded_awaiting_a_sync_point(), 2);
    }

    /// A gap landing exactly on a sync point costs nothing but the gap: the
    /// sync point is itself the re-entry point, so nothing is discarded.
    #[test]
    fn a_gap_landing_on_a_sync_point_re_enters_without_discarding_anything() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(30, true),
            ArrivingEncodedFrameDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.frames_lost_to_gaps(), 29);
        assert_eq!(gate.frames_discarded_awaiting_a_sync_point(), 0);
    }

    /// `sequence_index` is monotonic for the life of a producer, so a repeat
    /// or a step backwards describes a stream this reader's decode state
    /// cannot continue — it re-enters rather than decoding on.
    #[test]
    fn a_sequence_index_that_does_not_advance_by_one_breaks_continuity_too() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        gate.admit(0, true);
        gate.admit(1, false);
        assert_eq!(
            gate.admit(1, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(0, false),
            ArrivingEncodedFrameDisposition::DiscardUntilTheNextSyncPoint
        );
        // Neither backwards step is counted as frames lost — no frame went
        // missing, the producer's numbering stopped making sense.
        assert_eq!(gate.frames_lost_to_gaps(), 0);
        assert_eq!(gate.frames_discarded_awaiting_a_sync_point(), 2);
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
