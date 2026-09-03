// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The fragmented-MP4 writer body: bags in, `ftyp` / `moov` / `moof` + `mdat`
//! out, with no `Runtime` anywhere near it.
//!
//! Fragmented because teardown is not a promise. It runs after a processor's
//! loop exits, in graph-traversal order, and never on a panicked thread — so a
//! flat file whose trailing `moov` never lands is nothing, while this one plays
//! to its last closed fragment.
//!
//! Time is the plan's subtraction written into the container: the epoch is the
//! earliest first stamp across tracks, each track's first `tfdt` is its own
//! offset from it, and there is no edit list and no drift correction.

use std::collections::BTreeMap;
use std::io::Write;

use mp4_atom::{
    Any, Codec, DecodeMaybe, Dinf, Dref, Encode, Ftyp, Hdlr, Mdat, Mdhd, Mdia, Mfhd, Minf, Moof,
    Moov, Mvex, Mvhd, Smhd, Stbl, Stsd, Tfdt, Tfhd, Tkhd, Traf, Trak, Trex, Trun, TrunEntry, Url,
    Vmhd,
};
use serde::Deserialize;
use streamlib::sdk::error::{Error, Result};

use crate::encoded_audio_packet::{EncodedAudioCodec, read_encoded_audio_packet_bag};
use crate::encoded_video_frame::{EncodedVideoCodec, read_encoded_video_frame_bag};
use crate::mp4_annex_b_access_unit::{
    AnnexBNalHeaderGrammar, ParameterSetsFromAnnexBAccessUnit, length_prefix_annex_b_access_unit,
};
use crate::mp4_track_sample_entry::{
    OPUS_TRACK_TIMESCALE_HZ, build_avc1_sample_entry, build_hvc1_sample_entry,
    build_opus_sample_entry,
};

/// Nanoseconds, so a monotonic-nanosecond delta lands in the container
/// exactly. A legal `u32`, which is what lets the subtraction stay integral.
pub const VIDEO_TRACK_TIMESCALE_HZ: u32 = 1_000_000_000;

/// How long a fragment runs when no video track is wired to pace it.
const AUDIO_ONLY_FRAGMENT_SPAN_NS: i64 = 1_000_000_000;

/// A `mdat` header is the 32-bit size and the four-character code; `mp4-atom`
/// refuses a box past `u32::MAX` rather than promoting to a 64-bit largesize,
/// so this is exact for every box this writer emits.
const MDAT_BOX_HEADER_BYTES: usize = 8;

/// `sample_depends_on = 2` (an I-picture), `sample_is_non_sync_sample = 0`.
const SAMPLE_FLAGS_SYNC: u32 = 0x0200_0000;
/// `sample_depends_on = 1`, `sample_is_non_sync_sample = 1`.
const SAMPLE_FLAGS_NON_SYNC: u32 = 0x0101_0000;

/// Just enough of a bag to route it. The full read is the codec's own, which
/// refuses by name; this only decides which of the two to call.
#[derive(Deserialize)]
struct BagCodecPeek {
    codec: Option<String>,
}

/// Which elementary stream a track carries, fixed by its first bag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mp4TrackMedia {
    Video(EncodedVideoCodec),
    Audio,
}

impl Mp4TrackMedia {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Video(codec) => codec.as_wire_str(),
            Self::Audio => EncodedAudioCodec::Opus.as_wire_str(),
        }
    }

    fn timescale(self) -> u32 {
        match self {
            Self::Video(_) => VIDEO_TRACK_TIMESCALE_HZ,
            Self::Audio => OPUS_TRACK_TIMESCALE_HZ,
        }
    }

    fn handler(self) -> &'static str {
        match self {
            Self::Video(_) => "vide",
            Self::Audio => "soun",
        }
    }

    fn nal_header_grammar(self) -> Option<AnnexBNalHeaderGrammar> {
        match self {
            Self::Video(codec) => Some(codec.into()),
            Self::Audio => None,
        }
    }
}

/// One sample, already in the bytes the container carries.
#[derive(Debug, Clone)]
struct Mp4SampleAwaitingFragment {
    sample_bytes: Vec<u8>,
    duration_in_track_timescale: u32,
    is_sync_point: bool,
}

/// A video frame whose duration is not known until its successor arrives.
#[derive(Debug, Clone)]
struct HeldBackVideoSample {
    sample_bytes: Vec<u8>,
    timestamp_ns: i64,
    is_sync_point: bool,
}

/// One track, one inbound link, named by the source channel name the link
/// subscribed to.
#[derive(Debug)]
struct Mp4TrackFromInboundLink {
    inbound_link_name: String,
    track_id: u32,
    media: Option<Mp4TrackMedia>,
    sample_entry: Option<Codec>,
    committed_parameter_sets: Option<ParameterSetsFromAnnexBAccessUnit>,
    committed_channel_count: Option<u32>,
    first_timestamp_ns: Option<i64>,
    last_accepted_timestamp_ns: Option<i64>,
    next_fragment_decode_time_in_track_timescale: u64,
    samples_awaiting_fragment: Vec<Mp4SampleAwaitingFragment>,
    held_back_video_sample: Option<HeldBackVideoSample>,
    latched_refusal: Option<String>,
    bags_discarded_after_latch: u64,
    bags_dropped_out_of_order: u64,
}

impl Mp4TrackFromInboundLink {
    fn new(inbound_link_name: String, track_id: u32) -> Self {
        Self {
            inbound_link_name,
            track_id,
            media: None,
            sample_entry: None,
            committed_parameter_sets: None,
            committed_channel_count: None,
            first_timestamp_ns: None,
            last_accepted_timestamp_ns: None,
            next_fragment_decode_time_in_track_timescale: 0,
            samples_awaiting_fragment: Vec::new(),
            held_back_video_sample: None,
            latched_refusal: None,
            bags_discarded_after_latch: 0,
            bags_dropped_out_of_order: 0,
        }
    }

    /// Stop writing this track, keeping every other one recording.
    ///
    /// A `moof` owes a `traf` to no track (ISO/IEC 14496-12 §8.8.6), so a
    /// track that stops appearing is a legal file needing no extra machinery —
    /// which is why one microphone's format change must not end two cameras'
    /// recording.
    /// Returns whether this call is what latched it, so the tally counts a
    /// track once however many refusals it goes on to meet.
    fn latch(&mut self, refusal: String) -> bool {
        if self.latched_refusal.is_some() {
            return false;
        }
        tracing::error!(
            inbound_link_name = %self.inbound_link_name,
            last_written_timestamp_ns = ?self.last_accepted_timestamp_ns,
            "Mp4Sink: {refusal} — this track stops here and every other track keeps recording"
        );
        self.latched_refusal = Some(refusal);
        self.samples_awaiting_fragment.clear();
        self.held_back_video_sample = None;
        true
    }

