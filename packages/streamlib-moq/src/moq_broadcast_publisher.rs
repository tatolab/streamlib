// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One encoded bag in, one MoQ object out, in whichever container the caller
//! chose.
//!
//! The container decides what a track is called, what a subscriber must be told
//! before media flows, and when it can be told — but not when a group is cut.
//! That is one rule for both: a video sync point cuts every track at once,
//! which is what makes a MoQ group a GOP across audio and video. Cutting on an
//! audio sync point instead would cut on every Opus packet, and the transport
//! retains only a track's latest subgroup, so all but the newest packet would
//! become unreachable the moment the next one arrived.
//!
//! The transport is reached only through the instruction list this module
//! plans, so everything up to the byte handed to a QUIC stream is decided
//! without a relay.

use std::collections::HashMap;

use bytes::Bytes;

use crate::annex_b_access_unit::{
    AnnexBNalHeaderGrammar, ParameterSetsFromAnnexBAccessUnit, length_prefix_annex_b_access_unit,
};
use crate::cmaf_fragment::build_cmaf_fragment;
use crate::cmaf_init_segment::{CmafTrackDescriptionForTheInitSegment, build_cmaf_init_segment};
use crate::cmaf_sample_entry::{
    CmafTrackSampleEntry, build_opus_sample_entry, build_video_sample_entry,
};
use crate::cmaf_track_timeline::{
    CmafTrackTimeline, OPUS_TRACK_TIMESCALE_HZ, VIDEO_TRACK_TIMESCALE_HZ,
};
use crate::encoded_media_sample::{EncodedMediaSample, TrackMedium};
use crate::error::{MoqExtensionError, Result};
use crate::moq_broadcast_catalog::{
    CATALOG_TRACK_NAME, CMAF_PACKAGING, INIT_TRACK_NAME, MoqBroadcastCatalog,
    MoqCatalogTrackDescription, MoqCatalogTrackSelectionParameters, STREAMLIB_BAG_PACKAGING,
    media_track_name,
};
use crate::moq_relay_config::MoqRelayConfig;
use crate::moq_session::MoqBroadcastPublishingSession;
use crate::streamlib_bag_object::encode_object;

/// The container name a caller passes for fragmented MP4.
const CMAF_CONTAINER_WIRE_NAME: &str = "cmaf";

/// The container name a caller passes for this wheel's own packaging. Spelled
/// with an underscore because it is a Python-facing argument value; the
/// catalog's spelling of the same container is [`STREAMLIB_BAG_PACKAGING`],
/// which is hyphenated.
const STREAMLIB_BAG_CONTAINER_WIRE_NAME: &str = "streamlib_bag";

/// The RFC 6381 string a `streamlib_bag` track is named by in the catalog.
///
/// The catalog is one document covering every track and is written once, when
/// the session connects, before any bag has arrived — so no track's real codec
/// is known when it goes out. It never becomes wrong: every `streamlib_bag`
/// object states its own codec, and that is where a subscriber reads it. A
/// player that does not know this packaging skips the track before it reaches
/// this field.
const STREAMLIB_BAG_CATALOG_CODEC_STRING: &str = STREAMLIB_BAG_PACKAGING;

/// The most encoded media a CMAF broadcast holds while a declared track has
/// still not delivered its first bag.
///
/// The hold exists because ISO/IEC 14496-12 §6.1.2 puts every track's sample
/// entry in the one `moov`, written once — so a track that has not spoken stops
/// all of them. It is bounded because the only way that lasts is a link nothing
/// ever writes to, and an unbounded hold turns a miswired graph into the
/// helper's whole address space. Roughly two seconds of a 250 Mb/s intra-only
/// stream, which is well past any cadence a real encoder opens with.
const HIGHEST_BYTES_HELD_WHILE_A_DECLARED_CMAF_TRACK_HAS_NOT_SPOKEN: usize = 64 * 1024 * 1024;

/// Which container this broadcast's objects are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoqContainerFormat {
    Cmaf,
    StreamlibBag,
}

impl MoqContainerFormat {
    /// The container a caller named, or a refusal listing the ones there are.
    pub(crate) fn of_wire_name(wire_name: &str) -> Result<Self> {
        match wire_name {
            CMAF_CONTAINER_WIRE_NAME => Ok(MoqContainerFormat::Cmaf),
            STREAMLIB_BAG_CONTAINER_WIRE_NAME => Ok(MoqContainerFormat::StreamlibBag),
            unknown => Err(MoqExtensionError::Refused {
                what: format!(
                    "`{unknown}` is not a container this wheel publishes; it writes \
                     `{CMAF_CONTAINER_WIRE_NAME}` and `{STREAMLIB_BAG_CONTAINER_WIRE_NAME}`"
                ),
            }),
        }
    }

    /// How the broadcast catalog spells this container.
    pub(crate) fn catalog_packaging(self) -> &'static str {
        match self {
            MoqContainerFormat::Cmaf => CMAF_PACKAGING,
            MoqContainerFormat::StreamlibBag => STREAMLIB_BAG_PACKAGING,
        }
    }
}

/// Publishes encoded bags to one MoQ broadcast, one object per bag.
pub(crate) struct MoqBroadcastPublisher {
    relay_config: MoqRelayConfig,
    object_write_planner: MoqBroadcastObjectWritePlanner,
    publishing_session: Option<MoqBroadcastPublishingSession>,
}

impl MoqBroadcastPublisher {
    /// A publisher that has declared no tracks and holds no connection.
    pub(crate) fn new(relay_config: MoqRelayConfig, container_format: MoqContainerFormat) -> Self {
        let broadcast_namespace = relay_config.broadcast_path.clone();
        Self {
            relay_config,
            object_write_planner: MoqBroadcastObjectWritePlanner::of(
                container_format,
                broadcast_namespace,
            ),
            publishing_session: None,
        }
    }

    /// Fix this broadcast's tracks and their order. Track ids are 1-based in
    /// the order given, and that order reaches the catalog unchanged.
    pub(crate) fn declare_tracks(&mut self, inbound_link_names: Vec<String>) -> Result<()> {
        self.object_write_planner.declare_tracks(inbound_link_names)
    }

