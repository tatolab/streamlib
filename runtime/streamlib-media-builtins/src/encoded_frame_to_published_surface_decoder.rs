// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The decode body both hardware video decoder built-ins are: media frames
//! in, published video surfaces out, via the video codec seam.
//!
//! The session mints in `setup()`, whose typestate is already Full, with its
//! coded dimensions auto-detected from the stream's first SPS. Decoded
//! pictures arrive in pooled pixel buffers whose pool id is the published
//! `surface_id`, already cropped to the stream's conformance window by the
//! session, so the CTU padding an H.265 stream codes never reaches a
//! consumer.
//!
//! One session serves one set of parameter sets. A producer that renegotiates
//! — the shipped encoders re-mint at a new extent and open the new stream at a
//! keyframe — is noticed by a keyframe whose parameter sets differ from the
//! ones the session was fed: the session resets rather than feeding new
//! parameter sets into a session configured for the old ones.
//!
//! A consumer of an encoded stream must bound loss, so this one enters the
//! stream at a group start and, after a read reports a loss, discards to the
//! next one: it never hands a decoder slices whose reference frames it did not
//! see, and never publishes a picture built from them.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    DecodedVideoPictureInPooledPixelBuffer, RuntimeContextFullAccess,
    VideoDecodeMaximumCodedExtent, VideoDecodeSession, VideoDecodeSessionRequest,
    probe_video_codec_backend,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::hang_legacy_container_media_frame::read_hang_legacy_container_media_frame;
use streamlib::sdk::iceoryx2::{
    MessageGroupBoundary, MessageGroupLossRecoveryDisposition, MessageGroupLossRecoveryGate,
    MessageReadFromInputPort,
};
use streamlib::sdk::rhi::PixelBuffer;
use streamlib::sdk::schemars::JsonSchema;
use streamlib::sdk::video_bitstream::{
    ParameterSetsFromAnnexBAccessUnit, parameter_sets_of_annex_b_access_unit,
};