    fn is_latched(&self) -> bool {
        self.latched_refusal.is_some()
    }
}

/// What a finished run counted, for the teardown line and for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mp4SinkRunTally {
    pub fragments_written: u32,
    pub samples_written: u64,
    pub bags_dropped_out_of_order: u64,
    pub bags_discarded_after_latch: u64,
    pub tracks_latched: u32,
}

/// The writer body.
pub struct Mp4FragmentedFileWriter<W: Write> {
    sink: W,
    tracks: Vec<Mp4TrackFromInboundLink>,
    header_already_written: bool,
    next_fragment_sequence_number: u32,
    open_fragment_started_at_ns: Option<i64>,
    tally: Mp4SinkRunTally,
}

impl<W: Write> Mp4FragmentedFileWriter<W> {
    /// One track per inbound link, in wiring order, named by its link.
    pub fn new(sink: W, inbound_link_names: &[String]) -> Self {
        let tracks = inbound_link_names
            .iter()
            .enumerate()
            .map(|(index, name)| Mp4TrackFromInboundLink::new(name.clone(), index as u32 + 1))
            .collect();
        Self {
            sink,
            tracks,
            header_already_written: false,
            next_fragment_sequence_number: 1,
            open_fragment_started_at_ns: None,
            tally: Mp4SinkRunTally::default(),
        }
    }

    /// Stop one track and count it once.
    fn latch_track(&mut self, track_index: usize, refusal: String) {
        if self.tracks[track_index].latch(refusal) {
            self.tally.tracks_latched += 1;
        }
    }

    pub fn tally(&self) -> &Mp4SinkRunTally {
        &self.tally
    }

    /// Whether `moov` has landed — false while any track is still silent.
    pub fn header_already_written(&self) -> bool {
        self.header_already_written
    }

    /// Every link still owing its first sync-point bag.
    pub fn inbound_links_still_silent(&self) -> Vec<&str> {
        self.tracks
            .iter()
            .filter(|track| track.sample_entry.is_none() && !track.is_latched())
            .map(|track| track.inbound_link_name.as_str())
            .collect()
    }

    /// Take one bag on one inbound link.
    pub fn accept_bag(
        &mut self,
        inbound_link_name: &str,
        bag_bytes: &[u8],
        timestamp_ns: i64,
    ) -> Result<()> {
        let Some(track_index) = self
            .tracks
            .iter()
            .position(|track| track.inbound_link_name == inbound_link_name)
        else {
            return Err(Error::Runtime(format!(
                "Mp4Sink: a bag arrived on `{inbound_link_name}`, which is not one of the \
                 inbound links this sink enumerated at setup"
            )));
        };
        if self.tracks[track_index].is_latched() {
            self.tracks[track_index].bags_discarded_after_latch += 1;
            self.tally.bags_discarded_after_latch += 1;
            return Ok(());
        }

        let peeked: BagCodecPeek = rmp_serde::from_slice(bag_bytes).map_err(|decode_failure| {
            Error::Runtime(format!(
                "Mp4Sink: the bag on `{inbound_link_name}` is not a named map this sink can \
                 route: {decode_failure}"
            ))
        })?;
        let named_codec = peeked.codec.as_deref();
        if named_codec
            .and_then(EncodedVideoCodec::from_wire_str)
            .is_some()
        {
            return self.accept_video_bag(track_index, bag_bytes, timestamp_ns);
        }
        if named_codec
            .and_then(EncodedAudioCodec::from_wire_str)
            .is_some()
        {
            return self.accept_audio_bag(track_index, bag_bytes, timestamp_ns);
        }
        let track_kinds = EncodedVideoCodec::ALL
            .iter()
            .map(|codec| codec.as_wire_str())
            .chain(
                EncodedAudioCodec::ALL
                    .iter()
                    .map(|codec| codec.as_wire_str()),
            )
            .collect::<Vec<_>>()
            .join("`, `");
        let refusal = format!(
            "the bag on `{inbound_link_name}` names codec {} — a track is one of \
             `{track_kinds}`, and a caption or data convention is its own rung",
            named_codec
                .map(|codec| format!("`{codec}`"))
                .unwrap_or("nothing".into())
        );
        self.latch_track(track_index, refusal);
        Ok(())
    }

    fn accept_video_bag(
        &mut self,
        track_index: usize,
        bag_bytes: &[u8],
        timestamp_ns: i64,
    ) -> Result<()> {
        let frame = read_encoded_video_frame_bag(bag_bytes).map_err(|refusal| {
            Error::Runtime(format!(
                "Mp4Sink: {}: {refusal}",
                self.tracks[track_index].inbound_link_name
            ))
        })?;
        let media = Mp4TrackMedia::Video(frame.codec);
        if !self.commit_or_latch_media(track_index, media) {
            return Ok(());
        }

        let grammar = media
            .nal_header_grammar()
            .expect("a video track has a grammar");
        let split = length_prefix_annex_b_access_unit(&frame.annex_b_access_unit_bytes, grammar);

        if frame.is_sync_point && split.parameter_sets.is_complete_for(grammar) {
            match &self.tracks[track_index].committed_parameter_sets {
                Some(committed) if committed != &split.parameter_sets => {
                    let refusal = format!(
                        "the parameter sets on `{}` changed mid-file, and a sample entry lives \
                         only in the one `moov` — there is no second entry to switch to",
                        self.tracks[track_index].inbound_link_name
                    );
                    self.latch_track(track_index, refusal);
                    return Ok(());
                }
                Some(_) => {}
                None => {
                    let entry = match grammar {
                        AnnexBNalHeaderGrammar::H264 => build_avc1_sample_entry(
                            &self.tracks[track_index].inbound_link_name,
                            &split.parameter_sets,
                            frame.width,
                            frame.height,
                        )
                        .map(Codec::Avc1),
                        AnnexBNalHeaderGrammar::H265 => build_hvc1_sample_entry(
                            &self.tracks[track_index].inbound_link_name,
                            &split.parameter_sets,
                            frame.width,
                            frame.height,
                        )
                        .map(Codec::Hvc1),
                    };
                    match entry {
                        Ok(entry) => {
                            self.tracks[track_index].sample_entry = Some(entry);
                            self.tracks[track_index].committed_parameter_sets =
                                Some(split.parameter_sets.clone());
                        }
                        Err(refusal) => {
                            self.latch_track(track_index, refusal.to_string());
                            return Ok(());
                        }
                    }
                }
            }
        }
        if self.tracks[track_index].sample_entry.is_none() {
            // Still before this track's first usable sync point; nothing can
            // be written for it yet and the header is still waiting.
            return Ok(());
        }
        if !self.accept_timestamp(track_index, timestamp_ns) {
            return Ok(());
        }

        // A fragment closes at the pacing video track's sync points, so the
        // close happens before this sample joins the next one.
        if frame.is_sync_point && self.is_pacing_video_track(track_index) {
            self.close_open_fragment()?;
        }

        let previous =
            self.tracks[track_index]
                .held_back_video_sample
                .replace(HeldBackVideoSample {
                    sample_bytes: split.length_prefixed_sample_bytes,
                    timestamp_ns,
                    is_sync_point: frame.is_sync_point,
                });
        if let Some(previous) = previous {
            let duration_ns = timestamp_ns.saturating_sub(previous.timestamp_ns).max(0);
            self.tracks[track_index]
                .samples_awaiting_fragment
                .push(Mp4SampleAwaitingFragment {
                    sample_bytes: previous.sample_bytes,
                    duration_in_track_timescale: duration_ns as u32,
                    is_sync_point: previous.is_sync_point,
                });
        }
        self.write_header_once_every_track_can_be_described()?;
        Ok(())
    }