    /// Publish one encoded bag, connecting the session if this is the first
    /// object the broadcast has to write.
    pub(crate) async fn publish(
        &mut self,
        inbound_link_name: &str,
        sample: EncodedMediaSample,
    ) -> Result<()> {
        let instructions = self
            .object_write_planner
            .plan_the_writes_for(inbound_link_name, sample)?;
        if instructions.is_empty() {
            return Ok(());
        }

        // Connected here rather than in `declare_tracks`: a relay round trip
        // inside `setup()` spends the helper's start-up budget before the graph
        // is running.
        let session = match self.publishing_session.as_mut() {
            Some(already_connected) => already_connected,
            None => {
                let moq_track_names = self.object_write_planner.moq_track_names();
                let connected = MoqBroadcastPublishingSession::connect(
                    self.relay_config.clone(),
                    moq_track_names,
                )
                .await?;
                self.publishing_session.insert(connected)
            }
        };

        for instruction in instructions {
            match instruction {
                MoqObjectWriteInstruction::CutANewGroupOnEveryTrack => {
                    session.open_a_new_group_on_every_track();
                }
                MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                    moq_track_name,
                    object_payload,
                } => session.write_the_only_object_of(&moq_track_name, object_payload)?,
                MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                    moq_track_name,
                    object_payload,
                } => session.write_object(&moq_track_name, object_payload)?,
            }
        }
        Ok(())
    }

    /// Whether the relay session is up.
    pub(crate) fn is_connected(&self) -> bool {
        self.publishing_session.is_some()
    }

    /// Finish every open group and end the session.
    pub(crate) async fn close(&mut self) {
        if let Some(session) = self.publishing_session.take() {
            session.close();
        }
    }
}

/// One thing the transport is asked to do, in the order it must happen.
///
/// Planning the whole ordered list before any of it executes is what lets the
/// init object be proven to precede the catalog, and a group cut to precede the
/// sync point that caused it, with no relay in reach.
#[derive(Debug, Clone, PartialEq)]
enum MoqObjectWriteInstruction {
    CutANewGroupOnEveryTrack,
    WriteTheOnlyObjectATrackEverCarries {
        moq_track_name: String,
        object_payload: Bytes,
    },
    AppendOneObjectToATracksOpenGroup {
        moq_track_name: String,
        object_payload: Bytes,
    },
}

/// Everything about a broadcast that needs no connection: its tracks, their
/// names, their descriptions, and what each bag turns into.
struct MoqBroadcastObjectWritePlanner {
    container_format: MoqContainerFormat,
    broadcast_namespace: String,
    /// Declaration order, which is also track-id order and catalog order.
    declared_tracks: Vec<DeclaredMoqTrackPublicationState>,
    declared_track_index_by_inbound_link_name: HashMap<String, usize>,
    /// Whether the catalog — and, on CMAF, the init object — have been planned.
    the_descriptive_objects_have_been_planned: bool,
    samples_held_until_every_cmaf_track_is_described:
        Vec<HeldSampleAwaitingEveryCmafTrackDescription>,
    bytes_held_until_every_cmaf_track_is_described: usize,
}

impl MoqBroadcastObjectWritePlanner {
    fn of(container_format: MoqContainerFormat, broadcast_namespace: String) -> Self {
        Self {
            container_format,
            broadcast_namespace,
            declared_tracks: Vec::new(),
            declared_track_index_by_inbound_link_name: HashMap::new(),
            the_descriptive_objects_have_been_planned: false,
            samples_held_until_every_cmaf_track_is_described: Vec::new(),
            bytes_held_until_every_cmaf_track_is_described: 0,
        }
    }

    fn declare_tracks(&mut self, inbound_link_names: Vec<String>) -> Result<()> {
        if !self.declared_tracks.is_empty() {
            return Err(MoqExtensionError::Refused {
                what: format!(
                    "this broadcast's tracks are already declared as {}; a MoQ broadcast's tracks \
                     are created once, when its session connects, and a second declaration would \
                     name tracks no subscriber can reach",
                    self.describe_inbound_link_names()
                ),
            });
        }
        if inbound_link_names.is_empty() {
            return Err(MoqExtensionError::Refused {
                what: "a MoQ broadcast declaring no tracks publishes nothing; declare at least \
                       one inbound link"
                    .to_owned(),
            });
        }

        for (declared_track_index, inbound_link_name) in inbound_link_names.iter().enumerate() {
            if self
                .declared_track_index_by_inbound_link_name
                .contains_key(inbound_link_name)
            {
                return Err(MoqExtensionError::Refused {
                    what: format!(
                        "`{inbound_link_name}` is declared twice; each inbound link is one MoQ \
                         track, and two tracks of one broadcast cannot share a name"
                    ),
                });
            }
            let moq_media_track_name = match self.container_format {
                // `.catalog`, `0.mp4` and `{track_id}.m4s` are a fallback
                // contract, not this wheel's to vary: the reference subscriber
                // hardcodes all three when it is not asked to fetch a catalog.
                MoqContainerFormat::Cmaf => media_track_name(declared_track_index as u32 + 1),
                MoqContainerFormat::StreamlibBag => {
                    if inbound_link_name == CATALOG_TRACK_NAME {
                        return Err(MoqExtensionError::Refused {
                            what: format!(
                                "an inbound link named `{CATALOG_TRACK_NAME}` cannot be published \
                                 over `{STREAMLIB_BAG_CONTAINER_WIRE_NAME}`: this container names \
                                 each media track after its link, and that name is already this \
                                 broadcast's catalog track"
                            ),
                        });
                    }
                    inbound_link_name.clone()
                }
            };
            self.declared_track_index_by_inbound_link_name
                .insert(inbound_link_name.clone(), declared_track_index);
            self.declared_tracks
                .push(DeclaredMoqTrackPublicationState::of(
                    inbound_link_name.clone(),
                    declared_track_index as u32 + 1,
                    moq_media_track_name,
                ));
        }
        Ok(())
    }

    /// Every MoQ track name the session must create before it announces the
    /// namespace, the descriptive tracks included.
    fn moq_track_names(&self) -> Vec<String> {
        let mut moq_track_names = vec![CATALOG_TRACK_NAME.to_owned()];
        if self.container_format == MoqContainerFormat::Cmaf {
            moq_track_names.push(INIT_TRACK_NAME.to_owned());
        }
        moq_track_names.extend(
            self.declared_tracks
                .iter()
                .map(|track| track.moq_media_track_name.clone()),
        );
        moq_track_names
    }

