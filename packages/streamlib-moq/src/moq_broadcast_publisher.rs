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
//! A data object never cuts a group — the audio rule, for the audio reason —
//! so a broadcast with no video in it is cut by the session's object bound and
//! by the age bound planned here.
//!
//! Whether a bag is published at all is decided here too, before any of it
//! reaches the transport — see [`crate::delivery_deadline`] for why that is the
//! only moment there is.
//!
//! The transport is reached only through the instruction list this module
//! plans, so everything up to the byte handed to a QUIC stream is decided
//! without a relay. Planning states what would be written; nothing about the
//! broadcast's own progress is recorded until the publisher reports that every
//! instruction of a plan reached the transport.

use std::collections::{HashMap, HashSet};

use bytes::Bytes;

use crate::annex_b_access_unit::{
    AnnexBNalHeaderGrammar, ParameterSetsFromAnnexBAccessUnit, length_prefix_annex_b_access_unit,
    parameter_sets_of_annex_b_access_unit,
};
use crate::cmaf_fragment::build_cmaf_fragment;
use crate::cmaf_init_segment::{CmafTrackDescriptionForTheInitSegment, build_cmaf_init_segment};
use crate::cmaf_sample_entry::{
    CmafTrackSampleEntry, build_opus_sample_entry, build_video_sample_entry,
};
use crate::cmaf_track_timeline::{
    CmafTrackTimeline, OPUS_TRACK_TIMESCALE_HZ, VIDEO_TRACK_TIMESCALE_HZ,
};
use crate::delivery_deadline::{
    DeliveryDeadlineVerdict, MoqPublisherDeliveryDeadline, ObjectsTheDeliveryDeadlineShedOnOneTrack,
};
use crate::encoded_media_sample::{EncodedMediaSample, TrackMedium};
use crate::error::{MoqExtensionError, Result};
use crate::moq_broadcast_catalog::{
    CATALOG_TRACK_NAME, INIT_TRACK_NAME, MoqBroadcastCatalog, MoqCatalogTrackDescription,
    MoqCatalogTrackSelectionParameters, STREAMLIB_BAG_PACKAGING, media_track_name,
};
use crate::moq_relay_config::MoqRelayConfig;
use crate::moq_session::{
    LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS, MoqBroadcastPublishingSession,
    track_priority_of,
};
use crate::moq_track_sample::{MoqTrackKind, MoqTrackSample, WhatATrackCarries};
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
/// still not become describable.
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
}

/// Publishes encoded bags to one MoQ broadcast, one object per bag.
pub(crate) struct MoqBroadcastPublisher {
    relay_config: MoqRelayConfig,
    object_write_planner: MoqBroadcastObjectWritePlanner,
    publishing_session: Option<MoqBroadcastPublishingSession>,
}

impl MoqBroadcastPublisher {
    /// A publisher that has declared no tracks and holds no connection.
    pub(crate) fn new(
        relay_config: MoqRelayConfig,
        container_format: MoqContainerFormat,
        delivery_deadline: MoqPublisherDeliveryDeadline,
    ) -> Self {
        let broadcast_namespace = relay_config.broadcast_path.clone();
        Self {
            relay_config,
            object_write_planner: MoqBroadcastObjectWritePlanner::of(
                container_format,
                broadcast_namespace,
                delivery_deadline,
            ),
            publishing_session: None,
        }
    }

    /// Fix this broadcast's tracks and their order. Track ids are 1-based in
    /// the order given, and that order reaches the catalog unchanged. Under
    /// `streamlib_bag` the app may name the tracks, one name per link in the
    /// same order; absent, each is its link's own name.
    pub(crate) fn declare_tracks(
        &mut self,
        inbound_link_names: Vec<String>,
        track_names: Option<Vec<String>>,
    ) -> Result<()> {
        self.object_write_planner
            .declare_tracks(inbound_link_names, track_names)
    }

    /// Publish one bag, connecting the session on the first bag the broadcast
    /// is handed.
    pub(crate) async fn publish(
        &mut self,
        inbound_link_name: &str,
        sample: MoqTrackSample,
        now_ns: i64,
    ) -> Result<WhatBecameOfOnePublishedBag> {
        // Connected here rather than in `declare_tracks`: a relay round trip
        // inside `setup()` spends the helper's start-up budget before the graph
        // is running. Before the plan rather than after it: a plan is the only
        // account of what a bag became, and a connect that fails once one is in
        // hand discards writes the broadcast can never make again.
        self.connect_the_publishing_session_unless_it_is_already_up()
            .await?;

        // Everything the plan is about to spend, so a plan that does not reach
        // the relay whole can give it back.
        let spent_before_planning = self.object_write_planner.what_planning_spends();
        let planned =
            self.object_write_planner
                .plan_the_writes_for(inbound_link_name, sample, now_ns);
        let PlannedMoqObjectWrites {
            instructions,
            writing_them_all_opens_the_broadcast,
            the_delivery_deadline_shed_it,
        } = match planned {
            Ok(planned) => planned,
            Err(refusal) => {
                self.object_write_planner
                    .give_back_what_planning_spent(spent_before_planning);
                return Err(refusal);
            }
        };
        if the_delivery_deadline_shed_it {
            return Ok(WhatBecameOfOnePublishedBag::ShedByTheDeliveryDeadline);
        }
        if instructions.is_empty() {
            return Ok(WhatBecameOfOnePublishedBag::ReachesTheTransport);
        }

        let Some(session) = self.publishing_session.as_mut() else {
            self.object_write_planner
                .give_back_what_planning_spent(spent_before_planning);
            return Err(MoqExtensionError::NotConnected { role: "publishing" });
        };
        let mut wrote_them_all = Ok(());
        for instruction in instructions {
            wrote_them_all = match instruction {
                MoqObjectWriteInstruction::CutANewGroupOnEveryTrack => {
                    session.open_a_new_group_on_every_track();
                    Ok(())
                }
                MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                    moq_track_name,
                    object_payload,
                } => session.write_the_only_object_of(&moq_track_name, object_payload),
                MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                    moq_track_name,
                    object_payload,
                    publisher_priority,
                } => session.write_object(&moq_track_name, object_payload, publisher_priority),
            };
            if wrote_them_all.is_err() {
                break;
            }
        }
        if let Err(failure) = wrote_them_all {
            self.object_write_planner
                .give_back_what_planning_spent(spent_before_planning);
            return Err(failure);
        }

        if writing_them_all_opens_the_broadcast {
            self.object_write_planner
                .record_that_every_descriptive_object_and_held_sample_was_written();
        }
        Ok(WhatBecameOfOnePublishedBag::ReachesTheTransport)
    }

    async fn connect_the_publishing_session_unless_it_is_already_up(&mut self) -> Result<()> {
        if self.publishing_session.is_some() {
            return Ok(());
        }
        let connected = MoqBroadcastPublishingSession::connect(
            self.relay_config.clone(),
            self.object_write_planner.moq_track_names(),
        )
        .await?;
        self.publishing_session = Some(connected);
        Ok(())
    }

    /// Whether the relay session is up.
    pub(crate) fn is_connected(&self) -> bool {
        self.publishing_session.is_some()
    }

    /// What the delivery deadline has shed so far, per inbound link.
    ///
    /// Read back across the CPython boundary rather than only logged: this
    /// crate's `tracing` events reach no dispatcher inside a helper process,
    /// so a drop reported only that way is reported to nobody.
    pub(crate) fn objects_the_delivery_deadline_shed(
        &self,
    ) -> Vec<ObjectsTheDeliveryDeadlineShedOnOneTrack> {
        self.object_write_planner
            .objects_the_delivery_deadline_shed()
    }

    /// Finish every open group and end the session.
    /// Close the broadcast, handing back the encoded media the hold discards.
    ///
    /// Returned rather than only logged: this crate's `tracing` events reach no
    /// dispatcher inside a helper process, so a loss reported only that way is
    /// reported to nobody. The caller says it through the log channel the
    /// helper does install.
    pub(crate) async fn close(&mut self) -> Option<EncodedMediaTheHoldDiscardsAtClose> {
        let discarded = self
            .object_write_planner
            .the_encoded_media_the_hold_discards_at_close();
        if let Some(discarded) = discarded.as_ref() {
            tracing::warn!(
                broadcast = %self.relay_config.broadcast_path,
                discarded_samples = discarded.held_sample_count,
                discarded_bytes = discarded.held_byte_count,
                why = %discarded.why_the_broadcast_never_opened,
                "closing a MoQ broadcast that never became playable; the encoded media held for \
                 its undescribed tracks is discarded unwritten"
            );
        }
        if let Some(session) = self.publishing_session.take() {
            session.close().await;
        }
        discarded
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
        /// The rung the track's group is opened at — audio outranks video, and
        /// both sit below the descriptive tracks.
        publisher_priority: u8,
    },
}

/// What became of one bag `publish` was handed, for the caller that counts and
/// says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WhatBecameOfOnePublishedBag {
    /// Its object reached the transport — or the CMAF hold keeps it and it
    /// reaches the transport with the flush.
    ReachesTheTransport,
    /// The delivery deadline shed it; nothing of it reaches the transport.
    ShedByTheDeliveryDeadline,
}

/// What one bag became, ready for a transport that has not been reached yet.
#[derive(Debug, Clone, PartialEq)]
struct PlannedMoqObjectWrites {
    instructions: Vec<MoqObjectWriteInstruction>,
    the_delivery_deadline_shed_it: bool,
    /// Whether these instructions carry the broadcast's descriptive objects and
    /// every sample the hold was keeping for them. The planner records that
    /// only once the caller reports all of them written, so a refusal or a dead
    /// relay leaves the descriptive objects still owed rather than lost.
    writing_them_all_opens_the_broadcast: bool,
}

impl PlannedMoqObjectWrites {
    /// A bag the CMAF hold keeps until every track is describable.
    fn of_a_bag_the_hold_keeps() -> Self {
        Self {
            instructions: Vec::new(),
            the_delivery_deadline_shed_it: false,
            writing_them_all_opens_the_broadcast: false,
        }
    }

    /// A bag the delivery deadline shed; the transport is asked for nothing.
    fn of_a_bag_the_delivery_deadline_shed() -> Self {
        Self {
            instructions: Vec::new(),
            the_delivery_deadline_shed_it: true,
            writing_them_all_opens_the_broadcast: false,
        }
    }
}

/// The encoded media a broadcast that never became playable throws away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EncodedMediaTheHoldDiscardsAtClose {
    pub(crate) held_sample_count: usize,
    pub(crate) held_byte_count: usize,
    pub(crate) why_the_broadcast_never_opened: String,
}

/// Everything about a broadcast that needs no connection: its tracks, their
/// names, their descriptions, and what each bag turns into.
struct MoqBroadcastObjectWritePlanner {
    container_format: MoqContainerFormat,
    broadcast_namespace: String,
    delivery_deadline: MoqPublisherDeliveryDeadline,
    /// Declaration order, which is also track-id order and catalog order.
    declared_tracks: Vec<DeclaredMoqTrackPublicationState>,
    declared_track_index_by_inbound_link_name: HashMap<String, usize>,
    /// Whether the catalog — and, on CMAF, the init object — have reached the
    /// transport.
    the_descriptive_objects_have_been_written: bool,
    samples_held_until_every_cmaf_track_is_described:
        Vec<HeldSampleAwaitingEveryCmafTrackDescription>,
    bytes_held_until_every_cmaf_track_is_described: usize,
    /// Whether the hold stopped at its bound. A bag refused there is a hole in
    /// every track's timeline, so the refusal covers every later bag rather
    /// than only the one that would not fit.
    the_hold_stopped_at_its_bound: bool,
    /// When the broadcast's open group was opened, on the publisher's own
    /// clock; `None` before the first object. What the age bound is measured
    /// from on a broadcast with no video.
    the_open_group_was_opened_at_ns: Option<i64>,
}