    fn accept_audio_bag(
        &mut self,
        track_index: usize,
        bag_bytes: &[u8],
        timestamp_ns: i64,
    ) -> Result<()> {
        let packet = read_encoded_audio_packet_bag(bag_bytes).map_err(|refusal| {
            Error::Runtime(format!(
                "Mp4Sink: {}: {refusal}",
                self.tracks[track_index].inbound_link_name
            ))
        })?;
        if !self.commit_or_latch_media(track_index, Mp4TrackMedia::Audio) {
            return Ok(());
        }

        match self.tracks[track_index].committed_channel_count {
            Some(committed) if committed != packet.channels => {
                let refusal = format!(
                    "the opus track on `{}` changed from {committed} to {} channels mid-file, \
                     and `dOps` shall carry the identification header's count \
                     (Opus-in-ISOBMFF §4.3.2) — there is no second sample entry to switch to",
                    self.tracks[track_index].inbound_link_name, packet.channels
                );
                self.latch_track(track_index, refusal);
                return Ok(());
            }
            Some(_) => {}
            None => {
                match build_opus_sample_entry(
                    &self.tracks[track_index].inbound_link_name,
                    packet.channels,
                    packet.pre_skip,
                ) {
                    Ok(entry) => {
                        self.tracks[track_index].sample_entry = Some(Codec::Opus(entry));
                        self.tracks[track_index].committed_channel_count = Some(packet.channels);
                    }
                    Err(refusal) => {
                        self.latch_track(track_index, refusal.to_string());
                        return Ok(());
                    }
                }
            }
        }
        if !self.accept_timestamp(track_index, timestamp_ns) {
            return Ok(());
        }

        self.tracks[track_index]
            .samples_awaiting_fragment
            .push(Mp4SampleAwaitingFragment {
                sample_bytes: packet.opus_packet_bytes,
                // Opus's own clock is the track's timescale, so the packet's
                // per-channel sample count is its duration verbatim.
                duration_in_track_timescale: packet.sample_count,
                is_sync_point: true,
            });
        self.write_header_once_every_track_can_be_described()?;

        if self.no_video_track_is_wired()
            && let Some(started_at) = self.open_fragment_started_at_ns
            && timestamp_ns.saturating_sub(started_at) >= AUDIO_ONLY_FRAGMENT_SPAN_NS
        {
            self.close_open_fragment()?;
        }
        Ok(())
    }

    /// Fix a track's media on its first bag; latch when a later bag disagrees.
    fn commit_or_latch_media(&mut self, track_index: usize, media: Mp4TrackMedia) -> bool {
        match self.tracks[track_index].media {
            Some(committed) if committed != media => {
                let refusal = format!(
                    "the track on `{}` changed codec from `{}` to `{}` mid-file",
                    self.tracks[track_index].inbound_link_name,
                    committed.wire_name(),
                    media.wire_name()
                );
                self.latch_track(track_index, refusal);
                false
            }
            Some(_) => true,
            None => {
                self.tracks[track_index].media = Some(media);
                true
            }
        }
    }

    /// Refuse a stamp at or before the track's last, which is a producer bug
    /// on an `ordered` input.
    fn accept_timestamp(&mut self, track_index: usize, timestamp_ns: i64) -> bool {
        if let Some(last) = self.tracks[track_index].last_accepted_timestamp_ns
            && timestamp_ns <= last
        {
            self.tracks[track_index].bags_dropped_out_of_order += 1;
            self.tally.bags_dropped_out_of_order += 1;
            tracing::warn!(
                inbound_link_name = %self.tracks[track_index].inbound_link_name,
                timestamp_ns,
                last_written_timestamp_ns = last,
                "Mp4Sink: a bag arrived stamped at or before this track's last written sample \
                 — dropped and counted, a producer bug on an `ordered` input"
            );
            return false;
        }
        self.tracks[track_index].last_accepted_timestamp_ns = Some(timestamp_ns);
        if self.tracks[track_index].first_timestamp_ns.is_none() {
            self.tracks[track_index].first_timestamp_ns = Some(timestamp_ns);
        }
        if self.open_fragment_started_at_ns.is_none() {
            self.open_fragment_started_at_ns = Some(timestamp_ns);
        }
        true
    }

    fn no_video_track_is_wired(&self) -> bool {
        !self
            .tracks
            .iter()
            .any(|track| matches!(track.media, Some(Mp4TrackMedia::Video(_))))
    }

    /// The first video track still recording paces fragment closes; if it
    /// latches, pacing moves to the next healthy one.
    fn is_pacing_video_track(&self, track_index: usize) -> bool {
        self.tracks
            .iter()
            .position(|track| {
                matches!(track.media, Some(Mp4TrackMedia::Video(_))) && !track.is_latched()
            })
            .is_some_and(|pacing| pacing == track_index)
    }

    /// `ftyp` + `moov`, once every track has delivered a sync point.
    fn write_header_once_every_track_can_be_described(&mut self) -> Result<()> {
        if self.header_already_written {
            return Ok(());
        }
        let every_track_describable = self
            .tracks
            .iter()
            .all(|track| track.sample_entry.is_some() || track.is_latched());
        if !every_track_describable {
            return Ok(());
        }
        let Some(epoch_ns) = self
            .tracks
            .iter()
            .filter_map(|track| track.first_timestamp_ns)
            .min()
        else {
            return Ok(());
        };

        let mut header_bytes = Vec::new();
        Ftyp {
            major_brand: b"iso6".into(),
            minor_version: 512,
            compatible_brands: vec![b"iso6".into(), b"mp41".into(), b"cmfc".into()],
        }
        .encode(&mut header_bytes)
        .map_err(box_write_failure)?;
        self.build_moov()?
            .encode(&mut header_bytes)
            .map_err(box_write_failure)?;
        self.sink.write_all(&header_bytes)?;

        // Each track's first `tfdt` is its own offset from the epoch.
        for track in &mut self.tracks {
            // A track latched before it ever named a codec has no media and no
            // `trak`, exactly as `build_moov` skips it.
            let Some(media) = track.media else {
                continue;
            };
            let offset_ns = track
                .first_timestamp_ns
                .unwrap_or(epoch_ns)
                .saturating_sub(epoch_ns)
                .max(0);
            track.next_fragment_decode_time_in_track_timescale =
                rescale_nanoseconds(offset_ns, media.timescale());
        }
        self.header_already_written = true;
        Ok(())
    }