    fn plan_the_writes_for(
        &mut self,
        inbound_link_name: &str,
        sample: EncodedMediaSample,
    ) -> Result<Vec<MoqObjectWriteInstruction>> {
        let declared_track_index = self.declared_track_index_of(inbound_link_name)?;
        self.declared_tracks[declared_track_index]
            .refuse_a_sample_unlike_the_one_this_track_was_first_published_from(&sample)?;

        match self.container_format {
            MoqContainerFormat::StreamlibBag => {
                self.plan_a_streamlib_bag_write(declared_track_index, &sample)
            }
            MoqContainerFormat::Cmaf => self.plan_a_cmaf_write(declared_track_index, sample),
        }
    }

    fn plan_a_streamlib_bag_write(
        &mut self,
        declared_track_index: usize,
        sample: &EncodedMediaSample,
    ) -> Result<Vec<MoqObjectWriteInstruction>> {
        let object_payload = encode_object(sample)?;
        let mut instructions = Vec::new();
        if !self.the_descriptive_objects_have_been_planned {
            instructions.push(
                MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                    moq_track_name: CATALOG_TRACK_NAME.to_owned(),
                    object_payload: self.build_the_broadcast_catalog()?,
                },
            );
            self.the_descriptive_objects_have_been_planned = true;
        }
        if this_sample_cuts_a_new_group_on_every_track(sample) {
            instructions.push(MoqObjectWriteInstruction::CutANewGroupOnEveryTrack);
        }
        instructions.push(
            MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                moq_track_name: self.declared_tracks[declared_track_index]
                    .moq_media_track_name
                    .clone(),
                object_payload,
            },
        );
        Ok(instructions)
    }

    fn plan_a_cmaf_write(
        &mut self,
        declared_track_index: usize,
        sample: EncodedMediaSample,
    ) -> Result<Vec<MoqObjectWriteInstruction>> {
        self.declared_tracks[declared_track_index].learn_its_cmaf_description_from(&sample)?;

        if self.the_descriptive_objects_have_been_planned {
            return self.plan_one_cmaf_media_object(declared_track_index, &sample);
        }
        if !self.every_declared_track_is_described() {
            self.hold_until_every_declared_track_is_described(declared_track_index, sample)?;
            return Ok(Vec::new());
        }

        let init_object_payload = self.build_the_cmaf_init_segment()?;
        let catalog_object_payload = self.build_the_broadcast_catalog()?;
        // Init first, catalog second: a subscriber that reads the catalog and
        // then follows `initTrack` must find the init object already there.
        let mut instructions = vec![
            MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                moq_track_name: INIT_TRACK_NAME.to_owned(),
                object_payload: init_object_payload,
            },
            MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                moq_track_name: CATALOG_TRACK_NAME.to_owned(),
                object_payload: catalog_object_payload,
            },
        ];
        self.the_descriptive_objects_have_been_planned = true;
        tracing::info!(
            broadcast = %self.broadcast_namespace,
            tracks = self.declared_tracks.len(),
            held_samples = self.samples_held_until_every_cmaf_track_is_described.len(),
            "every CMAF track is described; this broadcast is playable from here"
        );

        let held = std::mem::take(&mut self.samples_held_until_every_cmaf_track_is_described);
        self.bytes_held_until_every_cmaf_track_is_described = 0;
        for held_sample in held {
            instructions.extend(self.plan_one_cmaf_media_object(
                held_sample.declared_track_index,
                &held_sample.sample,
            )?);
        }
        instructions.extend(self.plan_one_cmaf_media_object(declared_track_index, &sample)?);
        Ok(instructions)
    }

    fn plan_one_cmaf_media_object(
        &mut self,
        declared_track_index: usize,
        sample: &EncodedMediaSample,
    ) -> Result<Vec<MoqObjectWriteInstruction>> {
        let track = &mut self.declared_tracks[declared_track_index];
        let Some(description) = track.cmaf_description.as_mut() else {
            return Err(MoqExtensionError::Refused {
                what: format!(
                    "`{}` has no CMAF track description, so nothing states the timescale its \
                     samples are placed on",
                    track.inbound_link_name
                ),
            });
        };

        let sample_bytes = match sample {
            EncodedMediaSample::VideoAccessUnit(unit) => {
                let grammar = nal_header_grammar_of(&unit.codec, &track.inbound_link_name)?;
                let length_prefixed =
                    length_prefix_annex_b_access_unit(&unit.annex_b_access_unit, grammar);
                if !parameter_sets_are_empty(&length_prefixed.parameter_sets)
                    && length_prefixed.parameter_sets
                        != track.parameter_sets_the_init_segment_states
                {
                    return Err(MoqExtensionError::Refused {
                        what: format!(
                            "`{}` delivered parameter sets different from the ones its init \
                             segment states, and a CMAF init segment is written once — ISO/IEC \
                             14496-12 §6.1.2 puts the sample entry in the one `moov` — so this \
                             stream cannot be re-described, and publishing on would misdescribe \
                             every sample after it",
                            track.inbound_link_name
                        ),
                    });
                }
                // The parameter sets are in the init segment's sample entry,
                // which is where a CMAF reader looks; carrying them again here
                // would hand a decoder bytes it configured from already.
                length_prefixed.length_prefixed_sample_bytes
            }
            EncodedMediaSample::AudioPacket(packet) => packet.opus_packet.to_vec(),
        };

        let placement = description.cmaf_track_timeline.place(sample.timestamp_ns());
        let sequence_number = track.next_cmaf_fragment_sequence_number;
        track.next_cmaf_fragment_sequence_number =
            track.next_cmaf_fragment_sequence_number.saturating_add(1);
        // The track's own `tkhd.track_id`, never a constant: a fragment names
        // the track it belongs to, and on a two-track broadcast a fixed id
        // would route the audio track's samples onto the video track's
        // timeline — or be rejected outright.
        let object_payload = build_cmaf_fragment(
            track.cmaf_track_id,
            sequence_number,
            placement.decode_time,
            placement.duration,
            sample.is_sync_point(),
            &sample_bytes,
        )?;

        let mut instructions = Vec::new();
        if this_sample_cuts_a_new_group_on_every_track(sample) {
            instructions.push(MoqObjectWriteInstruction::CutANewGroupOnEveryTrack);
        }
        instructions.push(
            MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                moq_track_name: track.moq_media_track_name.clone(),
                object_payload,
            },
        );
        Ok(instructions)
    }

    fn hold_until_every_declared_track_is_described(
        &mut self,
        declared_track_index: usize,
        sample: EncodedMediaSample,
    ) -> Result<()> {
        let sample_byte_count = encoded_byte_count_of(&sample);
        if self
            .bytes_held_until_every_cmaf_track_is_described
            .saturating_add(sample_byte_count)
            > HIGHEST_BYTES_HELD_WHILE_A_DECLARED_CMAF_TRACK_HAS_NOT_SPOKEN
        {
            return Err(MoqExtensionError::Refused {
                what: format!(
                    "{} bytes of encoded media are held because {} has not delivered a first \
                     bag; this broadcast's CMAF init segment describes every track in one `moov` \
                     written once, so no track can publish until every one of them has spoken, \
                     and the hold stops at \
                     {HIGHEST_BYTES_HELD_WHILE_A_DECLARED_CMAF_TRACK_HAS_NOT_SPOKEN} bytes",
                    self.bytes_held_until_every_cmaf_track_is_described,
                    self.describe_the_tracks_that_have_not_spoken()
                ),
            });
        }
        self.bytes_held_until_every_cmaf_track_is_described += sample_byte_count;
        self.samples_held_until_every_cmaf_track_is_described.push(
            HeldSampleAwaitingEveryCmafTrackDescription {
                declared_track_index,
                sample,
            },
        );
        Ok(())
    }

    fn every_declared_track_is_described(&self) -> bool {
        self.declared_tracks
            .iter()
            .all(|track| track.cmaf_description.is_some())
    }

    fn build_the_cmaf_init_segment(&self) -> Result<Bytes> {
        let mut init_segment_track_descriptions = Vec::with_capacity(self.declared_tracks.len());
        for track in &self.declared_tracks {
            let Some(description) = track.cmaf_description.as_ref() else {
                return Err(MoqExtensionError::Refused {
                    what: format!(
                        "`{}` has not been described, so the init segment cannot state its \
                         sample entry",
                        track.inbound_link_name
                    ),
                });
            };
            init_segment_track_descriptions.push(CmafTrackDescriptionForTheInitSegment {
                track_id: track.cmaf_track_id,
                inbound_link_name: track.inbound_link_name.clone(),
                cmaf_track_sample_entry: description.cmaf_track_sample_entry.clone(),
                media_timescale_hz: description.cmaf_track_timeline.timescale_hz(),
                coded_extent: description.coded_extent,
            });
        }
        build_cmaf_init_segment(&init_segment_track_descriptions)
    }

    fn build_the_broadcast_catalog(&self) -> Result<Bytes> {
        let catalog = match self.container_format {
            MoqContainerFormat::Cmaf => {
                let mut catalog_track_descriptions = Vec::with_capacity(self.declared_tracks.len());
                for track in &self.declared_tracks {
                    let Some(description) = track.cmaf_description.as_ref() else {
                        return Err(MoqExtensionError::Refused {
                            what: format!(
                                "`{}` has not been described, so the catalog cannot name its \
                                 codec",
                                track.inbound_link_name
                            ),
                        });
                    };
                    catalog_track_descriptions.push(MoqCatalogTrackDescription::of_cmaf_track_id(
                        track.cmaf_track_id,
                        description.catalog_selection_parameters.clone(),
                    ));
                }
                MoqBroadcastCatalog::of_cmaf_tracks(
                    self.broadcast_namespace.clone(),
                    catalog_track_descriptions,
                )
            }
            MoqContainerFormat::StreamlibBag => MoqBroadcastCatalog::of_streamlib_bag_tracks(
                self.broadcast_namespace.clone(),
                self.declared_tracks
                    .iter()
                    .map(|track| {
                        MoqCatalogTrackDescription::of_self_describing_track(
                            track.moq_media_track_name.clone(),
                            MoqCatalogTrackSelectionParameters::of_video_track(
                                STREAMLIB_BAG_CATALOG_CODEC_STRING,
                                0,
                                0,
                            ),
                        )
                    })
                    .collect(),
            ),
        };
        catalog.catalog_json_bytes()
    }

    fn declared_track_index_of(&self, inbound_link_name: &str) -> Result<usize> {
        self.declared_track_index_by_inbound_link_name
            .get(inbound_link_name)
            .copied()
            .ok_or_else(|| MoqExtensionError::Refused {
                what: format!(
                    "`{inbound_link_name}` is not an inbound link of this broadcast; it carries {}",
                    self.describe_inbound_link_names()
                ),
            })
    }

    fn describe_inbound_link_names(&self) -> String {
        if self.declared_tracks.is_empty() {
            return "no inbound links at all".to_owned();
        }
        self.declared_tracks
            .iter()
            .map(|track| format!("`{}`", track.inbound_link_name))
            .collect::<Vec<String>>()
            .join(", ")
    }

    fn describe_the_tracks_that_have_not_spoken(&self) -> String {
        let silent: Vec<String> = self
            .declared_tracks
            .iter()
            .filter(|track| track.cmaf_description.is_none())
            .map(|track| format!("`{}`", track.inbound_link_name))
            .collect();
        if silent.is_empty() {
            "no track".to_owned()
        } else {
            silent.join(", ")
        }
    }
}