/// What planning spends before a byte reaches the relay, so a plan that does
/// not reach it whole can give it back.
#[derive(Debug, Clone, PartialEq)]
struct WhatPlanningSpends {
    /// Per track: the next fragment sequence number and the CMAF timeline.
    cmaf_placement_by_track: Vec<(u32, Option<CmafTrackTimeline>)>,
    the_open_group_was_opened_at_ns: Option<i64>,
}

impl MoqBroadcastObjectWritePlanner {
    fn of(
        container_format: MoqContainerFormat,
        broadcast_namespace: String,
        delivery_deadline: MoqPublisherDeliveryDeadline,
    ) -> Self {
        Self {
            container_format,
            broadcast_namespace,
            delivery_deadline,
            declared_tracks: Vec::new(),
            declared_track_index_by_inbound_link_name: HashMap::new(),
            the_descriptive_objects_have_been_written: false,
            samples_held_until_every_cmaf_track_is_described: Vec::new(),
            bytes_held_until_every_cmaf_track_is_described: 0,
            the_hold_stopped_at_its_bound: false,
            the_open_group_was_opened_at_ns: None,
        }
    }

    fn declare_tracks(
        &mut self,
        inbound_link_names: Vec<String>,
        track_names: Option<Vec<String>>,
    ) -> Result<()> {
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
        let moq_media_track_names =
            self.moq_media_track_names_for(&inbound_link_names, track_names)?;

        for (declared_track_index, (inbound_link_name, moq_media_track_name)) in inbound_link_names
            .iter()
            .zip(moq_media_track_names)
            .enumerate()
        {
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

    /// The name each track answers to on the relay, one per inbound link in
    /// declaration order.
    ///
    /// Under `cmaf` they are the container's own — `.catalog`, `0.mp4` and
    /// `{track_id}.m4s` are a fallback contract, not this wheel's to vary,
    /// because the reference subscriber hardcodes all three when it is not
    /// asked to fetch a catalog — and names from the app are refused. Under
    /// `streamlib_bag` they are the app's, one per link, or absent that each
    /// link's own name.
    fn moq_media_track_names_for(
        &self,
        inbound_link_names: &[String],
        track_names: Option<Vec<String>>,
    ) -> Result<Vec<String>> {
        match self.container_format {
            MoqContainerFormat::Cmaf => {
                if track_names.is_some() {
                    return Err(MoqExtensionError::Refused {
                        what: format!(
                            "`track_names` cannot be set under `{CMAF_CONTAINER_WIRE_NAME}`: that \
                             container names its tracks `{CATALOG_TRACK_NAME}`, \
                             `{INIT_TRACK_NAME}` and `{{track_id}}.m4s`, because a subscriber not \
                             asked to fetch a catalog hardcodes exactly those; name tracks under \
                             `{STREAMLIB_BAG_CONTAINER_WIRE_NAME}`"
                        ),
                    });
                }
                Ok((1..=inbound_link_names.len())
                    .map(|track_id| media_track_name(track_id as u32))
                    .collect())
            }
            MoqContainerFormat::StreamlibBag => {
                let track_names = match track_names {
                    None => inbound_link_names.to_vec(),
                    Some(track_names) => {
                        refuse_track_names_that_do_not_name_the_links(
                            &track_names,
                            inbound_link_names.len(),
                        )?;
                        track_names
                    }
                };
                if track_names.iter().any(|name| name == CATALOG_TRACK_NAME) {
                    return Err(MoqExtensionError::Refused {
                        what: format!(
                            "`{CATALOG_TRACK_NAME}` cannot name a media track published over \
                             `{STREAMLIB_BAG_CONTAINER_WIRE_NAME}`: that name is already this \
                             broadcast's catalog track"
                        ),
                    });
                }
                Ok(track_names)
            }
        }
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
        sample: MoqTrackSample,
        now_ns: i64,
    ) -> Result<PlannedMoqObjectWrites> {
        let declared_track_index = self.declared_track_index_of(inbound_link_name)?;
        let delivery_deadline = self.delivery_deadline;
        let track = &mut self.declared_tracks[declared_track_index];
        track.refuse_a_sample_unlike_the_one_this_track_was_first_published_from(&sample)?;
        // Counted before the deadline reads it, because this counts what the
        // link handed over and not what reached the wire — a track shedding
        // every frame is still speaking, and the hold's account of a silent
        // track must not call it silent.
        track.count_one_more_delivered_bag();

        match delivery_deadline.verdict_for_one_sample(
            &sample,
            now_ns,
            track.the_open_group_is_being_shed,
        ) {
            DeliveryDeadlineVerdict::ShedItAndTheRestOfItsGroup => {
                track.record_one_object_the_delivery_deadline_shed(payload_byte_count_of(&sample));
                return Ok(PlannedMoqObjectWrites::of_a_bag_the_delivery_deadline_shed());
            }
            DeliveryDeadlineVerdict::PublishIt => track.the_open_group_is_being_shed = false,
        }

        let planned = match self.container_format {
            MoqContainerFormat::StreamlibBag => {
                self.plan_a_streamlib_bag_write(declared_track_index, &sample, now_ns)?
            }
            MoqContainerFormat::Cmaf => {
                self.plan_a_cmaf_write(declared_track_index, sample, now_ns)?
            }
        };
        // A held bag asks the transport for nothing, so the open group's age
        // is untouched by it.
        if !planned.instructions.is_empty() {
            self.note_when_the_open_group_was_opened(&planned.instructions, now_ns);
        }
        Ok(planned)
    }

    fn plan_a_streamlib_bag_write(
        &mut self,
        declared_track_index: usize,
        sample: &MoqTrackSample,
        now_ns: i64,
    ) -> Result<PlannedMoqObjectWrites> {
        let object_payload = encode_object(sample)?;
        let writing_them_all_opens_the_broadcast = !self.the_descriptive_objects_have_been_written;
        let mut instructions = Vec::new();
        if writing_them_all_opens_the_broadcast {
            instructions.push(
                MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                    moq_track_name: CATALOG_TRACK_NAME.to_owned(),
                    object_payload: self.build_the_broadcast_catalog()?,
                },
            );
        }
        if this_sample_cuts_a_new_group_on_every_track(sample)
            || self.the_age_bound_cuts_before_this_write(now_ns)
        {
            instructions.push(MoqObjectWriteInstruction::CutANewGroupOnEveryTrack);
        }
        instructions.push(
            MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                moq_track_name: self.declared_tracks[declared_track_index]
                    .moq_media_track_name
                    .clone(),
                object_payload,
                publisher_priority: track_priority_of(sample.kind()),
            },
        );
        Ok(PlannedMoqObjectWrites {
            instructions,
            the_delivery_deadline_shed_it: false,
            writing_them_all_opens_the_broadcast,
        })
    }