    fn build_moov(&self) -> Result<Moov> {
        let mut moov = Moov {
            mvhd: Mvhd {
                timescale: VIDEO_TRACK_TIMESCALE_HZ,
                // A fragmented file's duration is not known while it is being
                // written, and `mehd` is optional precisely so it need not be.
                duration: 0,
                next_track_id: self.tracks.len() as u32 + 1,
                ..Default::default()
            },
            mvex: Some(Mvex {
                mehd: None,
                trex: Vec::new(),
            }),
            ..Default::default()
        };
        for track in &self.tracks {
            let (Some(media), Some(sample_entry)) = (track.media, track.sample_entry.clone())
            else {
                continue;
            };
            moov.trak.push(Trak {
                tkhd: Tkhd {
                    track_id: track.track_id,
                    enabled: true,
                    in_movie: true,
                    duration: 0,
                    ..Default::default()
                },
                mdia: Mdia {
                    mdhd: Mdhd {
                        timescale: media.timescale(),
                        duration: 0,
                        language: "und".to_string(),
                        ..Default::default()
                    },
                    // The track's name is the inbound link it carries, which is
                    // what makes a recording self-describing without config.
                    hdlr: Hdlr {
                        handler: media.handler().as_bytes()[..4]
                            .try_into()
                            .map(|four: [u8; 4]| four.into())
                            .expect("a four-byte handler"),
                        name: track.inbound_link_name.clone(),
                    },
                    minf: Minf {
                        vmhd: matches!(media, Mp4TrackMedia::Video(_)).then(Vmhd::default),
                        smhd: matches!(media, Mp4TrackMedia::Audio).then(Smhd::default),
                        dinf: Dinf {
                            dref: Dref {
                                urls: vec![Url {
                                    location: String::new(),
                                }],
                            },
                        },
                        stbl: Stbl {
                            stsd: Stsd {
                                codecs: vec![sample_entry],
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                },
                ..Default::default()
            });
            if let Some(mvex) = moov.mvex.as_mut() {
                mvex.trex.push(Trex {
                    track_id: track.track_id,
                    default_sample_description_index: 1,
                    default_sample_duration: 0,
                    default_sample_size: 0,
                    default_sample_flags: SAMPLE_FLAGS_NON_SYNC,
                });
            }
        }
        Ok(moov)
    }

    /// Close the open fragment, writing one `traf` per track that has samples.
    pub fn close_open_fragment(&mut self) -> Result<()> {
        if !self.header_already_written {
            return Ok(());
        }
        let contributing: Vec<usize> = (0..self.tracks.len())
            .filter(|&index| !self.tracks[index].samples_awaiting_fragment.is_empty())
            .collect();
        if contributing.is_empty() {
            return Ok(());
        }

        // Sizes first: `trun.data_offset` is relative to the `moof` start, so
        // the box has to be laid out once before the offsets are knowable.
        // Every offset is `Some`, so the encoded size does not move when the
        // placeholder values are replaced.
        let mut moof = self.build_moof(&contributing, &BTreeMap::new())?;
        let mut sized = Vec::new();
        moof.encode(&mut sized).map_err(box_write_failure)?;
        let moof_bytes = sized.len();

        let mut data_offsets = BTreeMap::new();
        let mut running = moof_bytes + MDAT_BOX_HEADER_BYTES;
        for &index in &contributing {
            data_offsets.insert(index, running as i32);
            running += self.tracks[index]
                .samples_awaiting_fragment
                .iter()
                .map(|sample| sample.sample_bytes.len())
                .sum::<usize>();
        }
        moof = self.build_moof(&contributing, &data_offsets)?;

        let mut fragment_bytes = Vec::new();
        moof.encode(&mut fragment_bytes)
            .map_err(box_write_failure)?;
        if fragment_bytes.len() != moof_bytes {
            // Every `trun.data_offset` is `Some`, so it encodes as four bytes
            // whatever its value and pass two cannot move. If that ever stops
            // holding, every sample offset in the fragment is wrong and the
            // file is silently unplayable — worth one `usize` compare.
            return Err(Error::Runtime(format!(
                "Mp4Sink: the fragment moved between sizing and writing ({moof_bytes} then {} \
                 bytes), so every sample offset in it would be wrong",
                fragment_bytes.len()
            )));
        }

        self.sink.write_all(&fragment_bytes)?;

        // The samples are already contiguous per sample and the payload length
        // was measured above, so the `mdat` header is written directly and each
        // sample goes straight to the sink. Building the box would copy the
        // whole payload twice, once to concatenate and once to encode, on a
        // path that runs at every sync point.
        let media_data_bytes = running - moof_bytes - MDAT_BOX_HEADER_BYTES;
        let mdat_box_bytes = MDAT_BOX_HEADER_BYTES + media_data_bytes;
        let mdat_box_bytes: u32 = mdat_box_bytes.try_into().map_err(|_| {
            Error::Runtime(format!(
                "Mp4Sink: this fragment's media is {media_data_bytes} bytes, past what a \
                 32-bit box size can name — close fragments more often"
            ))
        })?;
        self.sink.write_all(&mdat_box_bytes.to_be_bytes())?;
        self.sink.write_all(b"mdat")?;
        for &index in &contributing {
            for sample in &self.tracks[index].samples_awaiting_fragment {
                self.sink.write_all(&sample.sample_bytes)?;
            }
        }
        self.sink.flush()?;

        for &index in &contributing {
            let written: u32 = self.tracks[index]
                .samples_awaiting_fragment
                .iter()
                .map(|sample| sample.duration_in_track_timescale)
                .sum();
            self.tally.samples_written += self.tracks[index].samples_awaiting_fragment.len() as u64;
            self.tracks[index].next_fragment_decode_time_in_track_timescale += written as u64;
            self.tracks[index].samples_awaiting_fragment.clear();
        }
        self.next_fragment_sequence_number += 1;
        self.tally.fragments_written += 1;
        self.open_fragment_started_at_ns = None;
        Ok(())
    }

    fn build_moof(
        &self,
        contributing: &[usize],
        data_offsets: &BTreeMap<usize, i32>,
    ) -> Result<Moof> {
        let mut moof = Moof {
            mfhd: Mfhd {
                sequence_number: self.next_fragment_sequence_number,
            },
            traf: Vec::new(),
        };
        for &index in contributing {
            let track = &self.tracks[index];
            moof.traf.push(Traf {
                tfhd: Tfhd {
                    track_id: track.track_id,
                    // Offsets are relative to this `moof`, which is what lets a
                    // fragment be read without knowing where the file starts.
                    default_base_is_moof: true,
                    base_data_offset: None,
                    sample_description_index: None,
                    default_sample_duration: None,
                    default_sample_size: None,
                    default_sample_flags: None,
                    duration_is_empty: false,
                },
                tfdt: Some(Tfdt {
                    base_media_decode_time: track.next_fragment_decode_time_in_track_timescale,
                }),
                trun: vec![Trun {
                    data_offset: Some(data_offsets.get(&index).copied().unwrap_or(0)),
                    entries: track
                        .samples_awaiting_fragment
                        .iter()
                        .map(|sample| TrunEntry {
                            duration: Some(sample.duration_in_track_timescale),
                            size: Some(sample.sample_bytes.len() as u32),
                            flags: Some(if sample.is_sync_point {
                                SAMPLE_FLAGS_SYNC
                            } else {
                                SAMPLE_FLAGS_NON_SYNC
                            }),
                            cts: None,
                        })
                        .collect(),
                }],
                ..Default::default()
            });
        }
        Ok(moof)
    }

    /// Close the open fragment, held-back frames included, and owe nothing
    /// else.
    ///
    /// A held-back frame has no successor to measure against, so it takes its
    /// predecessor's duration — the only honest guess, and one frame long.
    pub fn finish(mut self) -> Result<Mp4SinkRunTally> {
        for index in 0..self.tracks.len() {
            let Some(held_back) = self.tracks[index].held_back_video_sample.take() else {
                continue;
            };
            let predecessor_duration = self.tracks[index]
                .samples_awaiting_fragment
                .last()
                .map(|sample| sample.duration_in_track_timescale)
                .unwrap_or(0);
            self.tracks[index]
                .samples_awaiting_fragment
                .push(Mp4SampleAwaitingFragment {
                    sample_bytes: held_back.sample_bytes,
                    duration_in_track_timescale: predecessor_duration,
                    is_sync_point: held_back.is_sync_point,
                });
        }
        self.close_open_fragment()?;
        self.sink.flush()?;
        // The tally cannot say *which* link misbehaved, and that is the only
        // thing an operator can act on.
        for track in &self.tracks {
            if track.bags_dropped_out_of_order > 0 || track.bags_discarded_after_latch > 0 {
                tracing::info!(
                    inbound_link_name = %track.inbound_link_name,
                    bags_dropped_out_of_order = track.bags_dropped_out_of_order,
                    bags_discarded_after_latch = track.bags_discarded_after_latch,
                    latched_refusal = ?track.latched_refusal,
                    "Mp4Sink: this link did not record everything it sent"
                );
            }
        }
        Ok(self.tally)
    }
}

fn rescale_nanoseconds(nanoseconds: i64, timescale: u32) -> u64 {
    if timescale == VIDEO_TRACK_TIMESCALE_HZ {
        return nanoseconds.max(0) as u64;
    }
    (nanoseconds.max(0) as i128 * timescale as i128 / VIDEO_TRACK_TIMESCALE_HZ as i128) as u64
}

fn box_write_failure(failure: mp4_atom::Error) -> Error {
    Error::Runtime(format!("Mp4Sink: a box could not be written: {failure}"))
}

/// Re-parse a written file, for the container-bytes tests below.
///
/// `cargo xtask mp4-inspect` does not come through here — `xtask` does not
/// depend on this crate and walks the boxes itself.
#[cfg(test)]
fn parse_written_atoms(file_bytes: &[u8]) -> Result<Vec<Any>> {
    let mut cursor = std::io::Cursor::new(file_bytes);
    let mut atoms = Vec::new();
    loop {
        match Any::decode_maybe(&mut cursor) {
            Ok(Some(atom)) => atoms.push(atom),
            Ok(None) => break,
            Err(failure) => {
                return Err(Error::Runtime(format!(
                    "Mp4Sink: the written file did not re-parse: {failure}"
                )));
            }
        }
    }
    Ok(atoms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoded_audio_packet::{EncodedAudioCodec, EncodedAudioPacket};
    use crate::encoded_video_frame::EncodedVideoFrame;

    /// A 320x240 baseline SPS, and the PPS that goes with it.
    const H264_SEQUENCE_PARAMETER_SET: &[u8] = &[
        0x67, 0x42, 0xC0, 0x1E, 0xD9, 0x00, 0xA0, 0x3D, 0xA1, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00,
        0x00, 0x03, 0x00, 0x32, 0x0F, 0x16, 0x2E, 0x48,
    ];
    const H264_PICTURE_PARAMETER_SET: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];
    /// The same SPS at a different level, which is a mid-file change.
    const H264_SEQUENCE_PARAMETER_SET_AT_ANOTHER_LEVEL: &[u8] = &[
        0x67, 0x42, 0xC0, 0x28, 0xD9, 0x00, 0xA0, 0x3D, 0xA1, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00,
        0x00, 0x03, 0x00, 0x32, 0x0F, 0x16, 0x2E, 0x48,
    ];

    fn annex_b(nal_units: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for nal_unit in nal_units {
            bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            bytes.extend_from_slice(nal_unit);
        }
        bytes
    }

    fn h264_bag(
        sequence_index: u64,
        is_sync_point: bool,
        sequence_parameter_set: &[u8],
    ) -> Vec<u8> {
        let coded_slice: &[u8] = if is_sync_point {
            &[0x65, 0x88, 0x84, 0x21]
        } else {
            &[0x41, 0x9A, 0x22, 0x33]
        };
        let access_unit = if is_sync_point {
            annex_b(&[
                sequence_parameter_set,
                H264_PICTURE_PARAMETER_SET,
                coded_slice,
            ])
        } else {
            annex_b(&[coded_slice])
        };
        rmp_serde::to_vec_named(&EncodedVideoFrame {
            codec: EncodedVideoCodec::H264,
            annex_b_access_unit_bytes: access_unit,
            is_sync_point,
            group_index: 0,
            sequence_index,
            width: 320,
            height: 240,
            color: None,
        })
        .expect("msgpack serialize")
    }

    fn opus_bag(sequence_index: u64, channels: u32) -> Vec<u8> {
        rmp_serde::to_vec_named(&EncodedAudioPacket {
            codec: EncodedAudioCodec::Opus,
            opus_packet_bytes: vec![0xFC, 0xFF, 0xFE, sequence_index as u8],
            is_sync_point: true,
            group_index: sequence_index,
            sequence_index,
            sample_rate: 48_000,
            channels,
            sample_count: 960,
            pre_skip: 312,
        })
        .expect("msgpack serialize")
    }

    /// 30 fps in nanoseconds, so a duration lands on an exact integer.
    const ONE_VIDEO_FRAME_NS: i64 = 33_333_333;
    /// 20 ms, the span one Opus packet covers.
    const ONE_OPUS_PACKET_NS: i64 = 20_000_000;

    fn write_one_video_track(frames: usize) -> (Vec<u8>, Mp4SinkRunTally) {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["camera/video".to_string()]);
        for index in 0..frames {
            writer
                .accept_bag(
                    "camera/video",
                    &h264_bag(index as u64, index == 0, H264_SEQUENCE_PARAMETER_SET),
                    index as i64 * ONE_VIDEO_FRAME_NS,
                )
                .expect("the bag is accepted");
        }
        let tally = writer.finish().expect("the file closes");
        (file, tally)
    }

    fn only_moov(atoms: &[Any]) -> &Moov {
        atoms
            .iter()
            .find_map(|atom| match atom {
                Any::Moov(moov) => Some(moov),
                _ => None,
            })
            .expect("the file carries one moov")
    }

    fn every_moof(atoms: &[Any]) -> Vec<&Moof> {
        atoms
            .iter()
            .filter_map(|atom| match atom {
                Any::Moof(moof) => Some(moof),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_file_opens_with_the_brands_and_one_trak_per_link_named_after_its_producer() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "microphone/audio".to_string()],
        );
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(0, true, H264_SEQUENCE_PARAMETER_SET),
                0,
            )
            .expect("accepted");
        writer
            .accept_bag("microphone/audio", &opus_bag(0, 2), 0)
            .expect("accepted");
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(1, false, H264_SEQUENCE_PARAMETER_SET),
                ONE_VIDEO_FRAME_NS,
            )
            .expect("accepted");
        writer.finish().expect("the file closes");

        let atoms = parse_written_atoms(&file).expect("the file re-parses");
        let Any::Ftyp(ftyp) = &atoms[0] else {
            panic!("the file opens with ftyp, got {:?}", atoms[0]);
        };
        assert_eq!(ftyp.major_brand, b"iso6".into());
        assert!(ftyp.compatible_brands.contains(&b"cmfc".into()));

        let moov = only_moov(&atoms);
        assert_eq!(moov.trak.len(), 2, "one trak per inbound link");
        let track_names: Vec<&str> = moov
            .trak
            .iter()
            .map(|trak| trak.mdia.hdlr.name.as_str())
            .collect();
        assert_eq!(
            track_names,
            vec!["camera/video", "microphone/audio"],
            "each track is named by the source channel name its link subscribed to"
        );
        assert_eq!(
            moov.mvex
                .as_ref()
                .expect("a fragmented file has mvex")
                .trex
                .len(),
            2,
            "every track has a trex"
        );
    }