/// One bag kept aside while a sibling track has not yet become describable.
struct HeldSampleAwaitingEveryCmafTrackDescription {
    declared_track_index: usize,
    sample: EncodedMediaSample,
}

/// What the init segment and the catalog say about one track, and the clock its
/// fragments are placed on.
struct CmafTrackDescriptionLearnedFromItsFirstUsableSample {
    cmaf_track_sample_entry: CmafTrackSampleEntry,
    cmaf_track_timeline: CmafTrackTimeline,
    coded_extent: Option<(u32, u32)>,
    catalog_selection_parameters: MoqCatalogTrackSelectionParameters,
}

/// Everything one declared track accumulates as bags arrive on it.
struct DeclaredMoqTrackPublicationState {
    inbound_link_name: String,
    /// The `tkhd.track_id` this track takes in the init segment's `moov`, which
    /// is also what names its MoQ media track.
    cmaf_track_id: u32,
    moq_media_track_name: String,
    first_published_codec: Option<String>,
    first_published_medium: Option<TrackMedium>,
    cmaf_description: Option<CmafTrackDescriptionLearnedFromItsFirstUsableSample>,
    parameter_sets_the_init_segment_states: ParameterSetsFromAnnexBAccessUnit,
    next_cmaf_fragment_sequence_number: u32,
}