    fn plan_a_cmaf_write(
        &mut self,
        declared_track_index: usize,
        sample: MoqTrackSample,
        now_ns: i64,
    ) -> Result<PlannedMoqObjectWrites> {
        // Before the hold, not inside it: a data bag can never be described,
        // so holding for it would stall every other track up to the hold's
        // bound and only then refuse — and the refusal would name the wrong
        // thing. The link's first bag has already marked it as data, which
        // is what takes it out of everything the init segment describes.
        let sample = match sample {
            MoqTrackSample::DataObject(_) => {
                return Err(
                    self.refuse_a_data_bag_a_cmaf_broadcast_cannot_carry(declared_track_index)
                );
            }
            MoqTrackSample::EncodedMedia(sample) => sample,
        };
        if self.the_hold_stopped_at_its_bound {
            return Err(self.refuse_every_bag_a_stopped_hold_can_never_publish());
        }

        let track = &mut self.declared_tracks[declared_track_index];
        track.learn_its_cmaf_description_from(&sample)?;
        track.refuse_a_sync_point_the_init_segment_it_is_owed_cannot_describe(&sample)?;

        if self.the_descriptive_objects_have_been_written {
            let mut instructions = Vec::new();
            if self.the_age_bound_cuts_before_this_write(now_ns) {
                instructions.push(MoqObjectWriteInstruction::CutANewGroupOnEveryTrack);
            }
            instructions.extend(
                self.declared_tracks[declared_track_index].plan_the_cmaf_fragment_of(&sample)?,
            );
            return Ok(PlannedMoqObjectWrites {
                instructions,
                the_delivery_deadline_shed_it: false,
                writing_them_all_opens_the_broadcast: false,
            });
        }
        if !self.every_cmaf_media_track_is_described() {
            self.hold_until_every_cmaf_media_track_is_described(declared_track_index, sample)?;
            return Ok(PlannedMoqObjectWrites::of_a_bag_the_hold_keeps());
        }

        // Init first, catalog second: a subscriber that reads the catalog and
        // then follows `initTrack` must find the init object already there.
        let mut instructions = vec![
            MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                moq_track_name: INIT_TRACK_NAME.to_owned(),
                object_payload: self.build_the_cmaf_init_segment()?,
            },
            MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                moq_track_name: CATALOG_TRACK_NAME.to_owned(),
                object_payload: self.build_the_broadcast_catalog()?,
            },
        ];
        for held_sample in &self.samples_held_until_every_cmaf_track_is_described {
            instructions.extend(
                self.declared_tracks[held_sample.declared_track_index]
                    .plan_the_cmaf_fragment_of(&held_sample.sample)?,
            );
        }
        instructions
            .extend(self.declared_tracks[declared_track_index].plan_the_cmaf_fragment_of(&sample)?);
        Ok(PlannedMoqObjectWrites {
            instructions,
            the_delivery_deadline_shed_it: false,
            writing_them_all_opens_the_broadcast: true,
        })
    }

    /// Whether this write must cut a group first because the broadcast has no
    /// video to cut on — no track has published a video bag yet — and its open
    /// group has aged past the bound.
    fn the_age_bound_cuts_before_this_write(&self, now_ns: i64) -> bool {
        if self.a_track_has_published_video() {
            return false;
        }
        self.the_open_group_was_opened_at_ns
            .is_some_and(|opened_at_ns| {
                now_ns.saturating_sub(opened_at_ns)
                    > LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS
            })
    }

    fn a_track_has_published_video(&self) -> bool {
        self.declared_tracks
            .iter()
            .any(|track| track.carries(MoqTrackKind::Media(TrackMedium::Video)))
    }

    /// The open group is the one these instructions cut, or the one the first
    /// object write opened on the session's side without a cut.
    fn note_when_the_open_group_was_opened(
        &mut self,
        instructions: &[MoqObjectWriteInstruction],
        now_ns: i64,
    ) {
        let these_cut_a_group =
            instructions.contains(&MoqObjectWriteInstruction::CutANewGroupOnEveryTrack);
        if these_cut_a_group || self.the_open_group_was_opened_at_ns.is_none() {
            self.the_open_group_was_opened_at_ns = Some(now_ns);
        }
    }

    fn refuse_a_data_bag_a_cmaf_broadcast_cannot_carry(
        &self,
        declared_track_index: usize,
    ) -> MoqExtensionError {
        MoqExtensionError::Refused {
            what: format!(
                "`{}` delivered a data bag — one with no `bitstream` — and a \
                 `{CMAF_CONTAINER_WIRE_NAME}` broadcast has no packaging for it: a CMAF fragment \
                 carries coded media samples and nothing else. The link publishes nothing on \
                 this broadcast, whose media tracks are described without it; a data track \
                 needs `container_format=\"{STREAMLIB_BAG_CONTAINER_WIRE_NAME}\"`, which carries \
                 the bag whole",
                self.declared_tracks[declared_track_index].inbound_link_name
            ),
        }
    }

    /// Where every track's fragment numbering and timeline stand, and when the
    /// open group was opened.
    ///
    /// Planning a fragment spends a sequence number and advances its track's
    /// timeline, and a cut moves the open group's opening instant — all before
    /// a single byte reaches the relay. A plan that is not fully written must
    /// give them back, or the broadcast's `mfhd.sequence_number` skips, the
    /// next sample's duration is measured against a stamp no subscriber ever
    /// saw, and the age bound counts from a cut that never happened.
    fn what_planning_spends(&self) -> WhatPlanningSpends {
        WhatPlanningSpends {
            cmaf_placement_by_track: self
                .declared_tracks
                .iter()
                .map(|track| {
                    (
                        track.next_cmaf_fragment_sequence_number,
                        track
                            .cmaf_description
                            .as_ref()
                            .map(|description| description.cmaf_track_timeline),
                    )
                })
                .collect(),
            the_open_group_was_opened_at_ns: self.the_open_group_was_opened_at_ns,
        }
    }

    /// Put back what a snapshot found. A description a sample taught the
    /// planner is deliberately kept: it describes the stream, not the write
    /// that failed.
    fn give_back_what_planning_spent(&mut self, spent: WhatPlanningSpends) {
        for (track, (sequence_number, timeline)) in self
            .declared_tracks
            .iter_mut()
            .zip(spent.cmaf_placement_by_track)
        {
            track.next_cmaf_fragment_sequence_number = sequence_number;
            if let (Some(description), Some(timeline)) = (track.cmaf_description.as_mut(), timeline)
            {
                description.cmaf_track_timeline = timeline;
            }
        }
        self.the_open_group_was_opened_at_ns = spent.the_open_group_was_opened_at_ns;
    }

    /// Take the broadcast's descriptive objects and its hold as written, which
    /// is what makes it playable and what empties the hold.
    fn record_that_every_descriptive_object_and_held_sample_was_written(&mut self) {
        self.the_descriptive_objects_have_been_written = true;
        tracing::info!(
            broadcast = %self.broadcast_namespace,
            tracks = self.tracks_the_descriptive_objects_describe(),
            samples_written_from_the_hold =
                self.samples_held_until_every_cmaf_track_is_described.len(),
            "every track of this broadcast is described and its descriptive objects are \
             written; it is playable from here"
        );
        self.samples_held_until_every_cmaf_track_is_described
            .clear();
        self.bytes_held_until_every_cmaf_track_is_described = 0;
    }

    fn hold_until_every_cmaf_media_track_is_described(
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
            self.the_hold_stopped_at_its_bound = true;
            return Err(self.refuse_every_bag_a_stopped_hold_can_never_publish());
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

    fn refuse_every_bag_a_stopped_hold_can_never_publish(&self) -> MoqExtensionError {
        MoqExtensionError::Refused {
            what: format!(
                "{} bytes of encoded media are held because {}; this broadcast's CMAF init \
                 segment describes every track in one `moov` written once, so no track can \
                 publish until every one of them is describable, and the hold stops at \
                 {HIGHEST_BYTES_HELD_WHILE_A_DECLARED_CMAF_TRACK_HAS_NOT_SPOKEN} bytes — a \
                 broadcast whose hold has stopped refuses every later bag too, because the \
                 media it could publish now has a gap where the refused bags were",
                self.bytes_held_until_every_cmaf_track_is_described,
                self.describe_the_tracks_the_init_segment_is_waiting_on()
            ),
        }
    }

    /// What the deadline has shed, by the link an operator wired. A track that
    /// shed nothing is left out, so an empty list is a run that dropped
    /// nothing.
    fn objects_the_delivery_deadline_shed(&self) -> Vec<ObjectsTheDeliveryDeadlineShedOnOneTrack> {
        self.declared_tracks
            .iter()
            .filter(|track| track.objects_the_delivery_deadline_shed > 0)
            .map(|track| ObjectsTheDeliveryDeadlineShedOnOneTrack {
                inbound_link_name: track.inbound_link_name.clone(),
                objects_shed: track.objects_the_delivery_deadline_shed,
                bytes_shed: track.bytes_the_delivery_deadline_shed,
            })
            .collect()
    }

    /// Whether the init segment can be built: every track it describes has
    /// said what it is. A link that has shown itself to be data is outside
    /// the init segment altogether — a `cmaf` broadcast publishes nothing on
    /// it and does not wait on it.
    fn every_cmaf_media_track_is_described(&self) -> bool {
        self.the_cmaf_media_tracks_this_broadcast_describes()
            .all(|track| track.cmaf_description.is_some())
    }

    /// How many tracks the catalog — and, on `cmaf`, the init segment — name.
    fn tracks_the_descriptive_objects_describe(&self) -> usize {
        match self.container_format {
            MoqContainerFormat::Cmaf => self
                .the_cmaf_media_tracks_this_broadcast_describes()
                .count(),
            MoqContainerFormat::StreamlibBag => self.declared_tracks.len(),
        }
    }

    fn the_cmaf_media_tracks_this_broadcast_describes(
        &self,
    ) -> impl Iterator<Item = &DeclaredMoqTrackPublicationState> {
        self.declared_tracks
            .iter()
            .filter(|track| !track.carries(MoqTrackKind::Data))
    }

    /// What a `close()` throws away, or `None` when the hold is empty.
    fn the_encoded_media_the_hold_discards_at_close(
        &self,
    ) -> Option<EncodedMediaTheHoldDiscardsAtClose> {
        if self
            .samples_held_until_every_cmaf_track_is_described
            .is_empty()
        {
            return None;
        }
        Some(EncodedMediaTheHoldDiscardsAtClose {
            held_sample_count: self.samples_held_until_every_cmaf_track_is_described.len(),
            held_byte_count: self.bytes_held_until_every_cmaf_track_is_described,
            why_the_broadcast_never_opened: self.describe_why_the_broadcast_never_opened(),
        })
    }

    /// Why held media was never flushed: a described track was still owed,
    /// or the last link to speak showed itself to be data — which takes it
    /// out of the init segment's wait — and no media bag came after it to
    /// open the broadcast.
    fn describe_why_the_broadcast_never_opened(&self) -> String {
        if !self.every_cmaf_media_track_is_described() {
            return format!(
                "{} — so the init segment could not be built",
                self.describe_the_tracks_the_init_segment_is_waiting_on()
            );
        }
        let data_links: Vec<String> = self
            .declared_tracks
            .iter()
            .filter(|track| track.carries(MoqTrackKind::Data))
            .map(|track| format!("`{}`", track.inbound_link_name))
            .collect();
        if data_links.is_empty() {
            return "every media track was described and no media bag arrived after the last \
                    description to open the broadcast"
                .to_owned();
        }
        format!(
            "every media track was described once {} showed itself to be data, and no media \
             bag arrived after that to open the broadcast",
            data_links.join(", ")
        )
    }

    fn build_the_cmaf_init_segment(&self) -> Result<Bytes> {
        let mut init_segment_track_descriptions = Vec::with_capacity(self.declared_tracks.len());
        for track in self.the_cmaf_media_tracks_this_broadcast_describes() {
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
                for track in self.the_cmaf_media_tracks_this_broadcast_describes() {
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
                            catalog_selection_parameters_of_a_streamlib_bag_track(),
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

    /// Why each undescribed track cannot be described yet. A link nothing ever
    /// wrote to and an encoder that has delivered bags but never a sync point
    /// are both real misconfigurations, and naming the second as the first
    /// sends the operator to the wrong end of the graph.
    fn describe_the_tracks_the_init_segment_is_waiting_on(&self) -> String {
        let waiting_on: Vec<String> = self
            .the_cmaf_media_tracks_this_broadcast_describes()
            .filter(|track| track.cmaf_description.is_none())
            .map(|track| match track.bags_this_track_has_delivered {
                0 => format!("`{}` has delivered no bag at all", track.inbound_link_name),
                delivered => format!(
                    "`{}` has delivered {delivered} bag{} and no sync point to describe it from",
                    track.inbound_link_name,
                    if delivered == 1 { "" } else { "s" }
                ),
            })
            .collect();
        if waiting_on.is_empty() {
            "no track".to_owned()
        } else {
            waiting_on.join(", ")
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
    first_published: Option<WhatATrackCarries>,
    bags_this_track_has_delivered: u64,
    cmaf_description: Option<CmafTrackDescriptionLearnedFromItsFirstUsableSample>,
    parameter_sets_the_init_segment_states: ParameterSetsFromAnnexBAccessUnit,
    next_cmaf_fragment_sequence_number: u32,
    /// Whether the delivery deadline is shedding this track's open group. It
    /// ends at the next sync point, which is also what opens the next group.
    the_open_group_is_being_shed: bool,
    objects_the_delivery_deadline_shed: u64,
    bytes_the_delivery_deadline_shed: u64,
}

impl DeclaredMoqTrackPublicationState {
    fn of(inbound_link_name: String, cmaf_track_id: u32, moq_media_track_name: String) -> Self {
        Self {
            inbound_link_name,
            cmaf_track_id,
            moq_media_track_name,
            first_published: None,
            bags_this_track_has_delivered: 0,
            cmaf_description: None,
            parameter_sets_the_init_segment_states: ParameterSetsFromAnnexBAccessUnit::default(),
            // ISO/IEC 14496-12 §8.8.5: `mfhd.sequence_number` counts one
            // track's fragments from one.
            next_cmaf_fragment_sequence_number: 1,
            the_open_group_is_being_shed: false,
            objects_the_delivery_deadline_shed: 0,
            bytes_the_delivery_deadline_shed: 0,
        }
    }

    fn count_one_more_delivered_bag(&mut self) {
        self.bags_this_track_has_delivered = self.bags_this_track_has_delivered.saturating_add(1);
    }

    fn record_one_object_the_delivery_deadline_shed(&mut self, encoded_byte_count: usize) {
        self.the_open_group_is_being_shed = true;
        self.objects_the_delivery_deadline_shed =
            self.objects_the_delivery_deadline_shed.saturating_add(1);
        self.bytes_the_delivery_deadline_shed = self
            .bytes_the_delivery_deadline_shed
            .saturating_add(encoded_byte_count as u64);
    }

    /// Whether this track's first bag said it carries `kind`.
    fn carries(&self, kind: MoqTrackKind) -> bool {
        self.first_published
            .as_ref()
            .is_some_and(|carried| carried.kind == kind)
    }

    fn refuse_a_sample_unlike_the_one_this_track_was_first_published_from(
        &mut self,
        sample: &MoqTrackSample,
    ) -> Result<()> {
        match &self.first_published {
            Some(first) if !first.matches(sample) => Err(MoqExtensionError::Refused {
                what: format!(
                    "`{}` first published {first} and this bag carries {}; one link is one \
                     track, and a MoQ track's kind and codec are stated once — by its first \
                     bag, and on `{CMAF_CONTAINER_WIRE_NAME}` by the catalog and the init \
                     segment — so neither can be revised",
                    self.inbound_link_name,
                    sample.what_it_carries()
                ),
            }),
            Some(_) => Ok(()),
            None => {
                self.first_published = Some(sample.what_it_carries());
                Ok(())
            }
        }
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

    /// Refuse a reconfiguring sync point where it arrives, not where it is
    /// written: a held bag reaches the write path a whole flush later, and by
    /// then the init segment has been built from the sets it drifted from.
    fn refuse_a_sync_point_the_init_segment_it_is_owed_cannot_describe(
        &self,
        sample: &EncodedMediaSample,
    ) -> Result<()> {
        let EncodedMediaSample::VideoAccessUnit(unit) = sample else {
            return Ok(());
        };
        if self.cmaf_description.is_none() {
            return Ok(());
        }
        // Any access unit carrying parameter sets is checked, not only a sync
        // point: an encoder may repeat its sets on a delta frame, and a bag the
        // ingest path waves through is one the flush refuses a whole hold
        // later — with the init segment already built from the sets it drifted
        // from, and no way to plan the hold that now contains it.
        let grammar = nal_header_grammar_of(&unit.codec, &self.inbound_link_name)?;
        self.refuse_parameter_sets_this_tracks_sample_entry_cannot_state(
            &parameter_sets_of_annex_b_access_unit(&unit.annex_b_access_unit, grammar),
        )
    }

    fn refuse_parameter_sets_this_tracks_sample_entry_cannot_state(
        &self,
        parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
    ) -> Result<()> {
        if parameter_sets_are_empty(parameter_sets)
            || *parameter_sets == self.parameter_sets_the_init_segment_states
        {
            return Ok(());
        }
        Err(MoqExtensionError::Refused {
            what: format!(
                "`{}` delivered parameter sets different from the ones its init segment states, \
                 and a CMAF init segment is written once — ISO/IEC 14496-12 §6.1.2 puts the \
                 sample entry in the one `moov` — so this stream cannot be re-described, and \
                 publishing on would misdescribe every sample after it",
                self.inbound_link_name
            ),
        })
    }

    /// The bytes this sample's `mdat` carries.
    fn mdat_sample_bytes_of(&self, sample: &EncodedMediaSample) -> Result<Vec<u8>> {
        match sample {
            EncodedMediaSample::VideoAccessUnit(unit) => {
                let grammar = nal_header_grammar_of(&unit.codec, &self.inbound_link_name)?;
                let length_prefixed =
                    length_prefix_annex_b_access_unit(&unit.annex_b_access_unit, grammar);
                self.refuse_parameter_sets_this_tracks_sample_entry_cannot_state(
                    &length_prefixed.parameter_sets,
                )?;
                // The parameter sets are in the init segment's sample entry,
                // which is where a CMAF reader looks; carrying them again here
                // would hand a decoder bytes it configured from already.
                Ok(length_prefixed.length_prefixed_sample_bytes)
            }
            EncodedMediaSample::AudioPacket(packet) => Ok(packet.opus_packet.to_vec()),
        }
    }

    /// The group cut this sample forces on every track, then its own fragment.
    fn plan_the_cmaf_fragment_of(
        &mut self,
        sample: &EncodedMediaSample,
    ) -> Result<Vec<MoqObjectWriteInstruction>> {
        let sample_bytes = self.mdat_sample_bytes_of(sample)?;
        let Some(description) = self.cmaf_description.as_mut() else {
            return Err(MoqExtensionError::Refused {
                what: format!(
                    "`{}` has no CMAF track description, so nothing states the timescale its \
                     samples are placed on",
                    self.inbound_link_name
                ),
            });
        };

        // An Opus packet states its own duration and a coded picture does not,
        // so only the video track's `trun` is measured from arrival stamps.
        let placement = match sample {
            EncodedMediaSample::AudioPacket(packet) => description
                .cmaf_track_timeline
                .place_sample_of_stated_duration(sample.timestamp_ns(), packet.sample_count),
            EncodedMediaSample::VideoAccessUnit(_) => description
                .cmaf_track_timeline
                .place_sample_of_duration_measured_to_its_predecessor(sample.timestamp_ns()),
        };
        let sequence_number = self.next_cmaf_fragment_sequence_number;
        self.next_cmaf_fragment_sequence_number =
            self.next_cmaf_fragment_sequence_number.saturating_add(1);
        // The track's own `tkhd.track_id`, never a constant: a fragment names
        // the track it belongs to, and on a two-track broadcast a fixed id
        // would route the audio track's samples onto the video track's
        // timeline — or be rejected outright.
        let object_payload = build_cmaf_fragment(
            self.cmaf_track_id,
            sequence_number,
            placement.decode_time,
            placement.duration,
            sample.is_sync_point(),
            &sample_bytes,
        )?;

        let mut instructions = Vec::new();
        if this_media_sample_cuts_a_new_group_on_every_track(sample) {
            instructions.push(MoqObjectWriteInstruction::CutANewGroupOnEveryTrack);
        }
        instructions.push(
            MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                moq_track_name: self.moq_media_track_name.clone(),
                object_payload,
                publisher_priority: track_priority_of(MoqTrackKind::Media(sample.medium())),
            },
        );
        Ok(instructions)
    }
}

/// The one group-cadence rule both containers obey, over every track kind.
///
/// A data object never cuts: it stands alone like an Opus packet does, and the
/// transport retains only a track's latest subgroup, so cutting on each one
/// would leave one object per group and lose all but the newest. A video-free
/// broadcast is cut by its two backstops instead.
fn this_sample_cuts_a_new_group_on_every_track(sample: &MoqTrackSample) -> bool {
    match sample {
        MoqTrackSample::EncodedMedia(sample) => {
            this_media_sample_cuts_a_new_group_on_every_track(sample)
        }
        MoqTrackSample::DataObject(_) => false,
    }
}

/// A group is a GOP across every track at once, so the cut is a video sync
/// point and nothing else. Every Opus packet is a sync point, and the transport
/// retains only a track's latest subgroup, so cutting on audio would leave one
/// packet per group and lose all but the newest.
fn this_media_sample_cuts_a_new_group_on_every_track(sample: &EncodedMediaSample) -> bool {
    match sample {
        EncodedMediaSample::VideoAccessUnit(unit) => unit.is_sync_point,
        EncodedMediaSample::AudioPacket(_) => false,
    }
}

/// What the catalog can honestly say about a `streamlib_bag` track.
///
/// The document is written before any track has delivered a bag, so no track's
/// medium is known yet — and a coded extent stated here would assert a `0x0`
/// video track for an Opus link. Every `streamlib_bag` object carries its own
/// codec and extent, which is where a subscriber reads them.
fn catalog_selection_parameters_of_a_streamlib_bag_track() -> MoqCatalogTrackSelectionParameters {
    MoqCatalogTrackSelectionParameters {
        codec_string: STREAMLIB_BAG_CATALOG_CODEC_STRING.to_owned(),
        bitrate_bits_per_second: None,
        coded_width: None,
        coded_height: None,
        sample_rate_hz: None,
        channel_count: None,
    }
}

/// One name per link, none empty, none twice.
fn refuse_track_names_that_do_not_name_the_links(
    track_names: &[String],
    inbound_link_count: usize,
) -> Result<()> {
    if track_names.len() != inbound_link_count {
        return Err(MoqExtensionError::Refused {
            what: format!(
                "`track_names` names {} track{} and `tracks` has {} inbound link{}; give one \
                 name per link, in the order the links were wired",
                track_names.len(),
                if track_names.len() == 1 { "" } else { "s" },
                inbound_link_count,
                if inbound_link_count == 1 { "" } else { "s" },
            ),
        });
    }
    let mut names_seen = HashSet::with_capacity(track_names.len());
    for track_name in track_names {
        if track_name.is_empty() {
            return Err(MoqExtensionError::Refused {
                what: "`track_names` carries an empty name, which names no track on the relay"
                    .to_owned(),
            });
        }
        if !names_seen.insert(track_name.as_str()) {
            return Err(MoqExtensionError::Refused {
                what: format!(
                    "`track_names` names `{track_name}` twice; two tracks of one broadcast \
                     cannot share a name"
                ),
            });
        }
    }
    Ok(())
}

/// The bytes one bag puts on the wire, whatever its kind.
fn payload_byte_count_of(sample: &MoqTrackSample) -> usize {
    match sample {
        MoqTrackSample::EncodedMedia(sample) => encoded_byte_count_of(sample),
        MoqTrackSample::DataObject(object) => object.envelope_bytes.len(),
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
    use crate::cmaf_init_segment_reader::read_cmaf_init_segment;
    use crate::encoded_media_sample::{EncodedAudioPacket, EncodedVideoAccessUnit};
    use crate::moq_session::{
        AUDIO_MEDIA_TRACK_PRIORITY, DATA_TRACK_PRIORITY, VIDEO_MEDIA_TRACK_PRIORITY,
    };
    use crate::moq_track_sample::DataTrackObject;

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

    /// A data object as Python hands one over: an envelope the engine's codec
    /// already encoded. The bytes are opaque here, so any bytes will do.
    fn a_data_object(envelope_bytes: &'static [u8]) -> MoqTrackSample {
        MoqTrackSample::DataObject(DataTrackObject {
            envelope_bytes: Bytes::from_static(envelope_bytes),
        })
    }

    fn a_planner_over(
        container_format: MoqContainerFormat,
        inbound_link_names: &[&str],
    ) -> MoqBroadcastObjectWritePlanner {
        a_planner_over_with_a_delivery_deadline_of(container_format, inbound_link_names, None)
    }

    fn a_planner_over_with_a_delivery_deadline_of(
        container_format: MoqContainerFormat,
        inbound_link_names: &[&str],
        delivery_deadline_ms: Option<u64>,
    ) -> MoqBroadcastObjectWritePlanner {
        let mut planner = MoqBroadcastObjectWritePlanner::of(
            container_format,
            BROADCAST_NAMESPACE.to_owned(),
            MoqPublisherDeliveryDeadline::of_optional_milliseconds(delivery_deadline_ms),
        );
        planner
            .declare_tracks(each_name_owned(inbound_link_names), None)
            .expect("these inbound links are declarable");
        planner
    }

    fn each_name_owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Plan one bag's writes and report every one of them written, which is
    /// what the publisher does when the transport accepted the whole plan.
    ///
    /// Planned at the instant it was stamped, so nothing is ever late: a test
    /// that means to exercise the delivery deadline states its own instant.
    fn plan_the_writes_and_report_them_all_written(
        planner: &mut MoqBroadcastObjectWritePlanner,
        inbound_link_name: &str,
        sample: EncodedMediaSample,
    ) -> Result<Vec<MoqObjectWriteInstruction>> {
        let stamped_at_ns = sample.timestamp_ns();
        plan_the_writes_at_and_report_them_all_written(
            planner,
            inbound_link_name,
            sample,
            stamped_at_ns,
        )
    }

    fn plan_the_writes_at_and_report_them_all_written(
        planner: &mut MoqBroadcastObjectWritePlanner,
        inbound_link_name: &str,
        sample: impl Into<MoqTrackSample>,
        now_ns: i64,
    ) -> Result<Vec<MoqObjectWriteInstruction>> {
        let planned = planner.plan_the_writes_for(inbound_link_name, sample.into(), now_ns)?;
        if planned.writing_them_all_opens_the_broadcast {
            planner.record_that_every_descriptive_object_and_held_sample_was_written();
        }
        Ok(planned.instructions)
    }

    /// Each instruction as `cut`, `only:<track>` or `object:<track>`, which is
    /// the whole of what reaches the transport and in what order.
    fn describe_each_write_instruction_as_a_transport_verb(
        instructions: &[MoqObjectWriteInstruction],
    ) -> Vec<String> {
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
                    ..
                } if moq_track_name == wanted_track_name => Some(object_payload.clone()),
                _ => None,
            })
            .collect()
    }

    /// The `publisher_priority` every group opened on a track is opened at.
    fn publisher_priorities_of_objects_written_to(
        instructions: &[MoqObjectWriteInstruction],
        wanted_track_name: &str,
    ) -> Vec<u8> {
        instructions
            .iter()
            .filter_map(|instruction| match instruction {
                MoqObjectWriteInstruction::AppendOneObjectToATracksOpenGroup {
                    moq_track_name,
                    publisher_priority,
                    ..
                } if moq_track_name == wanted_track_name => Some(*publisher_priority),
                _ => None,
            })
            .collect()
    }

    fn the_only_object_written_to(
        instructions: &[MoqObjectWriteInstruction],
        wanted_track_name: &str,
    ) -> Option<Bytes> {
        instructions
            .iter()
            .find_map(|instruction| match instruction {
                MoqObjectWriteInstruction::WriteTheOnlyObjectATrackEverCarries {
                    moq_track_name,
                    object_payload,
                } if moq_track_name == wanted_track_name => Some(object_payload.clone()),
                _ => None,
            })
    }

    fn catalog_tracks_in(catalog_object: &Bytes) -> Vec<serde_json::Value> {
        let catalog: serde_json::Value =
            serde_json::from_slice(catalog_object).expect("the catalog is JSON");
        catalog["tracks"]
            .as_array()
            .expect("the catalog names its tracks")
            .clone()
    }

    fn catalog_track_names_in(catalog_object: &Bytes) -> Vec<String> {
        catalog_tracks_in(catalog_object)
            .iter()
            .map(|track| {
                track["name"]
                    .as_str()
                    .expect("a track has a name")
                    .to_owned()
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

    /// What one bag put on the wire, counted from the bag itself rather than
    /// from the hold's own accounting.
    fn annex_b_byte_count_of(sample: &EncodedMediaSample) -> usize {
        match sample {
            EncodedMediaSample::VideoAccessUnit(unit) => unit.annex_b_access_unit.len(),
            EncodedMediaSample::AudioPacket(packet) => packet.opus_packet.len(),
        }
    }

    fn refusal_of(failure: MoqExtensionError) -> String {
        assert!(
            matches!(failure, MoqExtensionError::Refused { .. }),
            "the caller passed something wrong, so this is a refusal: {failure}"
        );
        failure.to_string()
    }

    /// Feed one track bags of eight mebibytes until the hold stops, handing
    /// back the refusal and how many bags the track delivered in all.
    fn fill_the_hold_until_it_stops(
        planner: &mut MoqBroadcastObjectWritePlanner,
        inbound_link_name: &str,
    ) -> (String, u64) {
        // One allocation shared by every bag: `Bytes` is refcounted, so the
        // hold's accounting sees eight mebibytes per bag while the test holds
        // eight mebibytes in total.
        let eight_mebibytes = Bytes::from(vec![0x00_u8; 8 * 1024 * 1024]);
        let mut bags_delivered = 0u64;
        loop {
            let stamp_ns = 33_000_000 * (bags_delivered as i64 + 1);
            let planned = plan_the_writes_and_report_them_all_written(
                planner,
                inbound_link_name,
                video_access_unit(eight_mebibytes.clone(), false, stamp_ns),
            );
            bags_delivered += 1;
            match planned {
                Ok(nothing_written) => {
                    assert!(nothing_written.is_empty());
                    assert!(
                        bags_delivered < 64,
                        "the hold accepted {bags_delivered} bags of eight mebibytes without \
                         ever refusing"
                    );
                }
                Err(failure) => break (refusal_of(failure), bags_delivered),
            }
        }
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
    fn each_container_this_wheel_publishes_is_reachable_by_its_config_spelling() {
        assert_eq!(
            MoqContainerFormat::of_wire_name("cmaf").expect("cmaf is published"),
            MoqContainerFormat::Cmaf
        );
        assert_eq!(
            MoqContainerFormat::of_wire_name("streamlib_bag").expect("streamlib_bag is published"),
            MoqContainerFormat::StreamlibBag
        );
    }

    #[test]
    fn a_plan_that_is_refused_gives_back_the_fragment_numbering_it_spent() {
        // Planning spends a sequence number and advances the track's timeline
        // before a byte reaches the relay. A plan that is refused must give
        // both back, or the broadcast's `mfhd.sequence_number` skips and the
        // next sample's duration is measured against a stamp no subscriber
        // ever saw.
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera"]);
        planner
            .plan_the_writes_for("camera", a_video_sync_point(0).into(), 0)
            .expect("the first sync point describes the track");
        let spent_before = planner.what_planning_spends();

        let refused = planner.plan_the_writes_for(
            "camera",
            a_video_sync_point_carrying(&A_SECOND_H264_SEQUENCE_PARAMETER_SET, 33_000_000).into(),
            33_000_000,
        );
        assert!(refused.is_err(), "drifted parameter sets are refused");
        planner.give_back_what_planning_spent(spent_before.clone());

        assert_eq!(planner.what_planning_spends(), spent_before);
    }

    #[test]
    fn declaring_a_broadcasts_tracks_a_second_time_is_refused_and_names_the_links_already_declared()
    {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera"]);

        let refusal = refusal_of(
            planner
                .declare_tracks(vec!["microphone".to_owned()], None)
                .expect_err("the tracks are already fixed"),
        );

        assert!(refusal.contains("camera"), "{refusal}");
    }

    #[test]
    fn declaring_a_broadcast_carrying_no_inbound_links_at_all_is_refused() {
        let mut planner = MoqBroadcastObjectWritePlanner::of(
            MoqContainerFormat::Cmaf,
            BROADCAST_NAMESPACE.to_owned(),
            MoqPublisherDeliveryDeadline::of_optional_milliseconds(None),
        );

        let refusal = refusal_of(
            planner
                .declare_tracks(Vec::new(), None)
                .expect_err("a broadcast with no tracks publishes nothing"),
        );

        assert!(refusal.contains("at least one"), "{refusal}");
    }

    #[test]
    fn declaring_the_same_inbound_link_twice_is_refused_by_name() {
        let mut planner = MoqBroadcastObjectWritePlanner::of(
            MoqContainerFormat::Cmaf,
            BROADCAST_NAMESPACE.to_owned(),
            MoqPublisherDeliveryDeadline::of_optional_milliseconds(None),
        );

        let refusal = refusal_of(
            planner
                .declare_tracks(
                    vec![
                        "camera".to_owned(),
                        "microphone".to_owned(),
                        "camera".to_owned(),
                    ],
                    None,
                )
                .expect_err("two tracks cannot share a name"),
        );

        assert!(refusal.contains("camera"), "{refusal}");
    }

    #[test]
    fn a_streamlib_bag_link_that_would_take_the_catalog_tracks_name_is_refused_by_name() {
        let mut planner = MoqBroadcastObjectWritePlanner::of(
            MoqContainerFormat::StreamlibBag,
            BROADCAST_NAMESPACE.to_owned(),
            MoqPublisherDeliveryDeadline::of_optional_milliseconds(None),
        );

        let refusal = refusal_of(
            planner
                .declare_tracks(vec![CATALOG_TRACK_NAME.to_owned()], None)
                .expect_err("that name is already the catalog track"),
        );

        assert!(refusal.contains(CATALOG_TRACK_NAME), "{refusal}");
    }

    #[test]
    fn publishing_from_a_link_this_broadcast_does_not_carry_is_refused_and_names_the_links_it_does()
    {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);

        let refusal = refusal_of(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
                "screen",
                a_video_sync_point(0),
            )
            .expect_err("`screen` is not a link of this broadcast"),
        );

        assert!(refusal.contains("screen"), "{refusal}");
        assert!(refusal.contains("camera"), "{refusal}");
        assert!(refusal.contains("microphone"), "{refusal}");
    }

    #[test]
    fn a_video_sync_point_cuts_a_new_group_on_every_track_before_it_is_written() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["camera"]);

        let first_sync_point = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
        )
        .expect("a sync point publishes");
        let delta_frame = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
        )
        .expect("a delta frame publishes");
        let second_sync_point = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(66_000_000),
        )
        .expect("the next sync point publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&first_sync_point),
            vec!["only:.catalog", "cut", "object:camera"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&delta_frame),
            vec!["object:camera"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&second_sync_point),
            vec!["cut", "object:camera"]
        );
    }

    #[test]
    fn an_opus_packet_never_cuts_a_group_even_though_every_opus_packet_is_a_sync_point() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["microphone"]);

        let first = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("an opus packet publishes");
        let second = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(20_000_000),
        )
        .expect("the next opus packet publishes");
        let third = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(40_000_000),
        )
        .expect("the next opus packet publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&first),
            vec!["only:.catalog", "object:microphone"],
            "the catalog is written once, before the first media object"
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&second),
            vec!["object:microphone"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&third),
            vec!["object:microphone"]
        );
    }

    #[test]
    fn no_cmaf_object_is_written_until_every_declared_track_has_been_described() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);

        for (frame_index, stamp_ns) in [0, 33_000_000, 66_000_000].into_iter().enumerate() {
            let planned = plan_the_writes_and_report_them_all_written(
                &mut planner,
                "camera",
                a_video_sync_point(stamp_ns),
            )
            .expect("video publishes");
            assert!(
                planned.is_empty(),
                "video frame {frame_index} was written before the audio track was described: {:?}",
                describe_each_write_instruction_as_a_transport_verb(&planned)
            );
        }

        let once_the_audio_track_spoke = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("audio publishes");

        assert!(!once_the_audio_track_spoke.is_empty());
    }

    #[test]
    fn the_cmaf_init_object_is_written_before_the_catalog() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("video publishes");

        let planned = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("audio publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&planned)[..2],
            ["only:0.mp4".to_owned(), "only:.catalog".to_owned()]
        );
    }

    #[test]
    fn the_bags_held_while_a_track_was_silent_are_published_in_arrival_order_once_it_speaks() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        for stamp_ns in [0, 33_000_000, 66_000_000] {
            let held = plan_the_writes_and_report_them_all_written(
                &mut planner,
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

        let planned = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("audio publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&planned),
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
    fn an_opus_fragment_states_the_packets_sample_count_rather_than_the_gap_between_arrivals() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(9_000_000_000),
        )
        .expect("the video track is described");

        // Stamps a live capture carries: 20 ms apart in intent, never in fact,
        // so no gap between two of them is the packets' own 960 samples.
        let mut durations = Vec::new();
        let mut decode_times = Vec::new();
        for stamp_ns in [9_000_000_000i64, 9_020_500_000, 9_039_500_000] {
            let planned = plan_the_writes_and_report_them_all_written(
                &mut planner,
                "microphone",
                an_opus_packet(stamp_ns),
            )
            .expect("audio publishes");
            for fragment in object_payloads_written_to(&planned, "2.m4s") {
                let samples = read_cmaf_fragment(&fragment).expect("the fragment reads back");
                durations.push(samples[0].duration);
                decode_times.push(samples[0].decode_time);
            }
        }

        assert_eq!(
            durations,
            vec![960, 960, 960],
            "RFC 8486 §4.1 makes an Opus sample's duration its decoded sample count, which the \
             bag states; the arrival gaps here are 20.5 ms and 19 ms"
        );
        assert_eq!(
            decode_times,
            vec![0, 984, 1896],
            "`tfdt` still follows the stamps, so a capture gap stays visible in the decode times"
        );
    }

    #[test]
    fn a_cmaf_plan_the_transport_never_wrote_still_owes_the_init_object_the_catalog_and_the_hold() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        planner
            .plan_the_writes_for("camera", a_video_sync_point(0).into(), 0)
            .expect("the video track is held until the audio track speaks");

        let never_reached_the_transport = planner
            .plan_the_writes_for("microphone", an_opus_packet(0).into(), 0)
            .expect("the audio track describes itself and the broadcast can open");
        assert!(never_reached_the_transport.writing_them_all_opens_the_broadcast);

        let planned_again = planner
            .plan_the_writes_for("microphone", an_opus_packet(20_000_000).into(), 20_000_000)
            .expect("the next bag plans");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&planned_again.instructions),
            vec![
                "only:0.mp4",
                "only:.catalog",
                "cut",
                "object:1.m4s",
                "object:2.m4s",
            ],
            "a plan the session never executed leaves the descriptive objects and the held bag \
             still to write"
        );
    }

    #[test]
    fn a_streamlib_bag_plan_the_transport_never_wrote_still_owes_the_catalog() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["camera"]);
        let never_reached_the_transport = planner
            .plan_the_writes_for("camera", a_video_sync_point(0).into(), 0)
            .expect("a sync point plans");
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(
                &never_reached_the_transport.instructions
            ),
            vec!["only:.catalog", "cut", "object:camera"]
        );

        let planned_again = planner
            .plan_the_writes_for("camera", a_video_sync_point(33_000_000).into(), 33_000_000)
            .expect("the next sync point plans");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&planned_again.instructions),
            vec!["only:.catalog", "cut", "object:camera"],
            "a catalog no session ever wrote is still owed"
        );
    }

    #[test]
    fn a_broadcast_still_opens_after_a_sync_point_it_could_not_describe_was_refused_mid_hold() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        assert!(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
                "camera",
                a_video_sync_point(0)
            )
            .expect("the first sync point describes the video track")
            .is_empty()
        );

        let refusal = refusal_of(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
                "camera",
                a_video_sync_point_carrying(&A_SECOND_H264_SEQUENCE_PARAMETER_SET, 33_000_000),
            )
            .expect_err("the init segment this broadcast owes states the first sets"),
        );
        assert!(refusal.contains("parameter sets"), "{refusal}");

        let planned = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("the audio track describes itself and the broadcast opens");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&planned),
            vec![
                "only:0.mp4",
                "only:.catalog",
                "cut",
                "object:1.m4s",
                "object:2.m4s",
            ],
            "the refused bag cost the broadcast nothing: the init object, the catalog and the \
             bag the hold kept all still reach the transport"
        );
    }

    #[test]
    fn a_sync_point_whose_parameter_sets_drift_is_refused_while_its_track_is_still_held() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("the first sync point describes the video track");

        let refusal = refusal_of(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
                "camera",
                a_video_sync_point_carrying(&A_SECOND_H264_SEQUENCE_PARAMETER_SET, 33_000_000),
            )
            .expect_err("a drifted sync point is refused where it arrives"),
        );

        assert!(refusal.contains("camera"), "{refusal}");
        assert!(refusal.contains("parameter sets"), "{refusal}");
    }

    #[test]
    fn holding_for_a_track_that_never_speaks_is_bounded_and_the_refusal_names_the_silent_track() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("the video track describes itself");

        let (refusal, _) = fill_the_hold_until_it_stops(&mut planner, "camera");

        assert!(
            refusal.contains("`microphone` has delivered no bag at all"),
            "the refusal names the track that never spoke as silent: {refusal}"
        );
        assert!(
            !refusal.contains("camera"),
            "the track that did speak is not named as undescribed: {refusal}"
        );
    }

    #[test]
    fn a_track_delivering_bags_without_a_sync_point_is_named_by_its_count_rather_than_as_silent() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);

        let (refusal, bags_the_camera_delivered) =
            fill_the_hold_until_it_stops(&mut planner, "camera");

        assert!(
            refusal.contains(&format!(
                "`camera` has delivered {bags_the_camera_delivered} bags and no sync point"
            )),
            "an encoder that has never emitted a keyframe is named as such: {refusal}"
        );
        assert!(
            refusal.contains("`microphone` has delivered no bag at all"),
            "a link nothing ever wrote to is named as silent: {refusal}"
        );
    }

    #[test]
    fn once_the_hold_stops_at_its_bound_every_later_bag_is_refused_however_small() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("the video track describes itself");
        let (_, bags_the_camera_delivered) = fill_the_hold_until_it_stops(&mut planner, "camera");

        let a_small_bag_after_the_hold_stopped = refusal_of(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
                "camera",
                a_video_delta_frame(33_000_000 * (bags_the_camera_delivered as i64 + 2)),
            )
            .expect_err("a stopped hold refuses a bag that would have fitted"),
        );
        let the_silent_track_finally_speaking = refusal_of(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
                "microphone",
                an_opus_packet(0),
            )
            .expect_err("a broadcast with a gap in its hold can never be published whole"),
        );

        assert!(
            a_small_bag_after_the_hold_stopped.contains("held"),
            "{a_small_bag_after_the_hold_stopped}"
        );
        assert!(
            the_silent_track_finally_speaking.contains("held"),
            "{the_silent_track_finally_speaking}"
        );
    }

    #[test]
    fn closing_a_broadcast_that_never_became_playable_reports_the_encoded_media_it_discards() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        let first_bag = a_video_sync_point(0);
        let second_bag = a_video_delta_frame(33_000_000);
        let bytes_the_two_bags_carry =
            annex_b_byte_count_of(&first_bag) + annex_b_byte_count_of(&second_bag);
        for bag in [first_bag, second_bag] {
            plan_the_writes_and_report_them_all_written(&mut planner, "camera", bag)
                .expect("video is held while the audio track is silent");
        }

        let discarded = planner
            .the_encoded_media_the_hold_discards_at_close()
            .expect("two bags are still held");

        assert_eq!(discarded.held_sample_count, 2);
        assert_eq!(discarded.held_byte_count, bytes_the_two_bags_carry);
        assert!(
            discarded
                .why_the_broadcast_never_opened
                .contains("microphone"),
            "{}",
            discarded.why_the_broadcast_never_opened
        );
    }

    #[test]
    fn media_held_when_a_data_link_speaks_is_reported_for_what_it_is_if_the_broadcast_closes_first()
    {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "telemetry"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("video is held while the third link has not said what it is");
        planner
            .plan_the_writes_for("telemetry", a_data_object(A_DATA_ENVELOPE), 0)
            .expect_err("cmaf has no packaging for a data bag");

        let discarded = planner
            .the_encoded_media_the_hold_discards_at_close()
            .expect("the held bag is discarded: no media bag came to open the broadcast");

        assert_eq!(discarded.held_sample_count, 1);
        assert!(
            discarded
                .why_the_broadcast_never_opened
                .contains("telemetry")
                && discarded.why_the_broadcast_never_opened.contains("data"),
            "{}",
            discarded.why_the_broadcast_never_opened
        );
        assert!(
            !discarded
                .why_the_broadcast_never_opened
                .contains("no track"),
            "{}",
            discarded.why_the_broadcast_never_opened
        );
    }

    #[test]
    fn a_broadcast_that_wrote_everything_it_held_discards_nothing_when_it_closes() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("the one track describes itself and the broadcast opens");

        assert_eq!(planner.the_encoded_media_the_hold_discards_at_close(), None);
    }

    #[test]
    fn the_streamlib_bag_catalog_claims_no_coded_extent_for_a_track_whose_medium_is_unknown() {
        let mut planner =
            a_planner_over(MoqContainerFormat::StreamlibBag, &["camera", "microphone"]);

        let planned = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("an opus packet publishes");

        let catalog_object = the_only_object_written_to(&planned, CATALOG_TRACK_NAME)
            .expect("the catalog is written before the first media object");
        for track in &catalog_tracks_in(&catalog_object) {
            let selection_parameters = &track["selectionParams"];
            assert!(
                selection_parameters.get("width").is_none()
                    && selection_parameters.get("height").is_none(),
                "an audio link is described as a 0x0 video track: {selection_parameters}"
            );
        }
    }

    #[test]
    fn a_cmaf_video_sample_carries_no_parameter_sets_once_the_init_segment_states_them() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera"]);

        let planned = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
        )
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
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("the first sync point describes the track");

        let refusal = refusal_of(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
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
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("the first track describes itself");
        let both_described = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "screen",
            a_video_sync_point(0),
        )
        .expect("the second track describes itself and the broadcast opens");
        let camera_second = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
        )
        .expect("the first track publishes on");
        let screen_second = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "screen",
            a_video_delta_frame(33_000_000),
        )
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
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("h264 publishes");

        let refusal = refusal_of(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
                "camera",
                an_opus_packet(33_000_000),
            )
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
            MoqPublisherDeliveryDeadline::of_optional_milliseconds(None),
        );

        publisher
            .declare_tracks(vec!["camera".to_owned(), "microphone".to_owned()], None)
            .expect("the links are declarable");

        assert!(
            !publisher.is_connected(),
            "declaring tracks must not spend a relay round trip inside `setup()`"
        );
    }

    #[test]
    fn a_delivery_deadline_that_has_not_passed_sheds_nothing() {
        let mut planner = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::StreamlibBag,
            &["camera"],
            Some(100),
        );

        let opened = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
            0,
        )
        .expect("the first sync point plans");
        let inside_the_deadline = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
            33_000_000 + 99_000_000,
        )
        .expect("a frame inside the deadline plans");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&opened),
            vec!["only:.catalog", "cut", "object:camera"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&inside_the_deadline),
            vec!["object:camera"]
        );
        assert_eq!(planner.objects_the_delivery_deadline_shed(), vec![]);
    }

    #[test]
    fn a_shed_bag_and_a_held_bag_both_write_nothing_but_only_one_of_them_says_it_was_shed() {
        let mut held = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::Cmaf,
            &["camera", "microphone"],
            Some(100),
        );
        let the_hold_keeps_it = held
            .plan_the_writes_for("camera", a_video_sync_point(0).into(), 0)
            .expect("the video track is held until the audio track speaks");

        let mut shed = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::StreamlibBag,
            &["camera"],
            Some(100),
        );
        shed.plan_the_writes_for("camera", a_video_sync_point(0).into(), 0)
            .expect("the first sync point plans");
        let the_deadline_sheds_it = shed
            .plan_the_writes_for(
                "camera",
                a_video_delta_frame(33_000_000).into(),
                33_000_000 + 100_000_001,
            )
            .expect("a shed frame is not a refusal");

        assert!(the_hold_keeps_it.instructions.is_empty());
        assert!(!the_hold_keeps_it.the_delivery_deadline_shed_it);
        assert!(the_deadline_sheds_it.instructions.is_empty());
        assert!(
            the_deadline_sheds_it.the_delivery_deadline_shed_it,
            "the caller counts a held bag as published and a shed bag as not, so the plan has \
             to tell them apart"
        );
    }

    #[test]
    fn a_frame_past_the_delivery_deadline_asks_the_transport_for_nothing_at_all() {
        let mut planner = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::StreamlibBag,
            &["camera"],
            Some(100),
        );
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
            0,
        )
        .expect("the first sync point plans");

        let shed = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
            33_000_000 + 100_000_001,
        )
        .expect("a shed frame is not a refusal");

        // An empty instruction list is what "never mid-object" means here: the
        // object is not created, so no header promising a payload length ever
        // reaches a QUIC stream.
        assert_eq!(shed, vec![]);
    }

    #[test]
    fn the_rest_of_a_group_goes_with_the_frame_the_deadline_shed() {
        let mut planner = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::StreamlibBag,
            &["camera"],
            Some(100),
        );
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
            0,
        )
        .expect("the first sync point plans");
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
            33_000_000 + 100_000_001,
        )
        .expect("the late frame is shed");

        // On time by its own stamp, and still shed: a decoder cannot use a
        // frame whose reference was shed, so its bytes would buy nothing.
        let after_the_shed = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(66_000_000),
            66_000_000,
        )
        .expect("a frame in a shed group is not a refusal");

        assert_eq!(after_the_shed, vec![]);
    }

    #[test]
    fn a_sync_point_is_published_however_late_it_is_and_ends_the_shed() {
        let mut planner = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::StreamlibBag,
            &["camera"],
            Some(100),
        );
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
            0,
        )
        .expect("the first sync point plans");
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
            33_000_000 + 100_000_001,
        )
        .expect("the late frame is shed");

        let a_very_late_sync_point = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(66_000_000),
            66_000_000 + 60_000_000_000,
        )
        .expect("a sync point plans however late it is");
        let after_the_sync_point = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(99_000_000),
            99_000_000,
        )
        .expect("the frame after a sync point plans");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&a_very_late_sync_point),
            vec!["cut", "object:camera"],
            "abandoning the group a decoder re-enters at turns one late frame into a stall"
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&after_the_sync_point),
            vec!["object:camera"],
            "the shed ends at the sync point, which is also what opened the new group"
        );
    }

    #[test]
    fn an_opus_packet_is_never_shed_however_late_it_is_and_a_video_shed_beside_it_is_unaffected() {
        let mut planner = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::StreamlibBag,
            &["camera", "microphone"],
            Some(100),
        );
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
            0,
        )
        .expect("the first sync point plans");
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
            33_000_000 + 100_000_001,
        )
        .expect("the late frame is shed");

        let a_very_late_packet = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(20_000_000),
            20_000_000 + 60_000_000_000,
        )
        .expect("an Opus packet plans however late it is");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&a_very_late_packet),
            vec!["object:microphone"],
            "every Opus packet is a sync point, which is what makes audio outrank video here"
        );
        assert_eq!(
            planner
                .objects_the_delivery_deadline_shed()
                .into_iter()
                .map(|track| track.inbound_link_name)
                .collect::<Vec<_>>(),
            vec!["camera"],
            "shedding is per track, and the audio track shed nothing"
        );
    }

    #[test]
    fn the_shed_counts_name_the_inbound_link_and_match_the_bytes_that_were_never_written() {
        let mut planner = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::StreamlibBag,
            &["camera"],
            Some(100),
        );
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
            0,
        )
        .expect("the first sync point plans");
        let a_shed_frame = a_video_delta_frame(33_000_000);
        let one_frames_bytes = annex_b_byte_count_of(&a_shed_frame) as u64;
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_shed_frame,
            33_000_000 + 100_000_001,
        )
        .expect("the late frame is shed");
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(66_000_000),
            66_000_000,
        )
        .expect("the rest of the group goes with it");

        assert_eq!(
            planner.objects_the_delivery_deadline_shed(),
            vec![ObjectsTheDeliveryDeadlineShedOnOneTrack {
                inbound_link_name: "camera".to_owned(),
                objects_shed: 2,
                bytes_shed: one_frames_bytes * 2,
            }]
        );
    }

    #[test]
    fn a_publisher_with_no_delivery_deadline_writes_every_bag_however_late() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["camera"]);
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
            0,
        )
        .expect("the first sync point plans");

        let a_minute_late = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
            33_000_000 + 60_000_000_000,
        )
        .expect("an unconfigured deadline refuses nothing");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&a_minute_late),
            vec!["object:camera"]
        );
        assert_eq!(planner.objects_the_delivery_deadline_shed(), vec![]);
    }

    #[test]
    fn a_bag_the_deadline_shed_spends_no_cmaf_fragment_sequence_number() {
        let mut planner = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::Cmaf,
            &["camera"],
            Some(100),
        );
        let opened = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
            0,
        )
        .expect("the first sync point opens the broadcast");
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
            33_000_000 + 100_000_001,
        )
        .expect("the late frame is shed");
        let after_the_shed = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(66_000_000),
            66_000_000,
        )
        .expect("the next sync point plans");

        let sequence_numbers: Vec<u32> = [&opened, &after_the_shed]
            .into_iter()
            .flat_map(|instructions| object_payloads_written_to(instructions, "1.m4s"))
            .map(|fragment| cmaf_fragment_sequence_number(&fragment))
            .collect();

        assert_eq!(
            sequence_numbers,
            vec![1, 2],
            "ISO/IEC 14496-12 §8.8.5 numbers a track's fragments consecutively, so a bag that              was never written must not spend one"
        );
    }

    #[test]
    fn each_medium_opens_its_groups_at_its_own_rung_with_audio_ahead_of_video() {
        let mut planner =
            a_planner_over(MoqContainerFormat::StreamlibBag, &["camera", "microphone"]);
        let video = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
        )
        .expect("the video sync point plans");
        let audio = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(20_000_000),
        )
        .expect("the Opus packet plans");

        assert_eq!(
            publisher_priorities_of_objects_written_to(&video, "camera"),
            vec![VIDEO_MEDIA_TRACK_PRIORITY]
        );
        assert_eq!(
            publisher_priorities_of_objects_written_to(&audio, "microphone"),
            vec![AUDIO_MEDIA_TRACK_PRIORITY]
        );
    }

    #[test]
    fn a_cmaf_broadcasts_two_media_tracks_open_their_groups_at_their_own_mediums_rung() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "microphone"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("the video track is held until the audio track speaks");
        let opened = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("the audio track describes itself and the broadcast opens");

        assert_eq!(
            publisher_priorities_of_objects_written_to(&opened, "1.m4s"),
            vec![VIDEO_MEDIA_TRACK_PRIORITY]
        );
        assert_eq!(
            publisher_priorities_of_objects_written_to(&opened, "2.m4s"),
            vec![AUDIO_MEDIA_TRACK_PRIORITY]
        );
    }

    /// The envelope's bytes are opaque to the planner, so any bytes stand in.
    const A_DATA_ENVELOPE: &[u8] = b"\x81\xa3bag\x80";

    #[test]
    fn a_data_object_never_cuts_a_group_however_many_are_written() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["telemetry"]);

        let first = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("a data object publishes");
        let second = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            10_000_000,
        )
        .expect("the next data object publishes");
        let third = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            20_000_000,
        )
        .expect("the next data object publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&first),
            vec!["only:.catalog", "object:telemetry"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&second),
            vec!["object:telemetry"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&third),
            vec!["object:telemetry"]
        );
    }

    #[test]
    fn a_data_objects_payload_is_the_bytes_python_handed_over_unparsed() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["telemetry"]);

        let planned = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("a data object publishes");

        assert_eq!(
            object_payloads_written_to(&planned, "telemetry"),
            vec![Bytes::from_static(A_DATA_ENVELOPE)]
        );
    }

    #[test]
    fn a_data_track_opens_its_groups_at_the_data_rung() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["telemetry"]);

        let planned = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("a data object publishes");

        assert_eq!(
            publisher_priorities_of_objects_written_to(&planned, "telemetry"),
            vec![DATA_TRACK_PRIORITY]
        );
    }

    #[test]
    fn a_video_free_broadcast_cuts_a_group_once_its_open_group_is_older_than_the_bound() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["telemetry"]);
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("the first data object opens the group");

        let at_the_bound = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS,
        )
        .expect("a data object at the bound publishes");
        let past_the_bound = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS + 1,
        )
        .expect("a data object past the bound publishes");
        let just_after_the_cut = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS + 2,
        )
        .expect("a data object just after the cut publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&at_the_bound),
            vec!["object:telemetry"],
            "a group exactly as old as the bound is not older than it"
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&past_the_bound),
            vec!["cut", "object:telemetry"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&just_after_the_cut),
            vec!["object:telemetry"],
            "the cut is what the next age is measured from"
        );
    }

    #[test]
    fn an_audio_only_broadcast_is_cut_by_the_same_age_bound() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["microphone"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "microphone", an_opus_packet(0))
            .expect("the first packet opens the group");

        let past_the_bound = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(20_000_000),
            LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS + 20_000_000,
        )
        .expect("a packet past the bound publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&past_the_bound),
            vec!["cut", "object:microphone"]
        );
    }

    #[test]
    fn a_video_free_cmaf_broadcast_is_cut_by_the_age_bound_once_it_has_opened() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["microphone"]);
        let opened = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("the first packet describes the track and opens the broadcast");

        let past_the_bound = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(20_000_000),
            LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS + 20_000_000,
        )
        .expect("a packet past the bound publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&opened),
            vec!["only:0.mp4", "only:.catalog", "object:1.m4s"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&past_the_bound),
            vec!["cut", "object:1.m4s"]
        );
    }

    #[test]
    fn a_broadcast_with_video_never_reaches_the_age_bound() {
        let mut planner = a_planner_over(
            MoqContainerFormat::StreamlibBag,
            &["camera", "microphone", "telemetry"],
        );
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("the sync point opens the group");

        let audio_seconds_later = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(5_000_000_000),
        )
        .expect("audio publishes");
        let data_seconds_later = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            6_000_000_000,
        )
        .expect("data publishes");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&audio_seconds_later),
            vec!["object:microphone"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&data_seconds_later),
            vec!["object:telemetry"]
        );
    }

    #[test]
    fn a_plan_the_transport_never_wrote_gives_back_the_open_groups_age() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["telemetry"]);
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("the first data object opens the group");
        let spent_before = planner.what_planning_spends();

        let never_written = planner
            .plan_the_writes_for(
                "telemetry",
                a_data_object(A_DATA_ENVELOPE),
                LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS + 1,
            )
            .expect("a data object past the bound plans a cut");
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&never_written.instructions),
            vec!["cut", "object:telemetry"]
        );
        planner.give_back_what_planning_spent(spent_before);

        let retried = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            LONGEST_OPEN_GROUP_AGE_ON_A_VIDEO_FREE_BROADCAST_NS + 2,
        )
        .expect("the retry plans");
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&retried),
            vec!["cut", "object:telemetry"],
            "a cut the transport never made must not count as the group's opening"
        );
    }

    #[test]
    fn a_data_object_is_never_shed_by_the_delivery_deadline_however_late_the_reading_is() {
        let mut planner = a_planner_over_with_a_delivery_deadline_of(
            MoqContainerFormat::StreamlibBag,
            &["telemetry"],
            Some(100),
        );
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("the first data object publishes");

        let an_hour_later = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            3_600_000_000_000,
        )
        .expect("a data object is not a refusal however late");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&an_hour_later),
            vec!["cut", "object:telemetry"]
        );
        assert_eq!(planner.objects_the_delivery_deadline_shed(), vec![]);
    }

    #[test]
    fn a_data_bag_under_cmaf_is_refused_by_name_before_any_hold() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "telemetry"]);

        let refusal = refusal_of(
            planner
                .plan_the_writes_for("telemetry", a_data_object(A_DATA_ENVELOPE), 0)
                .expect_err("cmaf has no packaging for a data bag"),
        );

        assert!(refusal.contains("telemetry"), "{refusal}");
        assert!(refusal.contains("bitstream"), "{refusal}");
        assert!(refusal.contains("streamlib_bag"), "{refusal}");
        assert_eq!(planner.bytes_held_until_every_cmaf_track_is_described, 0);
        assert!(
            planner
                .samples_held_until_every_cmaf_track_is_described
                .is_empty()
        );
        assert!(!planner.the_hold_stopped_at_its_bound);
    }

    #[test]
    fn a_cmaf_broadcast_opens_without_a_data_link_once_that_link_has_shown_what_it_carries() {
        let mut planner = a_planner_over(
            MoqContainerFormat::Cmaf,
            &["camera", "microphone", "telemetry"],
        );
        let held_video = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
        )
        .expect("video is held until every describable track has spoken");
        let held_audio = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "microphone",
            an_opus_packet(0),
        )
        .expect("audio is held too: the third link has not yet said what it is");
        assert!(held_video.is_empty());
        assert!(held_audio.is_empty());

        let refusal = refusal_of(
            planner
                .plan_the_writes_for("telemetry", a_data_object(A_DATA_ENVELOPE), 0)
                .expect_err("cmaf has no packaging for a data bag"),
        );
        let opened = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_delta_frame(33_000_000),
        )
        .expect("the next media bag opens the broadcast without the data link");

        assert!(refusal.contains("telemetry"), "{refusal}");
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&opened),
            vec![
                "only:0.mp4",
                "only:.catalog",
                "cut",
                "object:1.m4s",
                "object:2.m4s",
                "object:1.m4s",
            ],
            "the media held for the data link must reach the transport once the link is known"
        );
        let catalog_object = the_only_object_written_to(&opened, CATALOG_TRACK_NAME)
            .expect("the catalog is written with the init object");
        assert_eq!(
            catalog_track_names_in(&catalog_object),
            vec!["1.m4s", "2.m4s"],
            "the catalog describes the two media tracks and not the data link"
        );
        assert_eq!(planner.bytes_held_until_every_cmaf_track_is_described, 0);
        assert_eq!(
            planner.describe_the_tracks_the_init_segment_is_waiting_on(),
            "no track"
        );
    }

    #[test]
    fn a_data_link_refused_first_under_cmaf_never_holds_the_media_declared_beside_it() {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["camera", "telemetry"]);
        planner
            .plan_the_writes_for("telemetry", a_data_object(A_DATA_ENVELOPE), 0)
            .expect_err("cmaf has no packaging for a data bag");

        let opened = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
        )
        .expect("the one describable track describes itself and the broadcast opens");
        let later = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(33_000_000),
        )
        .expect("the video track publishes on");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&opened),
            vec!["only:0.mp4", "only:.catalog", "cut", "object:1.m4s"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&later),
            vec!["cut", "object:1.m4s"]
        );
        assert!(
            planner
                .samples_held_until_every_cmaf_track_is_described
                .is_empty()
        );
    }

    #[test]
    fn a_data_link_declared_first_under_cmaf_keeps_its_slot_and_the_media_stay_aligned_with_the_moov()
     {
        let mut planner = a_planner_over(MoqContainerFormat::Cmaf, &["telemetry", "camera"]);
        planner
            .plan_the_writes_for("telemetry", a_data_object(A_DATA_ENVELOPE), 0)
            .expect_err("cmaf has no packaging for a data bag");

        let opened = plan_the_writes_and_report_them_all_written(
            &mut planner,
            "camera",
            a_video_sync_point(0),
        )
        .expect("the one describable track describes itself and the broadcast opens");

        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&opened),
            vec!["only:0.mp4", "only:.catalog", "cut", "object:2.m4s"]
        );
        let catalog_object = the_only_object_written_to(&opened, CATALOG_TRACK_NAME)
            .expect("the catalog is written with the init object");
        assert_eq!(catalog_track_names_in(&catalog_object), vec!["2.m4s"]);
        let init_object = the_only_object_written_to(&opened, INIT_TRACK_NAME)
            .expect("the init object is written");
        let trak_ids: Vec<u32> = read_cmaf_init_segment(&init_object)
            .expect("the init segment reads back")
            .iter()
            .map(|description| description.track_id)
            .collect();
        assert_eq!(
            trak_ids,
            vec![2],
            "the moov's one trak carries the id the catalog names, so a subscriber zipping \
             the two positionally lands on the video"
        );
        assert_eq!(
            planner.moq_track_names(),
            vec![".catalog", "0.mp4", "1.m4s", "2.m4s"],
            "the data link's slot stays announced and empty: names are fixed at connect, \
             before any link has said what it carries"
        );
    }

    #[test]
    fn a_media_bag_on_a_link_that_first_published_data_is_refused_by_name() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["telemetry"]);
        plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("the first data object publishes");

        let refusal = refusal_of(
            plan_the_writes_and_report_them_all_written(
                &mut planner,
                "telemetry",
                an_opus_packet(20_000_000),
            )
            .expect_err("a track's kind is stated once"),
        );

        assert!(refusal.contains("telemetry"), "{refusal}");
        assert!(refusal.contains("data"), "{refusal}");
        assert!(refusal.contains("opus"), "{refusal}");
    }

    #[test]
    fn a_data_bag_on_a_link_that_first_published_media_is_refused_by_name() {
        let mut planner = a_planner_over(MoqContainerFormat::StreamlibBag, &["camera"]);
        plan_the_writes_and_report_them_all_written(&mut planner, "camera", a_video_sync_point(0))
            .expect("h264 publishes");

        let refusal = refusal_of(
            plan_the_writes_at_and_report_them_all_written(
                &mut planner,
                "camera",
                a_data_object(A_DATA_ENVELOPE),
                33_000_000,
            )
            .expect_err("a track's kind is stated once"),
        );

        assert!(refusal.contains("camera"), "{refusal}");
        assert!(refusal.contains("h264"), "{refusal}");
        assert!(refusal.contains("data"), "{refusal}");
    }

    fn a_streamlib_bag_planner_declaring(
        inbound_link_names: &[&str],
        track_names: Option<&[&str]>,
    ) -> Result<MoqBroadcastObjectWritePlanner> {
        a_planner_declaring(
            MoqContainerFormat::StreamlibBag,
            inbound_link_names,
            track_names,
        )
    }

    fn a_planner_declaring(
        container_format: MoqContainerFormat,
        inbound_link_names: &[&str],
        track_names: Option<&[&str]>,
    ) -> Result<MoqBroadcastObjectWritePlanner> {
        let mut planner = MoqBroadcastObjectWritePlanner::of(
            container_format,
            BROADCAST_NAMESPACE.to_owned(),
            MoqPublisherDeliveryDeadline::of_optional_milliseconds(None),
        );
        planner.declare_tracks(
            each_name_owned(inbound_link_names),
            track_names.map(each_name_owned),
        )?;
        Ok(planner)
    }

    #[test]
    fn track_names_name_the_streamlib_bag_tracks_in_wiring_order() {
        let mut planner = a_streamlib_bag_planner_declaring(
            &["encoder/video", "encoder/audio", "probe/out"],
            Some(&["video", "audio", "telemetry"]),
        )
        .expect("one name per link is declarable");

        let planned = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "probe/out",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("a data object publishes");

        assert_eq!(
            planner.moq_track_names(),
            vec![".catalog", "video", "audio", "telemetry"]
        );
        assert_eq!(
            describe_each_write_instruction_as_a_transport_verb(&planned),
            vec!["only:.catalog", "object:telemetry"],
            "the third link's bags land on the third name"
        );
    }

    #[test]
    fn absent_track_names_leave_each_streamlib_bag_track_named_after_its_link() {
        let planner = a_streamlib_bag_planner_declaring(&["encoder/video", "probe/out"], None)
            .expect("declarable without names");

        assert_eq!(
            planner.moq_track_names(),
            vec![".catalog", "encoder/video", "probe/out"]
        );
    }

    #[test]
    fn track_names_unequal_in_count_to_the_inbound_links_are_refused_by_name() {
        let refusal = refusal_of(
            a_streamlib_bag_planner_declaring(&["encoder/video", "probe/out"], Some(&["video"]))
                .err()
                .expect("one name for two links is refused"),
        );

        assert!(refusal.contains("track_names"), "{refusal}");
        assert!(refusal.contains("1 track"), "{refusal}");
        assert!(refusal.contains("2 inbound links"), "{refusal}");
    }

    #[test]
    fn track_names_under_cmaf_are_refused_by_name() {
        let refusal = refusal_of(
            a_planner_declaring(
                MoqContainerFormat::Cmaf,
                &["encoder/video"],
                Some(&["video"]),
            )
            .err()
            .expect("cmaf's track names are the interop contract"),
        );

        assert!(refusal.contains("track_names"), "{refusal}");
        assert!(refusal.contains("cmaf"), "{refusal}");
        assert!(refusal.contains("streamlib_bag"), "{refusal}");
    }

    #[test]
    fn a_track_name_that_is_the_catalog_tracks_is_refused_by_name() {
        let refusal = refusal_of(
            a_streamlib_bag_planner_declaring(&["probe/out"], Some(&[CATALOG_TRACK_NAME]))
                .err()
                .expect("that name is already the catalog track"),
        );

        assert!(refusal.contains(CATALOG_TRACK_NAME), "{refusal}");
    }

    #[test]
    fn a_track_name_given_twice_is_refused_by_name() {
        let refusal = refusal_of(
            a_streamlib_bag_planner_declaring(
                &["encoder/video", "probe/out"],
                Some(&["video", "video"]),
            )
            .err()
            .expect("two tracks cannot share a name"),
        );

        assert!(refusal.contains("video"), "{refusal}");
        assert!(refusal.contains("twice"), "{refusal}");
    }

    #[test]
    fn an_empty_track_name_is_refused_by_name() {
        let refusal = refusal_of(
            a_streamlib_bag_planner_declaring(&["probe/out"], Some(&[""]))
                .err()
                .expect("an empty name names no track"),
        );

        assert!(refusal.contains("empty"), "{refusal}");
    }

    #[test]
    fn a_data_tracks_catalog_entry_is_the_entry_a_media_streamlib_bag_track_gets() {
        let mut planner =
            a_planner_over(MoqContainerFormat::StreamlibBag, &["camera", "telemetry"]);

        let planned = plan_the_writes_at_and_report_them_all_written(
            &mut planner,
            "telemetry",
            a_data_object(A_DATA_ENVELOPE),
            0,
        )
        .expect("a data object publishes");

        let catalog_object = the_only_object_written_to(&planned, CATALOG_TRACK_NAME)
            .expect("the catalog is written before the first object");
        let mut entries_without_their_names = catalog_tracks_in(&catalog_object);
        for entry in &mut entries_without_their_names {
            entry
                .as_object_mut()
                .expect("a track entry is an object")
                .remove("name");
        }
        assert_eq!(entries_without_their_names.len(), 2);
        assert_eq!(
            entries_without_their_names[0], entries_without_their_names[1],
            "the data track's entry differs from the media track's"
        );
        assert!(entries_without_their_names[1].get("initTrack").is_none());
    }
}