use crate::cumulative_count_report_threshold::CumulativeCountReportThreshold;
use crate::h273_color_vui_translation::h273_color_vui_to_color_info;
use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;
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
/// the codec seam's own and is not configurable here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct HardwareVideoDecoderConfig {
    /// Upper bound on the coded width the DPB is allocated for. Absent:
    /// auto-detected from the first SPS. It caps the extent together with
    /// [`Self::max_height`] or not at all: a half-specified pair warns and
    /// auto-detects both. On macOS, where CoreMedia reports only the cropped
    /// extent, a stream whose pictures pass the pair is refused by name.
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
    decode_session: Option<Box<dyn VideoDecodeSession>>,
    /// Where the session pushes the pictures one bag completes; kept across
    /// bags so the hot path reuses its capacity.
    decoded_pictures_from_session: Vec<DecodedVideoPictureInPooledPixelBuffer>,
    group_loss_recovery_gate: MessageGroupLossRecoveryGate,
    stream_re_entry_report_schedule: Option<CumulativeCountReportThreshold>,
    /// The parameter sets the session was last fed, compared against every
    /// later keyframe's. A plain loss re-enters with a parser flush alone —
    /// the full reset rebuilds session parameters, and that rebuild waits the
    /// whole device idle, which stalls every producer once per loss exactly
    /// when the decoder most needs to catch up.
    session_parameter_sets: Option<ParameterSetsFromAnnexBAccessUnit>,
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
            decoded_pictures_from_session: Vec::new(),
            group_loss_recovery_gate: MessageGroupLossRecoveryGate::default(),
            stream_re_entry_report_schedule: None,
            session_parameter_sets: None,
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
        let maximum_coded_extent =
            resolve_decoded_picture_buffer_dimension_caps(Identity::PROCESSOR_NAME, config);
        let processor_id_of_the_decoding_node = ctx.processor_id().ok_or_else(|| {
            Error::Configuration(format!(
                "{} needs its node's processor id to give its pictures a node output pool; \
                 this context carries none",
                Identity::PROCESSOR_NAME
            ))
        })?;
        let backend = probe_video_codec_backend();
        let session = backend
            .open_decode_session(
                ctx.gpu_full_access(),
                &VideoDecodeSessionRequest {
                    elementary_stream: Identity::VIDEO_CODEC_ELEMENTARY_STREAM,
                    maximum_coded_extent,
                    processor_id_of_the_decoding_node,
                },
            )
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
        tracing::info!(
            video_codec_backend = backend.backend_name(),
            ?maximum_coded_extent,
            "{}: session minted; entering the stream at its next group start",
            Identity::PROCESSOR_NAME
        );
        Ok(())
    }

    pub fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            frames_lost_on_the_link = self.group_loss_recovery_gate.messages_lost_on_the_link(),
            frames_discarded_awaiting_a_group_start = self
                .group_loss_recovery_gate
                .messages_discarded_awaiting_a_group_start(),
            "{}: teardown",
            Identity::PROCESSOR_NAME
        );
        self.decode_session.take();
        self.session_parameter_sets = None;
        Ok(())
    }

    /// Read one arriving message as a media frame, apply the loss doctrine
    /// to it, and hand back whatever pictures it completed. Frames are pushed
    /// onto `staged` as they are staged, so a failure part way through a batch
    /// leaves the caller holding — and writing — every picture that did
    /// complete. Discarding those because a later one failed would lose frames
    /// the decoder had already reconstructed.
    pub fn decode_one_arriving_message(
        &mut self,
        message: &MessageReadFromInputPort,
        staged: &mut Vec<DecodedFrameAwaitingPublication>,
    ) -> Result<()> {
        let disposition = self
            .group_loss_recovery_gate
            .admit_message(message.group_boundary, message.lost_since_last_read);
        if disposition == MessageGroupLossRecoveryDisposition::DiscardUntilTheNextGroupStart {
            return Ok(());
        }
        let media_frame =
            read_hang_legacy_container_media_frame(&message.payload_bytes).map_err(|refusal| {
                Error::Runtime(format!("{}: {refusal}", Identity::PROCESSOR_NAME))
            })?;
        let annex_b_access_unit_bytes = media_frame.codec_payload_bytes;
        if message.group_boundary == MessageGroupBoundary::StartsGroup {
            self.prepare_the_session_for_a_group_start(annex_b_access_unit_bytes, disposition)?;
        }

        let session = self.decode_session.as_mut().ok_or_else(|| {
            Error::Runtime(format!(
                "{}: decoder session not initialized",
                Identity::PROCESSOR_NAME
            ))
        })?;
        let decode_outcome = session
            .decode_annex_b_access_unit(
                annex_b_access_unit_bytes,
                &mut self.decoded_pictures_from_session,
            )
            .map_err(|decode_failure| {
                Error::Runtime(format!(
                    "{}: decode failed: {decode_failure}",
                    Identity::PROCESSOR_NAME
                ))
            });
        let published_color = session
            .parsed_parameter_set_color_vui()
            .as_ref()
            .and_then(h273_color_vui_to_color_info);

        let mut decoded_pictures = std::mem::take(&mut self.decoded_pictures_from_session);
        staged.extend(decoded_pictures.drain(..).map(|decoded_picture| {
            self.decoded_frame_awaiting_publication(
                decoded_picture,
                published_color.clone(),
                message.timestamp_ns,
            )
        }));
        self.decoded_pictures_from_session = decoded_pictures;
        decode_outcome
    }

    /// Ready the session for the group start about to be fed. Parameter sets
    /// other than the ones the session was fed mean the producer renegotiated
    /// and the session resets in full. Otherwise only a re-entry after a loss
    /// owes anything — the parser flush, since the same parameter sets ride
    /// the incoming keyframe.
    fn prepare_the_session_for_a_group_start(
        &mut self,
        annex_b_access_unit_bytes: &[u8],
        disposition: MessageGroupLossRecoveryDisposition,
    ) -> Result<()> {
        let session = self.decode_session.as_mut().ok_or_else(|| {
            Error::Runtime(format!(
                "{}: decoder session not initialized",
                Identity::PROCESSOR_NAME
            ))
        })?;
        let arriving_parameter_sets = parameter_sets_of_annex_b_access_unit(
            annex_b_access_unit_bytes,
            Identity::VIDEO_CODEC_ELEMENTARY_STREAM,
        );
        match session_preparation_for_a_group_start(
            self.session_parameter_sets.as_ref(),
            &arriving_parameter_sets,
            disposition,
        ) {
            SessionPreparationForAGroupStart::FullResetForNewParameterSets => {
                tracing::info!(
                    "{}: the producer's parameter sets changed — reconfiguring the session \
                     from this keyframe",
                    Identity::PROCESSOR_NAME
                );
                session.reset_for_new_parameter_sets();
            }
            SessionPreparationForAGroupStart::ParserFlushAfterALoss => {
                session.discard_in_flight_state_after_a_gap();
            }
            SessionPreparationForAGroupStart::Nothing => {}
        }
        if !arriving_parameter_sets.is_empty() {
            self.session_parameter_sets = Some(arriving_parameter_sets);
        }

        if disposition == MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart {
            self.report_the_stream_re_entry_if_worth_it();
        }
        Ok(())
    }

    fn report_the_stream_re_entry_if_worth_it(&mut self) {
        let stream_entries = self.group_loss_recovery_gate.group_starts_entered_at();
        let worth_reporting = self
            .stream_re_entry_report_schedule
            .as_mut()
            .is_some_and(|schedule| schedule.count_is_worth_reporting(stream_entries));
        if worth_reporting {
            tracing::info!(
                stream_entries,
                frames_lost_on_the_link = self.group_loss_recovery_gate.messages_lost_on_the_link(),
                frames_discarded_awaiting_a_group_start = self
                    .group_loss_recovery_gate
                    .messages_discarded_awaiting_a_group_start(),
                "{}: entering the stream at a group start",
                Identity::PROCESSOR_NAME
            );
        }
    }

    /// Wrap one decoded picture as the video-frame bag that publishes its
    /// pooled pixel buffer.
    fn decoded_frame_awaiting_publication(
        &mut self,
        decoded_picture: DecodedVideoPictureInPooledPixelBuffer,
        color_info: Option<ColorInfo>,
        frame_header_timestamp_ns: i64,
    ) -> DecodedFrameAwaitingPublication {
        let width = decoded_picture.width;
        let height = decoded_picture.height;
        let frame = VideoFrame {
            surface_id: decoded_picture.published_pixel_buffer_frame_id.to_string(),
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
        DecodedFrameAwaitingPublication {
            frame,
            _pooled_pixel_buffer_held_until_written: decoded_picture.pixel_buffer,
        }
    }
}