impl DeclaredMoqTrackPublicationState {
    fn of(inbound_link_name: String, cmaf_track_id: u32, moq_media_track_name: String) -> Self {
        Self {
            inbound_link_name,
            cmaf_track_id,
            moq_media_track_name,
            first_published_codec: None,
            first_published_medium: None,
            cmaf_description: None,
            parameter_sets_the_init_segment_states: ParameterSetsFromAnnexBAccessUnit::default(),
            // ISO/IEC 14496-12 §8.8.5: `mfhd.sequence_number` counts one
            // track's fragments from one.
            next_cmaf_fragment_sequence_number: 1,
        }
    }

    fn refuse_a_sample_unlike_the_one_this_track_was_first_published_from(
        &mut self,
        sample: &EncodedMediaSample,
    ) -> Result<()> {
        let medium = sample.medium();
        let codec = wire_codec_of(sample);
        match (
            self.first_published_codec.as_deref(),
            self.first_published_medium,
        ) {
            (Some(first_codec), Some(first_medium)) => {
                if first_medium != medium || first_codec != codec {
                    return Err(MoqExtensionError::Refused {
                        what: format!(
                            "`{}` first published {} `{first_codec}` and this bag carries {} \
                             `{codec}`; a MoQ track's medium and codec are stated once, by the \
                             catalog and the init segment, and neither can be revised",
                            self.inbound_link_name,
                            first_medium.as_str(),
                            medium.as_str()
                        ),
                    });
                }
            }
            _ => {
                self.first_published_codec = Some(codec.to_owned());
                self.first_published_medium = Some(medium);
            }
        }
        Ok(())
    }

    /// Describe this track if this bag is the first one that can: a video track
    /// from its first sync point, whose prepended parameter sets are what a
    /// sample entry is built from, and an audio track from its first packet.
    fn learn_its_cmaf_description_from(&mut self, sample: &EncodedMediaSample) -> Result<()> {
        if self.cmaf_description.is_some() {
            return Ok(());
        }
        match sample {
            EncodedMediaSample::VideoAccessUnit(unit) => {
                if !unit.is_sync_point {
                    return Ok(());
                }
                let grammar = nal_header_grammar_of(&unit.codec, &self.inbound_link_name)?;
                let length_prefixed =
                    length_prefix_annex_b_access_unit(&unit.annex_b_access_unit, grammar);
                let sample_entry = build_video_sample_entry(
                    &unit.codec,
                    &length_prefixed.parameter_sets,
                    unit.width,
                    unit.height,
                )?;
                self.parameter_sets_the_init_segment_states = length_prefixed.parameter_sets;
                self.cmaf_description = Some(CmafTrackDescriptionLearnedFromItsFirstUsableSample {
                    cmaf_track_sample_entry: sample_entry.cmaf_track_sample_entry,
                    cmaf_track_timeline: CmafTrackTimeline::on(VIDEO_TRACK_TIMESCALE_HZ),
                    coded_extent: Some((unit.width, unit.height)),
                    catalog_selection_parameters:
                        MoqCatalogTrackSelectionParameters::of_video_track(
                            sample_entry.rfc6381_codec_string,
                            unit.width,
                            unit.height,
                        ),
                });
            }
            EncodedMediaSample::AudioPacket(packet) => {
                let sample_entry =
                    build_opus_sample_entry(packet.channels, packet.sample_rate, packet.pre_skip)?;
                self.cmaf_description = Some(CmafTrackDescriptionLearnedFromItsFirstUsableSample {
                    cmaf_track_sample_entry: sample_entry.cmaf_track_sample_entry,
                    cmaf_track_timeline: CmafTrackTimeline::on(OPUS_TRACK_TIMESCALE_HZ),
                    coded_extent: None,
                    catalog_selection_parameters:
                        MoqCatalogTrackSelectionParameters::of_audio_track(
                            sample_entry.rfc6381_codec_string,
                            packet.sample_rate,
                            packet.channels,
                        ),
                });
            }
        }
        Ok(())
    }
}

/// The one group-cadence rule both containers obey.
///
/// A group is a GOP across every track at once, so the cut is a video sync
/// point and nothing else. Every Opus packet is a sync point, and the transport
/// retains only a track's latest subgroup, so cutting on audio would leave one
/// packet per group and lose all but the newest.
fn this_sample_cuts_a_new_group_on_every_track(sample: &EncodedMediaSample) -> bool {
    match sample {
        EncodedMediaSample::VideoAccessUnit(unit) => unit.is_sync_point,
        EncodedMediaSample::AudioPacket(_) => false,
    }
}

fn wire_codec_of(sample: &EncodedMediaSample) -> &str {
    match sample {
        EncodedMediaSample::VideoAccessUnit(unit) => &unit.codec,
        EncodedMediaSample::AudioPacket(packet) => &packet.codec,
    }
}

fn encoded_byte_count_of(sample: &EncodedMediaSample) -> usize {
    match sample {
        EncodedMediaSample::VideoAccessUnit(unit) => unit.annex_b_access_unit.len(),
        EncodedMediaSample::AudioPacket(packet) => packet.opus_packet.len(),
    }
}

fn parameter_sets_are_empty(parameter_sets: &ParameterSetsFromAnnexBAccessUnit) -> bool {
    parameter_sets.video_parameter_set_nal_units.is_empty()
        && parameter_sets.sequence_parameter_set_nal_units.is_empty()
        && parameter_sets.picture_parameter_set_nal_units.is_empty()
}

