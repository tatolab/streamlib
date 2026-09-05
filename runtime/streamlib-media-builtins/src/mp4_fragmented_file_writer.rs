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
    Codec, Dinf, Dref, Encode, FixedPoint, Ftyp, Hdlr, Mdhd, Mdia, Mfhd, Minf, Moof, Moov, Mvex,
    Mvhd, Smhd, Stbl, Stco, Stsd, Tfdt, Tfhd, Tkhd, Traf, Trak, Trex, Trun, TrunEntry, Url, Vmhd,
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

/// How many bytes of samples may be held while a link is still silent.
///
/// `moov` cannot be written until every track can be described, and nothing
/// can be flushed before it, so a link that never delivers a first sync point
/// would otherwise hold every healthy track's samples for the whole run — a
/// camera that fails to open turning a long recording into unbounded growth
/// and an empty file. At the budget the silent links are latched by name so
/// the tracks that did deliver can write `moov` and start flushing. Sized to
/// hold several seconds of 1080p at a normal bitrate; a recording that trips
/// it has a broken producer, not a busy one.
const HIGHEST_BYTES_HELD_AWAITING_THE_HEADER: usize = 64 * 1024 * 1024;

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
    /// The coded extent `tkhd` states as the track's presentation size.
    coded_extent: Option<(u32, u32)>,
    first_timestamp_ns: Option<i64>,
    last_accepted_timestamp_ns: Option<i64>,
    next_fragment_decode_time_in_track_timescale: u64,
    samples_awaiting_fragment: Vec<Mp4SampleAwaitingFragment>,
    held_back_video_sample: Option<HeldBackVideoSample>,
    /// Survives the close that empties `samples_awaiting_fragment`, so a final
    /// held-back frame still has a predecessor's duration to take.
    last_written_sample_duration_in_track_timescale: Option<u32>,
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
            coded_extent: None,
            first_timestamp_ns: None,
            last_accepted_timestamp_ns: None,
            next_fragment_decode_time_in_track_timescale: 0,
            samples_awaiting_fragment: Vec::new(),
            held_back_video_sample: None,
            last_written_sample_duration_in_track_timescale: None,
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
                            self.tracks[track_index].coded_extent =
                                Some((frame.width, frame.height));
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

        // The held-back predecessor is resolved and pushed *before* any close,
        // so it lands in the fragment it belongs to and the incoming sync frame
        // becomes the next fragment's first sample. A fragment whose first
        // sample were not a sync point would be no random-access point at all,
        // which is the whole premise of the `cmfc` brand this file declares.
        if let Some(previous) = self.tracks[track_index].held_back_video_sample.take() {
            let duration_ns = timestamp_ns.saturating_sub(previous.timestamp_ns).max(0);
            let Ok(duration_in_track_timescale) = u32::try_from(duration_ns) else {
                // The video timescale is 1 GHz, so a `trun` duration spans at
                // most 4.29 s. Writing the wrapped value would misplace every
                // later sample on the track rather than lose one.
                let refusal = format!(
                    "a frame on `{}` arrived {duration_ns} ns after its predecessor, past the \
                     {} ns a 32-bit sample duration can name at the 1 GHz video timescale",
                    self.tracks[track_index].inbound_link_name,
                    u32::MAX
                );
                self.latch_track(track_index, refusal);
                return Ok(());
            };
            self.tracks[track_index]
                .samples_awaiting_fragment
                .push(Mp4SampleAwaitingFragment {
                    sample_bytes: previous.sample_bytes,
                    duration_in_track_timescale,
                    is_sync_point: previous.is_sync_point,
                });
        }

        if frame.is_sync_point && self.is_pacing_video_track(track_index) {
            self.close_open_fragment()?;
        }

        self.tracks[track_index].held_back_video_sample = Some(HeldBackVideoSample {
            sample_bytes: split.length_prefixed_sample_bytes,
            timestamp_ns,
            is_sync_point: frame.is_sync_point,
        });
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

    /// Whether any video track is still recording and so still pacing closes.
    ///
    /// A latched track paces nothing: once the last healthy video track stops,
    /// the once-a-second rule has to take over or no fragment ever closes again
    /// and every other track's samples sit in memory until teardown — which
    /// this module's whole layout exists because it cannot rely on.
    fn no_video_track_is_wired(&self) -> bool {
        !self.tracks.iter().any(|track| {
            matches!(track.media, Some(Mp4TrackMedia::Video(_))) && !track.is_latched()
        })
    }

    /// The first video track still recording paces fragment closes; if it
    /// latches, pacing moves to the next healthy one.
    ///
    /// One pacer, not a rendezvous across all of them: with two cameras on
    /// independent sync schedules, only the pacer's fragments are guaranteed to
    /// open on a sync sample, and a second camera's `traf` may open mid-GOP.
    /// Waiting for every video track to reach a sync point together would bound
    /// nothing — two free-running encoders need never agree — so it is a
    /// deliberate limit rather than an oversight. Whether a recording owes
    /// every video track aligned random-access points is a plan question; the
    /// single-camera showcase this rung is for does not raise it.
    fn is_pacing_video_track(&self, track_index: usize) -> bool {
        self.tracks
            .iter()
            .position(|track| {
                matches!(track.media, Some(Mp4TrackMedia::Video(_))) && !track.is_latched()
            })
            .is_some_and(|pacing| pacing == track_index)
    }

    /// Bytes of samples held across every track, none of them writable until
    /// `moov` lands.
    fn bytes_held_awaiting_the_header(&self) -> usize {
        self.tracks
            .iter()
            .map(|track| {
                track
                    .samples_awaiting_fragment
                    .iter()
                    .map(|sample| sample.sample_bytes.len())
                    .sum::<usize>()
                    + track
                        .held_back_video_sample
                        .as_ref()
                        .map_or(0, |held| held.sample_bytes.len())
            })
            .sum()
    }

    /// Latch every link that has not delivered a first sync point, so the
    /// tracks that did can be described and the held samples can be written.
    fn latch_links_still_silent_at_the_budget(&mut self) {
        let bytes_held = self.bytes_held_awaiting_the_header();
        for index in 0..self.tracks.len() {
            if self.tracks[index].sample_entry.is_some() || self.tracks[index].is_latched() {
                continue;
            }
            let refusal = format!(
                "`{}` had not delivered a first sync-point bag by the time {bytes_held} bytes \
                 were held waiting for it, and nothing can be written until every track can be \
                 described — this link records nothing and the rest of the file proceeds",
                self.tracks[index].inbound_link_name
            );
            self.latch_track(index, refusal);
        }
    }

    /// `ftyp` + `moov`, once every track has delivered a sync point.
    fn write_header_once_every_track_can_be_described(&mut self) -> Result<()> {
        if self.header_already_written {
            return Ok(());
        }
        if self.bytes_held_awaiting_the_header() > HIGHEST_BYTES_HELD_AWAITING_THE_HEADER {
            self.latch_links_still_silent_at_the_budget();
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
                // §8.2.2 fixes 1.0 as normal playback and full volume; the
                // derived `Default` is 0 for both, which misdescribes the file.
                rate: FixedPoint::new(1, 0),
                volume: FixedPoint::new(1, 0),
                timescale: VIDEO_TRACK_TIMESCALE_HZ,
                // A fragmented file's duration is not known while it is being
                // written, and `mehd` is optional precisely so it need not be.
                duration: 0,
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
            let (coded_width, coded_height) = track.coded_extent.unwrap_or((0, 0));
            let track_volume = match media {
                Mp4TrackMedia::Video(_) => 0,
                Mp4TrackMedia::Audio => 1,
            };
            moov.trak.push(Trak {
                tkhd: Tkhd {
                    track_id: track.track_id,
                    enabled: true,
                    in_movie: true,
                    duration: 0,
                    // §8.3.2.3: a visual track states its presentation size and
                    // carries no volume; an audio track is the other way round.
                    // The extent is a `u16` here as it is in the sample entry,
                    // and one that would not fit was refused when the entry was
                    // built — a track only reaches this point with an extent
                    // both boxes can name.
                    width: FixedPoint::new(coded_width as u16, 0),
                    height: FixedPoint::new(coded_height as u16, 0),
                    volume: FixedPoint::new(track_volume, 0),
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
                            // A fragmented movie has no chunks in its `moov`, so
                            // this table is empty — but ISO/IEC 14496-12 §8.7.5
                            // states the box as mandatory with no exception for
                            // an empty one, and a reader that enforces it
                            // refuses the whole file without it.
                            stco: Some(Stco {
                                entries: Vec::new(),
                            }),
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
        // §8.2.2.3 wants a value larger than every track id in use, which is
        // the largest id plus one and not the number of links: a link latched
        // before it named a codec gets no `trak`, and ids need not run 1..n.
        moov.mvhd.next_track_id = moov
            .trak
            .iter()
            .map(|trak| trak.tkhd.track_id.saturating_add(1))
            .max()
            .unwrap_or(1);
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
            let written: u64 = self.tracks[index]
                .samples_awaiting_fragment
                .iter()
                .map(|sample| u64::from(sample.duration_in_track_timescale))
                .sum();
            self.tally.samples_written += self.tracks[index].samples_awaiting_fragment.len() as u64;
            self.tracks[index].next_fragment_decode_time_in_track_timescale += written;
            if let Some(last) = self.tracks[index].samples_awaiting_fragment.last() {
                self.tracks[index].last_written_sample_duration_in_track_timescale =
                    Some(last.duration_in_track_timescale);
            }
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
            // A run whose last frame is a sync point closed a fragment on the
            // way in, so the pending list is empty and the predecessor is the
            // last sample that fragment carried.
            let predecessor_duration = self.tracks[index]
                .samples_awaiting_fragment
                .last()
                .map(|sample| sample.duration_in_track_timescale)
                .or(self.tracks[index].last_written_sample_duration_in_track_timescale)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoded_audio_packet::{EncodedAudioCodec, EncodedAudioPacket};
    use crate::encoded_video_frame::EncodedVideoFrame;
    use mp4_atom::{Any, DecodeMaybe};

    /// Re-parse a written file, which is what every assertion below reads.
    ///
    /// `cargo xtask mp4-inspect` does not come through here — `xtask` does not
    /// depend on this crate and walks the boxes itself.
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

    fn opus_bag_of_size(sequence_index: u64, channels: u32, packet_bytes: usize) -> Vec<u8> {
        rmp_serde::to_vec_named(&EncodedAudioPacket {
            codec: EncodedAudioCodec::Opus,
            opus_packet_bytes: vec![0xFC; packet_bytes],
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

    fn write_video_and_audio_tracks(frames: usize) -> Vec<u8> {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "microphone/audio".to_string()],
        );
        for index in 0..frames {
            writer
                .accept_bag(
                    "camera/video",
                    &h264_bag(index as u64, index % 4 == 0, H264_SEQUENCE_PARAMETER_SET),
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
        writer.finish().expect("closes");
        file
    }

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

    /// The regression that shipped: every test in this module reads what the
    /// writer wrote with the same crate the writer wrote it with, so a box
    /// `mp4-atom` models as optional and the spec makes mandatory is invisible
    /// on both sides. An independent parser is the only thing that catches it.
    #[test]
    fn an_independent_parser_accepts_the_written_file() {
        let (file, _) = write_one_video_track(4);

        let size = file.len() as u64;
        let reader = mp4::Mp4Reader::read_header(std::io::Cursor::new(file), size)
            .expect("an independent ISOBMFF parser reads the whole file");

        assert_eq!(
            reader.tracks().len(),
            1,
            "the independent parser finds the track the writer described"
        );

        let two_track_file = write_video_and_audio_tracks(12);
        let size = two_track_file.len() as u64;
        let reader = mp4::Mp4Reader::read_header(std::io::Cursor::new(two_track_file), size)
            .expect("an independent parser reads a file with both track kinds");
        assert_eq!(reader.tracks().len(), 2);
        for track in reader.tracks().values() {
            assert_eq!(
                track
                    .trak
                    .mdia
                    .minf
                    .stbl
                    .stco
                    .as_ref()
                    .map(|stco| stco.entries.len()),
                Some(0),
                "the box is present and its table is empty, as a fragmented movie's is"
            );
        }
    }

    /// The mirror of the acceptance above: strip the box back out and the same
    /// independent parser refuses the file, which is what makes it an oracle.
    #[test]
    fn an_independent_parser_refuses_the_written_movie_header_with_its_chunk_offset_box_removed() {
        let (file, _) = write_one_video_track(4);
        let atoms = parse_written_atoms(&file).expect("the file re-parses");
        let Any::Ftyp(ftyp) = &atoms[0] else {
            panic!("the file opens with ftyp, got {:?}", atoms[0]);
        };

        let mut header_only = Vec::new();
        ftyp.encode(&mut header_only).expect("ftyp re-encodes");
        only_moov(&atoms)
            .encode(&mut header_only)
            .expect("moov re-encodes");
        let header_only_size = header_only.len() as u64;
        mp4::Mp4Reader::read_header(std::io::Cursor::new(header_only), header_only_size).expect(
            "ftyp and moov alone carry the parser as far as the check, no fragments needed",
        );

        let mut moov_without_the_chunk_offset_box = only_moov(&atoms).clone();
        for trak in &mut moov_without_the_chunk_offset_box.trak {
            trak.mdia.minf.stbl.stco = None;
        }
        let mut without_the_chunk_offset_box = Vec::new();
        ftyp.encode(&mut without_the_chunk_offset_box)
            .expect("ftyp re-encodes");
        moov_without_the_chunk_offset_box
            .encode(&mut without_the_chunk_offset_box)
            .expect("moov re-encodes");
        let without_the_box_size = without_the_chunk_offset_box.len() as u64;

        let refusal = mp4::Mp4Reader::read_header(
            std::io::Cursor::new(without_the_chunk_offset_box),
            without_the_box_size,
        )
        .expect_err("the independent parser refuses a stbl carrying neither stco nor co64");
        assert!(
            matches!(
                refusal,
                mp4::Error::Box2NotFound(mp4::BoxType::StcoBox, mp4::BoxType::Co64Box)
            ),
            "the refusal names the box the writer is on the hook for, got {refusal:?}"
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
        assert_eq!(
            only_moov(&atoms).mvhd.next_track_id,
            2,
            "§8.2.2.3 wants a value larger than every track id in use, and only track 1 is in \
             use — the link count would name 3 and describe an id nothing carries"
        );
    }

    #[test]
    fn the_movie_header_names_a_track_id_larger_than_every_one_the_movie_carries() {
        let file = write_video_and_audio_tracks(4);
        let atoms = parse_written_atoms(&file).expect("re-parses");
        let moov = only_moov(&atoms);

        let largest_track_id_in_use = moov
            .trak
            .iter()
            .map(|trak| trak.tkhd.track_id)
            .max()
            .expect("the movie carries tracks");
        assert!(
            moov.mvhd.next_track_id > largest_track_id_in_use,
            "§8.2.2.3: next_track_id {} must exceed the largest id in use {largest_track_id_in_use}",
            moov.mvhd.next_track_id
        );
    }

    /// Every top-level box start, which is what `data_offset` is measured from.
    fn top_level_box_offsets(file: &[u8]) -> Vec<(usize, [u8; 4], usize)> {
        let mut offsets = Vec::new();
        let mut at = 0usize;
        while at + 8 <= file.len() {
            let size = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = file[at + 4..at + 8].try_into().unwrap();
            offsets.push((at, kind, size));
            at += size;
        }
        offsets
    }

    #[test]
    fn every_truns_data_offset_points_at_that_tracks_samples_in_order() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "microphone/audio".to_string()],
        );
        for index in 0..12usize {
            writer
                .accept_bag(
                    "camera/video",
                    &h264_bag(index as u64, index % 4 == 0, H264_SEQUENCE_PARAMETER_SET),
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
        writer.finish().expect("closes");

        // This is the computation a player performs and the one nothing else
        // here exercises: locate each track's samples at `moof_start +
        // data_offset` and check the bytes are that track's, in order.
        let boxes = top_level_box_offsets(&file);
        let mut checked_truns = 0;
        for (box_index, (box_offset, kind, _)) in boxes.iter().enumerate() {
            if kind != b"moof" {
                continue;
            }
            let (_, _, moof_size) = boxes[box_index];
            let mut moof_cursor = std::io::Cursor::new(&file[*box_offset..*box_offset + moof_size]);
            let Ok(Some(Any::Moof(moof))) = Any::decode_maybe(&mut moof_cursor) else {
                panic!("a moof at {box_offset} must re-parse");
            };
            for traf in &moof.traf {
                for trun in &traf.trun {
                    let data_offset = trun.data_offset.expect("every trun names its offset");
                    let mut at = *box_offset + data_offset as usize;
                    for entry in &trun.entries {
                        let size = entry.size.expect("every entry names its size") as usize;
                        assert!(
                            at + size <= file.len(),
                            "trun for track {} points past the end of the file",
                            traf.tfhd.track_id
                        );
                        let sample = &file[at..at + size];
                        // A video sample opens with its own 4-byte NAL length;
                        // an Opus packet opens with the bytes the bag carried.
                        if traf.tfhd.track_id == 1 {
                            let first_nal_length =
                                u32::from_be_bytes(sample[0..4].try_into().unwrap()) as usize;
                            assert_eq!(
                                first_nal_length + 4,
                                size,
                                "a video sample is exactly its length-prefixed NALs"
                            );
                        } else {
                            assert_eq!(sample[0], 0xFC, "an Opus packet starts with its own bytes");
                        }
                        at += size;
                        checked_truns += 1;
                    }
                }
            }
        }
        assert!(
            checked_truns >= 12,
            "the walk reached {checked_truns} samples, so it proved nothing"
        );
    }

    #[test]
    fn every_fragment_after_the_first_opens_on_a_sync_sample() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["camera/video".to_string()]);
        for index in 0..18usize {
            writer
                .accept_bag(
                    "camera/video",
                    &h264_bag(index as u64, index % 6 == 0, H264_SEQUENCE_PARAMETER_SET),
                    index as i64 * ONE_VIDEO_FRAME_NS,
                )
                .expect("accepted");
        }
        writer.finish().expect("closes");

        let atoms = parse_written_atoms(&file).expect("re-parses");
        let fragments = every_moof(&atoms);
        assert!(fragments.len() >= 3, "got {} fragments", fragments.len());
        for (index, moof) in fragments.iter().enumerate() {
            let first_entry = &moof.traf[0].trun[0].entries[0];
            assert_eq!(
                first_entry.flags,
                Some(SAMPLE_FLAGS_SYNC),
                "fragment {index} opens on a non-sync sample, so it is no random-access point \
                 at all — which is the whole premise of the `cmfc` brand this file declares"
            );
        }
    }

    #[test]
    fn fragments_keep_closing_after_the_only_video_track_latches() {
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
                &h264_bag(1, true, H264_SEQUENCE_PARAMETER_SET_AT_ANOTHER_LEVEL),
                ONE_VIDEO_FRAME_NS,
            )
            .expect("the camera latches");

        // Three seconds of audio after the pacer is gone. If a latched track
        // still counted as "video wired", nothing would close and every one of
        // these would sit in memory until teardown.
        for index in 1..150u64 {
            writer
                .accept_bag(
                    "microphone/audio",
                    &opus_bag(index, 2),
                    index as i64 * ONE_OPUS_PACKET_NS,
                )
                .expect("accepted");
        }
        let tally = writer.finish().expect("closes");

        assert_eq!(tally.tracks_latched, 1);
        assert!(
            tally.fragments_written >= 3,
            "the once-a-second rule has to take over when the last video track latches, \
             got {} fragments",
            tally.fragments_written
        );
    }

    #[test]
    fn the_movie_and_track_headers_carry_the_values_the_spec_fixes() {
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
        writer.finish().expect("closes");

        let atoms = parse_written_atoms(&file).expect("re-parses");
        let moov = only_moov(&atoms);
        // §8.2.2 — normal playback rate and full volume, not the zeroes a
        // derived `Default` would leave.
        assert_eq!(moov.mvhd.rate.integer(), 1, "mvhd.rate is 1.0");
        assert_eq!(moov.mvhd.volume.integer(), 1, "mvhd.volume is 1.0");

        // §8.3.2.3 — a visual track states its presentation size and no volume.
        let video = &moov.trak[0];
        assert_eq!(video.tkhd.width.integer(), 320);
        assert_eq!(video.tkhd.height.integer(), 240);
        assert_eq!(video.tkhd.volume.integer(), 0);

        let audio = &moov.trak[1];
        assert_eq!(
            audio.tkhd.volume.integer(),
            1,
            "an audio track plays at 1.0"
        );
        assert_eq!(
            audio.tkhd.width.integer(),
            0,
            "an audio track has no extent"
        );
    }

    /// Writes the checked-in fixture `xtask` inspects, so `mp4-inspect` is
    /// exercised over bytes this writer actually produced rather than a
    /// second hand-built file. Regenerate with
    /// `STREAMLIB_WRITE_MP4_INSPECT_FIXTURE=<path> cargo test -p
    /// streamlib-media-builtins --lib the_checked_in_inspector_fixture`.
    #[test]
    fn the_checked_in_inspector_fixture_is_what_this_writer_produces() {
        let file = write_video_and_audio_tracks(12);

        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../xtask/tests/fixtures/two_track_recording.mp4");
        if std::env::var_os("STREAMLIB_WRITE_MP4_INSPECT_FIXTURE").is_some() {
            std::fs::write(&fixture_path, &file).expect("the fixture is writable");
            return;
        }
        let checked_in = std::fs::read(&fixture_path).expect("the fixture is checked in");
        assert_eq!(
            file, checked_in,
            "this writer's output moved away from the fixture `xtask mp4-inspect` reads — \
             regenerate it with STREAMLIB_WRITE_MP4_INSPECT_FIXTURE set, and read the \
             inspector's diff before trusting the new bytes"
        );
    }

    #[test]
    fn a_run_ending_on_a_sync_point_still_gives_its_last_frame_a_duration() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(&mut file, &["camera/video".to_string()]);
        // The last frame is a sync point, so the close it triggers empties the
        // pending list and `finish` has no predecessor left in it.
        for index in 0..7usize {
            writer
                .accept_bag(
                    "camera/video",
                    &h264_bag(index as u64, index % 3 == 0, H264_SEQUENCE_PARAMETER_SET),
                    index as i64 * ONE_VIDEO_FRAME_NS,
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
        assert!(
            durations.iter().all(|duration| *duration > 0),
            "a zero-duration sample makes the last fragment span no time: {durations:?}"
        );
        assert_eq!(
            *durations.last().expect("samples were written"),
            ONE_VIDEO_FRAME_NS as u32,
            "the final held-back frame takes its predecessor's duration"
        );
    }

    #[test]
    fn a_link_that_never_delivers_cannot_hold_the_others_samples_without_bound() {
        let mut file = Vec::new();
        let mut writer = Mp4FragmentedFileWriter::new(
            &mut file,
            &["camera/video".to_string(), "microphone/audio".to_string()],
        );
        // The camera never delivers a sync point, so nothing is writable — the
        // audio would otherwise accumulate for the whole run.
        let mut sequence_index = 0u64;
        while writer.tally().tracks_latched == 0 {
            writer
                .accept_bag(
                    "microphone/audio",
                    &opus_bag_of_size(sequence_index, 2, 64 * 1024),
                    sequence_index as i64 * ONE_OPUS_PACKET_NS,
                )
                .expect("accepted");
            sequence_index += 1;
            assert!(
                sequence_index < 20_000,
                "the hold is unbounded: {} bytes and still no latch",
                sequence_index * 64 * 1024
            );
        }

        let tally = writer.finish().expect("closes");
        assert_eq!(
            tally.tracks_latched, 1,
            "the silent link is latched, not the one that delivered"
        );
        let atoms = parse_written_atoms(&file).expect("the file is written at all");
        assert_eq!(
            only_moov(&atoms).trak.len(),
            1,
            "the track that did deliver is described and its samples reach the file"
        );
        assert_eq!(only_moov(&atoms).trak[0].mdia.hdlr.name, "microphone/audio");
    }
}