/// What the session owes the group start about to be fed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPreparationForAGroupStart {
    /// New parameter sets: the session reconfigures from this keyframe.
    FullResetForNewParameterSets,
    /// Re-entry after a loss with the same parameter sets: the parser drops
    /// what the broken stream left.
    ParserFlushAfterALoss,
    /// An unbroken stream, or the first entry into a fresh session.
    Nothing,
}

/// Parameter sets other than the ones the session was fed mean the producer
/// renegotiated; a keyframe carrying none says nothing about them.
fn session_preparation_for_a_group_start(
    session_parameter_sets: Option<&ParameterSetsFromAnnexBAccessUnit>,
    arriving_parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
    disposition: MessageGroupLossRecoveryDisposition,
) -> SessionPreparationForAGroupStart {
    let Some(session_parameter_sets) = session_parameter_sets else {
        return SessionPreparationForAGroupStart::Nothing;
    };
    if !arriving_parameter_sets.is_empty() && session_parameter_sets != arriving_parameter_sets {
        SessionPreparationForAGroupStart::FullResetForNewParameterSets
    } else if disposition == MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart {
        SessionPreparationForAGroupStart::ParserFlushAfterALoss
    } else {
        SessionPreparationForAGroupStart::Nothing
    }
}

/// Resolve the DPB allocation caps from config, `None` meaning auto-detect
/// from the first SPS. A half-specified pair caps nothing a DPB can be sized
/// from, so it warns and auto-detects rather than allocating against one axis.
fn resolve_decoded_picture_buffer_dimension_caps(
    processor_name: &'static str,
    config: &HardwareVideoDecoderConfig,
) -> Option<VideoDecodeMaximumCodedExtent> {
    match (config.max_width, config.max_height) {
        (Some(max_coded_width), Some(max_coded_height)) => Some(VideoDecodeMaximumCodedExtent {
            max_coded_width,
            max_coded_height,
        }),
        (None, None) => None,
        (max_width, max_height) => {
            tracing::warn!(
                ?max_width,
                ?max_height,
                "{processor_name}: max_width and max_height cap the DPB together or not at \
                 all; auto-detecting both from the first SPS"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_config_auto_detects_both_dimensions_from_the_first_sps() {
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps(
                "H265Decoder",
                &HardwareVideoDecoderConfig::default()
            ),
            None
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
            Some(VideoDecodeMaximumCodedExtent {
                max_coded_width: 1920,
                max_coded_height: 1080,
            })
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
            None
        );
        assert_eq!(
            resolve_decoded_picture_buffer_dimension_caps("H265Decoder", &height_only),
            None
        );
    }

    /// `stream_builder.add(H265Decoder)` with no config at all must deserialize.
    #[test]
    fn an_all_absent_decoder_config_deserializes_to_defaults() {
        let config: HardwareVideoDecoderConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config, HardwareVideoDecoderConfig::default());
    }

    fn parameter_sets_with_sequence_parameter_set(
        sequence_parameter_set: &[u8],
    ) -> ParameterSetsFromAnnexBAccessUnit {
        ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: Vec::new(),
            sequence_parameter_set_nal_units: vec![sequence_parameter_set.to_vec()],
            picture_parameter_set_nal_units: vec![vec![0x68, 0xEE]],
        }
    }

    #[test]
    fn a_keyframe_with_new_parameter_sets_resets_the_session_in_full() {
        let fed = parameter_sets_with_sequence_parameter_set(&[0x67, 0x64, 0x00, 0x1F]);
        let renegotiated = parameter_sets_with_sequence_parameter_set(&[0x67, 0x64, 0x00, 0x28]);
        for disposition in [
            MessageGroupLossRecoveryDisposition::Read,
            MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart,
        ] {
            assert_eq!(
                session_preparation_for_a_group_start(Some(&fed), &renegotiated, disposition),
                SessionPreparationForAGroupStart::FullResetForNewParameterSets
            );
        }
    }

    /// The full reset waits the whole device idle, so a re-entry whose
    /// keyframe carries the same parameter sets owes only the parser flush.
    #[test]
    fn a_re_entry_with_the_same_parameter_sets_flushes_only_the_parser() {
        let fed = parameter_sets_with_sequence_parameter_set(&[0x67, 0x64, 0x00, 0x1F]);
        assert_eq!(
            session_preparation_for_a_group_start(
                Some(&fed),
                &fed.clone(),
                MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart
            ),
            SessionPreparationForAGroupStart::ParserFlushAfterALoss
        );
        assert_eq!(
            session_preparation_for_a_group_start(
                Some(&fed),
                &fed.clone(),
                MessageGroupLossRecoveryDisposition::Read
            ),
            SessionPreparationForAGroupStart::Nothing
        );
    }

    #[test]
    fn a_keyframe_carrying_no_parameter_sets_never_resets_the_session() {
        let fed = parameter_sets_with_sequence_parameter_set(&[0x67, 0x64, 0x00, 0x1F]);
        assert_eq!(
            session_preparation_for_a_group_start(
                Some(&fed),
                &ParameterSetsFromAnnexBAccessUnit::default(),
                MessageGroupLossRecoveryDisposition::Read
            ),
            SessionPreparationForAGroupStart::Nothing
        );
    }

    #[test]
    fn the_first_entry_into_a_fresh_session_owes_it_nothing() {
        assert_eq!(
            session_preparation_for_a_group_start(
                None,
                &parameter_sets_with_sequence_parameter_set(&[0x67, 0x64, 0x00, 0x1F]),
                MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart
            ),
            SessionPreparationForAGroupStart::Nothing
        );
    }
}