fn nal_header_grammar_of(codec: &str, inbound_link_name: &str) -> Result<AnnexBNalHeaderGrammar> {
    AnnexBNalHeaderGrammar::of_wire_codec(codec).ok_or_else(|| MoqExtensionError::Refused {
        what: format!(
            "`{inbound_link_name}` carries video coded as `{codec}`, which this wheel cannot \
             length-prefix; it writes h264 and h265"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::annex_b_access_unit::ANNEX_B_START_CODE;
    use crate::cmaf_fragment::read_cmaf_fragment;
    use crate::encoded_media_sample::{EncodedAudioPacket, EncodedVideoAccessUnit};

    const BROADCAST_NAMESPACE: &str = "streamlib/a-broadcast";

    /// A constrained-baseline SPS: the NAL header, `profile_idc` 66, the
    /// constraint flags and `level_idc` 30. `avc1` reads exactly those three
    /// bytes out of it, so no further syntax is needed to describe a track.
    const AN_H264_SEQUENCE_PARAMETER_SET: [u8; 4] = [0x67, 0x42, 0xC0, 0x1E];
    /// The same SPS at level 3.1 — a different set, and so a reconfiguration.
    const A_SECOND_H264_SEQUENCE_PARAMETER_SET: [u8; 4] = [0x67, 0x42, 0xC0, 0x1F];
    const AN_H264_PICTURE_PARAMETER_SET: [u8; 4] = [0x68, 0xCE, 0x3C, 0x80];
    const AN_H264_IDR_SLICE: [u8; 6] = [0x65, 0x88, 0x84, 0x00, 0x21, 0xFF];
    const AN_H264_NON_IDR_SLICE: [u8; 5] = [0x41, 0x9A, 0x02, 0x10, 0x3B];

    fn annex_b(nal_units: &[&[u8]]) -> Bytes {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            access_unit.extend_from_slice(&ANNEX_B_START_CODE);
            access_unit.extend_from_slice(nal_unit);
        }
        Bytes::from(access_unit)
    }

    fn a_video_sync_point(timestamp_ns: i64) -> EncodedMediaSample {
        a_video_sync_point_carrying(&AN_H264_SEQUENCE_PARAMETER_SET, timestamp_ns)
    }

    fn a_video_sync_point_carrying(
        sequence_parameter_set: &[u8],
        timestamp_ns: i64,
    ) -> EncodedMediaSample {
        video_access_unit(
            annex_b(&[
                sequence_parameter_set,
                &AN_H264_PICTURE_PARAMETER_SET,
                &AN_H264_IDR_SLICE,
            ]),
            true,
            timestamp_ns,
        )
    }

    fn a_video_delta_frame(timestamp_ns: i64) -> EncodedMediaSample {
        video_access_unit(annex_b(&[&AN_H264_NON_IDR_SLICE]), false, timestamp_ns)
    }

    fn video_access_unit(
        annex_b_access_unit: Bytes,
        is_sync_point: bool,
        timestamp_ns: i64,
    ) -> EncodedMediaSample {
        EncodedMediaSample::VideoAccessUnit(EncodedVideoAccessUnit {
            codec: "h264".to_owned(),
            annex_b_access_unit,
            is_sync_point,
            group_index: 0,
            sequence_index: 0,
            width: 320,
            height: 180,
            color: None,
            timestamp_ns,
        })
    }

    fn an_opus_packet(timestamp_ns: i64) -> EncodedMediaSample {
        EncodedMediaSample::AudioPacket(EncodedAudioPacket {
            codec: "opus".to_owned(),
            opus_packet: Bytes::from_static(&[0xFC, 0xFF, 0xFE, 0x01, 0x02, 0x03]),
            is_sync_point: true,
            group_index: 0,
            sequence_index: 0,
            sample_rate: 48_000,
            channels: 2,
            sample_count: 960,
            pre_skip: 312,
            timestamp_ns,
        })
    }

    fn a_planner_over(
        container_format: MoqContainerFormat,
        inbound_link_names: &[&str],
    ) -> MoqBroadcastObjectWritePlanner {
        let mut planner =
            MoqBroadcastObjectWritePlanner::of(container_format, BROADCAST_NAMESPACE.to_owned());
        planner
            .declare_tracks(
                inbound_link_names
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
            )
            .expect("these inbound links are declarable");
        planner
    }

    /// Each instruction as `cut`, `only:<track>` or `object:<track>`, which is
    /// the whole of what reaches the transport and in what order.
    fn describe(instructions: &[MoqObjectWriteInstruction]) -> Vec<String> {
        instructions
            .iter()
            .map(|instruction| match instruction {
                MoqObjectWriteInstruction::CutANewGroupOnEveryTrack => "cut".to_owned(),
                MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                    moq_track_name,
                    ..
                } => {
                    format!("only:{moq_track_name}")
                }
                MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                    moq_track_name,
                    ..
                } => {
                    format!("object:{moq_track_name}")
                }
            })
            .collect()
    }

    fn object_payloads_written_to(
        instructions: &[MoqObjectWriteInstruction],
        wanted_track_name: &str,
    ) -> Vec<Bytes> {
        instructions
            .iter()
            .filter_map(|instruction| match instruction {
                MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                    moq_track_name,
                    object_payload,
                } if moq_track_name == wanted_track_name => Some(object_payload.clone()),
                _ => None,
            })
            .collect()
    }

    fn cmaf_fragment_sequence_number(fragment_bytes: &[u8]) -> u32 {
        use mp4_atom::{Atom, Decode, Header, Moof};
        let mut unread: &[u8] = fragment_bytes;
        let moof_header = Header::decode(&mut unread).expect("the fragment opens with a moof");
        assert_eq!(moof_header.kind, Moof::KIND);
        let moof_body_bytes = moof_header.size.expect("the moof states its size");
        let mut moof_body = &unread[..moof_body_bytes];
        Moof::decode_body(&mut moof_body)
            .expect("the moof parses")
            .mfhd
            .sequence_number
    }

    fn refusal_of(failure: MoqExtensionError) -> String {
        assert!(
            matches!(failure, MoqExtensionError::Refused { .. }),
            "the caller passed something wrong, so this is a refusal: {failure}"
        );
        failure.to_string()
    }

    #[test]
    fn a_cmaf_broadcast_carries_the_catalog_track_the_init_track_and_one_media_track_per_link() {
        let planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);

        assert_eq!(
            planner.moq_track_names(),
            vec![".catalog", "0.mp4", "1.m4s", "2.m4s"]
        );
    }

    #[test]
    fn a_streamlib_bag_broadcast_carries_the_catalog_track_and_one_track_named_after_each_link() {
        let planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["camera", "microphone"]);

        assert_eq!(
            planner.moq_track_names(),
            vec![".catalog", "camera", "microphone"]
        );
    }

    #[test]
    fn a_container_this_wheel_does_not_publish_is_refused_by_name() {
        let refusal = refusal_of(
            MoqContainerFormat::of_wire_name("loc").expect_err("`loc` is not published here"),
        );

        assert!(refusal.contains("loc"), "{refusal}");
        assert!(refusal.contains("cmaf"), "{refusal}");
        assert!(refusal.contains("streamlib_bag"), "{refusal}");
    }

    #[test]
    fn each_container_the_wheel_publishes_is_named_the_same_way_by_the_catalog_and_the_caller() {
        assert_eq!(
            MoqContainerFormat::of_wire_name("cmaf").expect("cmaf is published"),
            MoqContainerFormat::Cmaf
        );
        assert_eq!(
            MoqContainerFormat::of_wire_name("streamlib_bag").expect("streamlib_bag is published"),
            MoqContainerFormat::StreamlibBag
        );
        assert_eq!(MoqContainerFormat::Cmaf.catalog_packaging(), CMAF_PACKAGING);
        assert_eq!(
            MoqContainerFormat::StreamlibBag.catalog_packaging(),
            STREAMLIB_BAG_PACKAGING
        );
    }

    #[test]
    fn declaring_a_broadcasts_tracks_a_second_time_is_refused_and_names_the_links_already_declared()
    {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera"]);

        let refusal = refusal_of(
            planner
                .declare_tracks(vec!["microphone".to_owned()])
                .expect_err("the tracks are already fixed"),
        );

        assert!(refusal.contains("camera"), "{refusal}");
    }

    #[test]
    fn declaring_a_broadcast_carrying_no_inbound_links_at_all_is_refused() {
        let mut planner = MoqBroadcastObjectWritePlanner::of(
            MoqContainerFormat::Cmaf,
            BROADCAST_NAMESPACE.to_owned(),
        );

        let refusal = refusal_of(
            planner
                .declare_tracks(Vec::new())
                .expect_err("a broadcast with no tracks publishes nothing"),
        );

        assert!(refusal.contains("at least one"), "{refusal}");
    }

    #[test]
    fn declaring_the_same_inbound_link_twice_is_refused_by_name() {
        let mut planner = MoqBroadcastObjectWritePlanner::of(
            MoqContainerFormat::Cmaf,
            BROADCAST_NAMESPACE.to_owned(),
        );

        let refusal = refusal_of(
            planner
                .declare_tracks(vec![
                    "camera".to_owned(),
                    "microphone".to_owned(),
                    "camera".to_owned(),
                ])
                .expect_err("two tracks cannot share a name"),
        );

        assert!(refusal.contains("camera"), "{refusal}");
    }

    #[test]
    fn a_streamlib_bag_link_that_would_take_the_catalog_tracks_name_is_refused_by_name() {
        let mut planner = MoqBroadcastObjectWritePlanner::of(
            MoqContainerFormat::StreamlibBag,
            BROADCAST_NAMESPACE.to_owned(),
        );

        let refusal = refusal_of(
            planner
                .declare_tracks(vec![CATALOG_TRACK_NAME.to_owned()])
                .expect_err("that name is already the catalog track"),
        );

        assert!(refusal.contains(CATALOG_TRACK_NAME), "{refusal}");
    }

    #[test]
    fn publishing_from_a_link_this_broadcast_does_not_carry_is_refused_and_names_the_links_it_does()
    {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);

        let refusal = refusal_of(
            planner
                .plan_the_writes_for("screen", a_video_sync_point(0))
                .expect_err("`screen` is not a link of this broadcast"),
        );

        assert!(refusal.contains("screen"), "{refusal}");
        assert!(refusal.contains("camera"), "{refusal}");
        assert!(refusal.contains("microphone"), "{refusal}");
    }

    #[test]
    fn a_video_sync_point_cuts_a_new_group_on_every_track_before_it_is_written() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["camera"]);

        let first_sync_point = planner
            .plan_the_writes_for("camera", a_video_sync_point(0))
            .expect("a sync point publishes");
        let delta_frame = planner
            .plan_the_writes_for("camera", a_video_delta_frame(33_000_000))
            .expect("a delta frame publishes");
        let second_sync_point = planner
            .plan_the_writes_for("camera", a_video_sync_point(66_000_000))
            .expect("the next sync point publishes");

        assert_eq!(
            describe(&first_sync_point),
            vec!["only:.catalog", "cut", "object:camera"]
        );
        assert_eq!(describe(&delta_frame), vec!["object:camera"]);
        assert_eq!(describe(&second_sync_point), vec!["cut", "object:camera"]);
    }

    #[test]
    fn an_opus_packet_never_cuts_a_group_even_though_every_opus_packet_is_a_sync_point() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["microphone"]);

        let first = planner
            .plan_the_writes_for("microphone", an_opus_packet(0))
            .expect("an opus packet publishes");
        let second = planner
            .plan_the_writes_for("microphone", an_opus_packet(20_000_000))
            .expect("the next opus packet publishes");
        let third = planner
            .plan_the_writes_for("microphone", an_opus_packet(40_000_000))
            .expect("the next opus packet publishes");

        assert_eq!(
            describe(&first),
            vec!["only:.catalog", "object:microphone"],
            "the catalog is written once, before the first media object"
        );
        assert_eq!(describe(&second), vec!["object:microphone"]);
        assert_eq!(describe(&third), vec!["object:microphone"]);
    }

    #[test]
    fn no_cmaf_object_is_written_until_every_declared_track_has_been_described() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);

        for (frame_index, stamp_ns) in [0, 33_000_000, 66_000_000].into_iter().enumerate() {
            let planned = planner
                .plan_the_writes_for("camera", a_video_sync_point(stamp_ns))
                .expect("video publishes");
            assert!(
                planned.is_empty(),
                "video frame {frame_index} was written before the audio track was described: {:?}",
                describe(&planned)
            );
        }

        let once_the_audio_track_spoke = planner
            .plan_the_writes_for("microphone", an_opus_packet(0))
            .expect("audio publishes");

        assert!(!once_the_audio_track_spoke.is_empty());
    }

    #[test]
    fn the_cmaf_init_object_is_written_before_the_catalog() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        planner
            .plan_the_writes_for("camera", a_video_sync_point(0))
            .expect("video publishes");

        let planned = planner
            .plan_the_writes_for("microphone", an_opus_packet(0))
            .expect("audio publishes");

        assert_eq!(
            describe(&planned)[..2],
            ["only:0.mp4".to_owned(), "only:.catalog".to_owned()]
        );
    }

    #[test]
    fn the_bags_held_while_a_track_was_silent_are_published_in_arrival_order_once_it_speaks() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        for stamp_ns in [0, 33_000_000, 66_000_000] {
            let held = planner
                .plan_the_writes_for(
                    "camera",
                    if stamp_ns == 0 {
                        a_video_sync_point(stamp_ns)
                    } else {
                        a_video_delta_frame(stamp_ns)
                    },
                )
                .expect("video publishes");
            assert!(held.is_empty());
        }

        let planned = planner
            .plan_the_writes_for("microphone", an_opus_packet(0))
            .expect("audio publishes");

        assert_eq!(
            describe(&planned),
            vec![
                "only:0.mp4",
                "only:.catalog",
                "cut",
                "object:1.m4s",
                "object:1.m4s",
                "object:1.m4s",
                "object:2.m4s",
            ]
        );
        let decode_times: Vec<u64> = object_payloads_written_to(&planned, "1.m4s")
            .iter()
            .map(|fragment| {
                read_cmaf_fragment(fragment).expect("the fragment reads back")[0].decode_time
            })
            .collect();
        assert_eq!(
            decode_times,
            vec![0, 33_000_000, 66_000_000],
            "the held bags were placed on the track's timeline in arrival order"
        );
    }

    #[test]
    fn holding_for_a_track_that_never_speaks_is_bounded_and_the_refusal_names_the_silent_track() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        planner
            .plan_the_writes_for("camera", a_video_sync_point(0))
            .expect("the video track describes itself");

        // One allocation shared by every bag: `Bytes` is refcounted, so the
        // hold's accounting sees eight mebibytes per bag while the test holds
        // eight mebibytes in total.
        let eight_mebibytes = Bytes::from(vec![0x00_u8; 8 * 1024 * 1024]);
        let mut bags_the_hold_accepted = 0usize;
        let refusal = loop {
            let stamp_ns = 33_000_000 * (bags_the_hold_accepted as i64 + 1);
            let planned = planner.plan_the_writes_for(
                "camera",
                video_access_unit(eight_mebibytes.clone(), false, stamp_ns),
            );
            match planned {
                Ok(nothing_written) => {
                    assert!(nothing_written.is_empty());
                    bags_the_hold_accepted += 1;
                    assert!(
                        bags_the_hold_accepted < 64,
                        "the hold accepted {bags_the_hold_accepted} bags of eight mebibytes \
                         without ever refusing"
                    );
                }
                Err(failure) => break refusal_of(failure),
            }
        };

        assert!(
            refusal.contains("microphone"),
            "the refusal names the track that never spoke: {refusal}"
        );
        assert!(
            !refusal.contains("camera"),
            "the track that did speak is not named as silent: {refusal}"
        );
    }

    #[test]
    fn a_cmaf_video_sample_carries_no_parameter_sets_once_the_init_segment_states_them() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera"]);

        let planned = planner
            .plan_the_writes_for("camera", a_video_sync_point(0))
            .expect("the sync point describes the track and publishes");

        let fragment = object_payloads_written_to(&planned, "1.m4s")
            .pop()
            .expect("the sync point was written");
        let sample = read_cmaf_fragment(&fragment).expect("the fragment reads back");
        let mut only_the_slice = (AN_H264_IDR_SLICE.len() as u32).to_be_bytes().to_vec();
        only_the_slice.extend_from_slice(&AN_H264_IDR_SLICE);
        assert_eq!(
            sample[0].sample_bytes, only_the_slice,
            "the parameter sets are in the init segment's sample entry, not in the mdat"
        );
    }

    #[test]
    fn a_video_tracks_parameter_sets_changing_after_the_init_segment_was_written_is_refused_by_name()
     {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera"]);
        planner
            .plan_the_writes_for("camera", a_video_sync_point(0))
            .expect("the first sync point describes the track");

        let refusal = refusal_of(
            planner
                .plan_the_writes_for(
                    "camera",
                    a_video_sync_point_carrying(&A_SECOND_H264_SEQUENCE_PARAMETER_SET, 66_000_000),
                )
                .expect_err("a written init segment cannot be revised"),
        );

        assert!(refusal.contains("camera"), "{refusal}");
        assert!(refusal.contains("parameter sets"), "{refusal}");
    }

    #[test]
    fn each_cmaf_track_numbers_its_own_fragments_from_one() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "screen"]);
        planner
            .plan_the_writes_for("camera", a_video_sync_point(0))
            .expect("the first track describes itself");
        let both_described = planner
            .plan_the_writes_for("screen", a_video_sync_point(0))
            .expect("the second track describes itself and the broadcast opens");
        let camera_second = planner
            .plan_the_writes_for("camera", a_video_delta_frame(33_000_000))
            .expect("the first track publishes on");
        let screen_second = planner
            .plan_the_writes_for("screen", a_video_delta_frame(33_000_000))
            .expect("the second track publishes on");

        let sequence_numbers_of = |instructions: &[MoqObjectWriteInstruction], track: &str| {
            object_payloads_written_to(instructions, track)
                .iter()
                .map(|fragment| cmaf_fragment_sequence_number(fragment))
                .collect::<Vec<u32>>()
        };
        assert_eq!(sequence_numbers_of(&both_described, "1.m4s"), vec![1]);
        assert_eq!(sequence_numbers_of(&both_described, "2.m4s"), vec![1]);
        assert_eq!(sequence_numbers_of(&camera_second, "1.m4s"), vec![2]);
        assert_eq!(sequence_numbers_of(&screen_second, "2.m4s"), vec![2]);
    }

    #[test]
    fn a_bag_whose_codec_is_not_the_one_its_track_was_first_published_from_is_refused_by_name() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["camera"]);
        planner
            .plan_the_writes_for("camera", a_video_sync_point(0))
            .expect("h264 publishes");

        let refusal = refusal_of(
            planner
                .plan_the_writes_for("camera", an_opus_packet(33_000_000))
                .expect_err("a track's medium is stated once"),
        );

        assert!(refusal.contains("camera"), "{refusal}");
        assert!(refusal.contains("h264"), "{refusal}");
        assert!(refusal.contains("opus"), "{refusal}");
    }

    #[test]
    fn a_publisher_holds_no_relay_session_until_it_has_an_object_to_write() {
        let mut publisher = MoqBroadcastPublisher::new(
            MoqRelayConfig {
                relay_endpoint_url: "https://relay.example/t0ken".to_owned(),
                broadcast_path: BROADCAST_NAMESPACE.to_owned(),
            },
            MoqContainerFormat::Cmaf,
        );

        publisher
            .declare_tracks(vec!["camera".to_owned(), "microphone".to_owned()])
            .expect("the links are declarable");

        assert!(
            !publisher.is_connected(),
            "declaring tracks must not spend a relay round trip inside `setup()`"
        );
    }
}