    #[test]
    fn an_avc1_track_states_the_profile_and_level_from_its_first_sync_point() {
        let (file, _) = write_one_video_track(3);
        let atoms = parse_written_atoms(&file).expect("the file re-parses");
        let moov = only_moov(&atoms);

        let Codec::Avc1(avc1) = &moov.trak[0].mdia.minf.stbl.stsd.codecs[0] else {
            panic!("an h264 track is described by avc1");
        };
        assert_eq!(avc1.avcc.avc_profile_indication, 0x42);
        assert_eq!(avc1.avcc.profile_compatibility, 0xC0);
        assert_eq!(avc1.avcc.avc_level_indication, 0x1E);
        assert_eq!(
            avc1.avcc.sequence_parameter_sets,
            vec![H264_SEQUENCE_PARAMETER_SET.to_vec()],
            "the sets live in the sample entry, which is the only place avc1 allows"
        );
        assert_eq!(
            avc1.avcc.picture_parameter_sets,
            vec![H264_PICTURE_PARAMETER_SET.to_vec()]
        );
    }

    #[test]
    fn an_opus_track_states_its_channels_and_the_encoders_pre_skip() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["microphone/audio".to_string()]);
        for index in 0..3u64 {
            writer
                .accept_bag(
                    "microphone/audio",
                    &opus_bag(index, 2),
                    index as i64 * ONE_OPUS_PACKET_NS,
                )
                .expect("accepted");
        }
        writer.finish().expect("the file closes");

        let atoms = parse_written_atoms(&file).expect("the file re-parses");
        let Codec::Opus(opus) = &only_moov(&atoms).trak[0].mdia.minf.stbl.stsd.codecs[0] else {
            panic!("an opus track is described by an Opus entry");
        };
        assert_eq!(opus.dops.output_channel_count, 2);
        assert_eq!(opus.dops.pre_skip, 312);
        assert_eq!(opus.dops.input_sample_rate, 48_000);
        assert_eq!(
            only_moov(&atoms).trak[0].mdia.mdhd.timescale,
            48_000,
            "an opus track runs on Opus's own clock"
        );
    }

    #[test]
    fn no_parameter_set_nal_survives_into_any_sample() {
        let (file, _) = write_one_video_track(4);
        let media_data: Vec<u8> = parse_written_atoms(&file)
            .expect("re-parses")
            .into_iter()
            .filter_map(|atom| match atom {
                Any::Mdat(mdat) => Some(mdat.data),
                _ => None,
            })
            .flatten()
            .collect();

        for parameter_set in [H264_SEQUENCE_PARAMETER_SET, H264_PICTURE_PARAMETER_SET] {
            assert!(
                !media_data
                    .windows(parameter_set.len())
                    .any(|window| window == parameter_set),
                "ISO/IEC 14496-15 forbids in-band parameter sets under avc1, so no sample \
                 may carry one"
            );
        }
    }

    #[test]
    fn every_sample_nal_inside_mdat_is_four_byte_length_prefixed() {
        let (file, _) = write_one_video_track(4);
        let atoms = parse_written_atoms(&file).expect("re-parses");
        let media_data: Vec<u8> = atoms
            .iter()
            .filter_map(|atom| match atom {
                Any::Mdat(mdat) => Some(mdat.data.clone()),
                _ => None,
            })
            .flatten()
            .collect();

        // Walking the whole mdat by its own length prefixes must land exactly
        // on the end: that is what a player does, and any drift desyncs it.
        let mut offset = 0usize;
        let mut nal_units_walked = 0;
        while offset < media_data.len() {
            let length =
                u32::from_be_bytes(media_data[offset..offset + 4].try_into().unwrap()) as usize;
            assert!(length > 0, "a zero-length NAL is not a NAL");
            offset += 4 + length;
            nal_units_walked += 1;
        }
        assert_eq!(
            offset,
            media_data.len(),
            "the length-prefix walk lands exactly on the end of mdat"
        );
        assert!(nal_units_walked >= 3);
    }

    #[test]
    fn each_tracks_first_tfdt_is_its_own_offset_from_the_earliest_stamp() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "microphone/audio".to_string()],
        );
        // The camera is the epoch; the microphone starts 100 ms later.
        let microphone_offset_ns = 100_000_000i64;
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(0, true, H264_SEQUENCE_PARAMETER_SET),
                0,
            )
            .expect("accepted");
        writer
            .accept_bag("microphone/audio", &opus_bag(0, 2), microphone_offset_ns)
            .expect("accepted");
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(1, false, H264_SEQUENCE_PARAMETER_SET),
                ONE_VIDEO_FRAME_NS,
            )
            .expect("accepted");
        writer.finish().expect("closes");

        let atoms = parse_written_atoms(&file).expect("re-parses");
        let first_fragment = every_moof(&atoms)[0];
        let video_traf = first_fragment
            .traf
            .iter()
            .find(|traf| traf.tfhd.track_id == 1)
            .expect("the video track contributes");
        let audio_traf = first_fragment
            .traf
            .iter()
            .find(|traf| traf.tfhd.track_id == 2)
            .expect("the audio track contributes");

        assert_eq!(
            video_traf.tfdt.as_ref().unwrap().base_media_decode_time,
            0,
            "the earliest track starts at the epoch itself"
        );
        assert_eq!(
            audio_traf.tfdt.as_ref().unwrap().base_media_decode_time,
            (microphone_offset_ns as u64 * 48_000) / 1_000_000_000,
            "a later track's first tfdt is its own offset from the epoch, in its own timescale"
        );
    }

    #[test]
    fn a_video_samples_duration_is_the_delta_to_its_successor() {
        let (file, _) = write_one_video_track(4);
        let atoms = parse_written_atoms(&file).expect("re-parses");
        let durations: Vec<u32> = every_moof(&atoms)
            .iter()
            .flat_map(|moof| moof.traf.iter())
            .flat_map(|traf| traf.trun.iter())
            .flat_map(|trun| trun.entries.iter())
            .filter_map(|entry| entry.duration)
            .collect();

        assert!(!durations.is_empty());
        for duration in &durations {
            assert_eq!(
                *duration, ONE_VIDEO_FRAME_NS as u32,
                "a video sample's duration is the nanosecond delta to its successor, and the \
                 last takes its predecessor's"
            );
        }
    }

    #[test]
    fn an_opus_samples_duration_is_its_own_sample_count() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["microphone/audio".to_string()]);
        for index in 0..5u64 {
            writer
                .accept_bag(
                    "microphone/audio",
                    &opus_bag(index, 1),
                    index as i64 * ONE_OPUS_PACKET_NS,
                )
                .expect("accepted");
        }
        writer.finish().expect("closes");

        let atoms = parse_written_atoms(&file).expect("re-parses");
        let durations: Vec<u32> = every_moof(&atoms)
            .iter()
            .flat_map(|moof| moof.traf.iter())
            .flat_map(|traf| traf.trun.iter())
            .flat_map(|trun| trun.entries.iter())
            .filter_map(|entry| entry.duration)
            .collect();
        assert_eq!(
            durations,
            vec![960; 5],
            "each sample spans its sample_count"
        );
    }

    #[test]
    fn a_file_truncated_at_any_fragment_boundary_re_parses_cleanly() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["camera/video".to_string()]);
        // Four sync points, so four fragment boundaries land in the file.
        for index in 0..12usize {
            writer
                .accept_bag(
                    "camera/video",
                    &h264_bag(index as u64, index % 3 == 0, H264_SEQUENCE_PARAMETER_SET),
                    index as i64 * ONE_VIDEO_FRAME_NS,
                )
                .expect("accepted");
        }
        writer.finish().expect("closes");

        // Every top-level box boundary is a point a reader could stop at, which
        // is the whole reason the layout is fragmented.
        let mut boundary = 0usize;
        let mut boundaries = Vec::new();
        while boundary < file.len() {
            let size =
                u32::from_be_bytes(file[boundary..boundary + 4].try_into().unwrap()) as usize;
            assert!(size >= 8, "a box declares its own size");
            boundary += size;
            boundaries.push(boundary);
        }
        assert!(
            boundaries.len() >= 4,
            "the run produced several fragments, got {}",
            boundaries.len()
        );

        for &truncate_at in &boundaries {
            parse_written_atoms(&file[..truncate_at]).unwrap_or_else(|failure| {
                panic!("truncating at {truncate_at} must still re-parse: {failure}")
            });
        }
    }

    #[test]
    fn a_mid_file_parameter_set_change_stops_that_track_and_leaves_the_others_recording() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "microphone/audio".to_string()],
        );
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(0, true, H264_SEQUENCE_PARAMETER_SET),
                0,
            )
            .expect("accepted");
        writer
            .accept_bag("microphone/audio", &opus_bag(0, 2), 0)
            .expect("accepted");
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(1, false, H264_SEQUENCE_PARAMETER_SET),
                ONE_VIDEO_FRAME_NS,
            )
            .expect("accepted");
        // A second sync point carrying a different SPS: there is no second
        // sample entry to switch to.
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(2, true, H264_SEQUENCE_PARAMETER_SET_AT_ANOTHER_LEVEL),
                2 * ONE_VIDEO_FRAME_NS,
            )
            .expect("the refusal is a latch, not an error");
        for index in 1..6u64 {
            writer
                .accept_bag(
                    "microphone/audio",
                    &opus_bag(index, 2),
                    index as i64 * ONE_OPUS_PACKET_NS,
                )
                .expect("the healthy track keeps recording");
        }
        let tally = writer.finish().expect("closes");

        assert_eq!(
            tally.tracks_latched, 1,
            "exactly the offending track stopped"
        );
        let atoms = parse_written_atoms(&file).expect("the file stays readable");
        assert_eq!(
            only_moov(&atoms).trak.len(),
            2,
            "both tracks are still described; one simply stops contributing"
        );
        let audio_samples: usize = every_moof(&atoms)
            .iter()
            .flat_map(|moof| moof.traf.iter())
            .filter(|traf| traf.tfhd.track_id == 2)
            .flat_map(|traf| traf.trun.iter())
            .map(|trun| trun.entries.len())
            .sum();
        assert_eq!(
            audio_samples, 6,
            "every microphone packet reached the file after the camera stopped"
        );
    }

    #[test]
    fn an_opus_track_whose_channel_count_changes_stops_naming_both_counts() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["microphone/audio".to_string()]);
        writer
            .accept_bag("microphone/audio", &opus_bag(0, 2), 0)
            .expect("accepted");
        writer
            .accept_bag("microphone/audio", &opus_bag(1, 1), ONE_OPUS_PACKET_NS)
            .expect("the refusal is a latch");
        let tally = writer.finish().expect("closes");

        assert_eq!(tally.tracks_latched, 1);
        assert_eq!(
            tally.bags_discarded_after_latch, 0,
            "the changing bag is the one that latches; nothing followed it"
        );
    }

    #[test]
    fn a_bag_stamped_at_or_before_the_last_written_one_is_dropped_and_counted() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["microphone/audio".to_string()]);
        writer
            .accept_bag("microphone/audio", &opus_bag(0, 1), 5_000)
            .expect("accepted");
        writer
            .accept_bag("microphone/audio", &opus_bag(1, 1), 5_000)
            .expect("accepted but dropped");
        writer
            .accept_bag("microphone/audio", &opus_bag(2, 1), 4_000)
            .expect("accepted but dropped");
        let tally = writer.finish().expect("closes");

        assert_eq!(
            tally.bags_dropped_out_of_order, 2,
            "a stamp at or before the last written one is a producer bug on an ordered input"
        );
        assert_eq!(tally.samples_written, 1);
    }

    #[test]
    fn a_bag_naming_a_codec_no_track_kind_covers_is_refused_by_name() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["captions/text".to_string()]);
        let caption_shaped_bag = rmp_serde::to_vec_named(&serde_json::json!({
            "codec": "webvtt",
            "bitstream": "hello",
        }))
        .expect("msgpack serialize");

        writer
            .accept_bag("captions/text", &caption_shaped_bag, 0)
            .expect("the refusal is a latch, not an error");
        let tally = writer.finish().expect("closes");
        assert_eq!(tally.tracks_latched, 1);
    }

    #[test]
    fn a_bag_on_a_link_the_sink_never_enumerated_is_an_error_not_a_latch() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["camera/video".to_string()]);
        let failure = writer
            .accept_bag(
                "ghost/video",
                &h264_bag(0, true, H264_SEQUENCE_PARAMETER_SET),
                0,
            )
            .expect_err("a link that was never wired cannot have a track");
        assert!(failure.to_string().contains("ghost/video"));
    }

    #[test]
    fn the_header_waits_until_every_link_has_delivered_a_sync_point() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "microphone/audio".to_string()],
        );
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(0, true, H264_SEQUENCE_PARAMETER_SET),
                0,
            )
            .expect("accepted");
        assert!(
            !writer.header_already_written(),
            "a sample entry needs the Opus header, so moov waits on the silent link"
        );
        assert_eq!(
            writer.inbound_links_still_silent(),
            vec!["microphone/audio"]
        );

        writer
            .accept_bag("microphone/audio", &opus_bag(0, 2), 0)
            .expect("accepted");
        assert!(
            writer.header_already_written(),
            "every track can now be described"
        );
        assert!(writer.inbound_links_still_silent().is_empty());
    }

    #[test]
    fn a_fragment_closes_at_the_pacing_video_tracks_sync_points() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "microphone/audio".to_string()],
        );
        // Interleaved the way a real recording arrives: both links deliver
        // from the start, so `moov` lands on the first pair.
        for index in 0..18usize {
            writer
                .accept_bag(
                    "camera/video",
                    &h264_bag(index as u64, index % 6 == 0, H264_SEQUENCE_PARAMETER_SET),
                    index as i64 * ONE_VIDEO_FRAME_NS,
                )
                .expect("accepted");
            writer
                .accept_bag(
                    "microphone/audio",
                    &opus_bag(index as u64, 2),
                    index as i64 * ONE_OPUS_PACKET_NS,
                )
                .expect("accepted");
        }
        let tally = writer.finish().expect("closes");

        let atoms = parse_written_atoms(&file).expect("re-parses");
        let fragments = every_moof(&atoms);
        assert!(
            fragments.len() >= 3,
            "a fragment closes at each sync point of the pacing video track, got {}",
            fragments.len()
        );
        assert_eq!(tally.fragments_written as usize, fragments.len());

        let sequence_numbers: Vec<u32> = fragments
            .iter()
            .map(|moof| moof.mfhd.sequence_number)
            .collect();
        let mut expected: Vec<u32> = (1..=fragments.len() as u32).collect();
        expected.sort_unstable();
        assert_eq!(
            sequence_numbers, expected,
            "fragment sequence numbers run 1..n with no gap"
        );

        // Each track's decode time advances monotonically across fragments,
        // which is what keeps the two tracks aligned to the one epoch.
        for track_id in [1u32, 2u32] {
            let decode_times: Vec<u64> = fragments
                .iter()
                .flat_map(|moof| moof.traf.iter())
                .filter(|traf| traf.tfhd.track_id == track_id)
                .filter_map(|traf| traf.tfdt.as_ref().map(|tfdt| tfdt.base_media_decode_time))
                .collect();
            assert!(
                decode_times.windows(2).all(|pair| pair[0] < pair[1]),
                "track {track_id} decode times must advance: {decode_times:?}"
            );
        }
    }

    #[test]
    fn a_link_latched_before_it_named_a_codec_leaves_the_healthy_track_recording() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "captions/text".to_string()],
        );
        // The caption link latches without ever committing a media kind, so it
        // has no track to describe while the camera still needs its `moov`.
        let caption_shaped_bag = rmp_serde::to_vec_named(&serde_json::json!({
            "codec": "webvtt",
            "bitstream": "hello",
        }))
        .expect("msgpack serialize");
        writer
            .accept_bag("captions/text", &caption_shaped_bag, 0)
            .expect("the refusal is a latch");
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(0, true, H264_SEQUENCE_PARAMETER_SET),
                0,
            )
            .expect("accepted");
        writer
            .accept_bag(
                "camera/video",
                &h264_bag(1, false, H264_SEQUENCE_PARAMETER_SET),
                ONE_VIDEO_FRAME_NS,
            )
            .expect("accepted");
        let tally = writer
            .finish()
            .expect("the header lands despite the latched link");

        assert_eq!(tally.tracks_latched, 1);
        let atoms = parse_written_atoms(&file).expect("re-parses");
        assert_eq!(
            only_moov(&atoms).trak.len(),
            1,
            "a link that never named a codec describes no track, and the camera still records"
        );
        assert_eq!(only_moov(&atoms).trak[0].mdia.hdlr.name, "camera/video");
    }
}
