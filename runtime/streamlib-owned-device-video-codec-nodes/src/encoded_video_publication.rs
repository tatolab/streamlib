// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Publishing encoded access units on `encoded_video`: the Packets stream
//! facts a session's first sync point states, set ahead of the access unit
//! they describe, and a refused write dropping to the next sync point.

use streamlib_video_bitstream::VideoCodecElementaryStream;
use streamlib_video_bitstream::annex_b_access_unit::parameter_sets_of_annex_b_access_unit;

/// The family whose stream facts `encoded_video` states.
pub const ENCODED_VIDEO_STREAM_FACTS_FAMILY: &str = "packets";

/// The output port encoded access units are written to.
pub trait EncodedVideoOutputPort {
    /// Replace the port's stream facts of `family` with `stream_facts_json`,
    /// one JSON object in `PacketsStreamFacts`' field names.
    fn set_stream_facts(&self, family: &str, stream_facts_json: &str) -> Result<(), String>;
    /// Write one Annex-B access unit, opening a group when `starts_group`.
    fn write(
        &self,
        annex_b_access_unit: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
    ) -> Result<(), String>;
}

/// One access unit the encoder completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedVideoAccessUnit {
    pub annex_b_access_unit_bytes: Vec<u8>,
    pub is_sync_point: bool,
}

/// What publishing one frame's access units left the encoder owing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EncodedVideoPublicationOutcome {
    /// A write or the stream facts were refused, so what the session encodes
    /// next must not reference what was lost.
    pub next_frame_must_be_a_sync_point: bool,
    /// The first refusal, to be logged.
    pub refusal: Option<String>,
}

/// The publication state of one `encoded_video` port across sessions.
#[derive(Debug)]
pub struct EncodedVideoPublication {
    elementary_stream: VideoCodecElementaryStream,
    stream_facts_on_the_port: Option<String>,
    stream_facts_derived_for_this_session: bool,
    dropping_access_units_until_the_next_sync_point: bool,
    access_units_dropped_after_a_refusal: u64,
}

impl EncodedVideoPublication {
    /// The publication of a port carrying `elementary_stream` access units.
    pub fn new(elementary_stream: VideoCodecElementaryStream) -> Self {
        Self {
            elementary_stream,
            stream_facts_on_the_port: None,
            stream_facts_derived_for_this_session: false,
            dropping_access_units_until_the_next_sync_point: false,
            access_units_dropped_after_a_refusal: 0,
        }
    }

    /// A new session was minted; its first sync point states the facts again.
    pub fn session_minted(&mut self) {
        self.stream_facts_derived_for_this_session = false;
    }

    /// Access units dropped because a write before them was refused.
    pub fn access_units_dropped_after_a_refusal(&self) -> u64 {
        self.access_units_dropped_after_a_refusal
    }

    /// Publish one frame's `access_units`, stamped `timestamp_ns`, from a
    /// session whose coded extent is `coded_width` x `coded_height`.
    pub fn publish(
        &mut self,
        output_port: &dyn EncodedVideoOutputPort,
        access_units: &[EncodedVideoAccessUnit],
        timestamp_ns: i64,
        (coded_width, coded_height): (u32, u32),
    ) -> EncodedVideoPublicationOutcome {
        if let Some(stream_facts_json) = self
            .stream_facts_from_the_first_sync_point_of_this_session(
                access_units,
                coded_width,
                coded_height,
            )
            && self.stream_facts_on_the_port.as_ref() != Some(&stream_facts_json)
        {
            if let Err(refusal) =
                output_port.set_stream_facts(ENCODED_VIDEO_STREAM_FACTS_FAMILY, &stream_facts_json)
            {
                self.stream_facts_derived_for_this_session = false;
                self.dropping_access_units_until_the_next_sync_point = true;
                self.access_units_dropped_after_a_refusal += access_units.len() as u64;
                return EncodedVideoPublicationOutcome {
                    next_frame_must_be_a_sync_point: true,
                    refusal: Some(format!("the stream facts were refused: {refusal}")),
                };
            }
            self.stream_facts_on_the_port = Some(stream_facts_json);
        }

        let mut outcome = EncodedVideoPublicationOutcome::default();
        for access_unit in access_units {
            if self.dropping_access_units_until_the_next_sync_point && !access_unit.is_sync_point {
                self.access_units_dropped_after_a_refusal += 1;
                continue;
            }
            match output_port.write(
                &access_unit.annex_b_access_unit_bytes,
                timestamp_ns,
                access_unit.is_sync_point,
            ) {
                Ok(()) => self.dropping_access_units_until_the_next_sync_point = false,
                Err(refusal) => {
                    self.dropping_access_units_until_the_next_sync_point = true;
                    self.access_units_dropped_after_a_refusal += 1;
                    outcome.next_frame_must_be_a_sync_point = true;
                    outcome.refusal.get_or_insert_with(|| {
                        format!("an access unit write was refused: {refusal}")
                    });
                }
            }
        }
        outcome
    }

    fn stream_facts_from_the_first_sync_point_of_this_session(
        &mut self,
        access_units: &[EncodedVideoAccessUnit],
        coded_width: u32,
        coded_height: u32,
    ) -> Option<String> {
        if self.stream_facts_derived_for_this_session {
            return None;
        }
        let first_sync_point = access_units
            .iter()
            .find(|access_unit| access_unit.is_sync_point)?;
        self.stream_facts_derived_for_this_session = true;
        let codec = parameter_sets_of_annex_b_access_unit(
            &first_sync_point.annex_b_access_unit_bytes,
            self.elementary_stream,
        )
        .codec_string_of_the_sequence_parameter_set(self.elementary_stream)?;
        Some(format!(
            "{{\"rfc6381_codec\":\"{codec}\",\"coded_width\":{coded_width},\"coded_height\":{coded_height}}}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingOutputPort {
        stream_facts_stated: RefCell<Vec<(String, String)>>,
        writes: RefCell<Vec<(Vec<u8>, i64, bool)>>,
        refuse_writes: RefCell<bool>,
        refuse_stream_facts: RefCell<bool>,
    }

    impl EncodedVideoOutputPort for RecordingOutputPort {
        fn set_stream_facts(&self, family: &str, stream_facts_json: &str) -> Result<(), String> {
            if *self.refuse_stream_facts.borrow() {
                return Err("refused".to_owned());
            }
            self.stream_facts_stated
                .borrow_mut()
                .push((family.to_owned(), stream_facts_json.to_owned()));
            Ok(())
        }

        fn write(
            &self,
            annex_b_access_unit: &[u8],
            timestamp_ns: i64,
            starts_group: bool,
        ) -> Result<(), String> {
            if *self.refuse_writes.borrow() {
                return Err("over the channel ceiling".to_owned());
            }
            self.writes.borrow_mut().push((
                annex_b_access_unit.to_vec(),
                timestamp_ns,
                starts_group,
            ));
            Ok(())
        }
    }

    fn sync_point() -> EncodedVideoAccessUnit {
        EncodedVideoAccessUnit {
            annex_b_access_unit_bytes: vec![0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1F, 0, 0, 1, 0x65],
            is_sync_point: true,
        }
    }

    fn predicted() -> EncodedVideoAccessUnit {
        EncodedVideoAccessUnit {
            annex_b_access_unit_bytes: vec![0, 0, 0, 1, 0x41, 0x9A],
            is_sync_point: false,
        }
    }

    #[test]
    fn the_first_sync_point_states_the_stream_facts_once_before_its_access_unit() {
        let port = RecordingOutputPort::default();
        let mut publication = EncodedVideoPublication::new(VideoCodecElementaryStream::H264);

        publication.publish(&port, &[sync_point()], 1_000_000, (1280, 720));
        publication.publish(&port, &[predicted()], 2_000_000, (1280, 720));

        assert_eq!(
            *port.stream_facts_stated.borrow(),
            [(
                "packets".to_owned(),
                "{\"rfc6381_codec\":\"avc1.64001f\",\"coded_width\":1280,\"coded_height\":720}"
                    .to_owned()
            )]
        );
        let writes = port.writes.borrow();
        assert_eq!(writes.len(), 2);
        assert_eq!((writes[0].1, writes[0].2), (1_000_000, true));
        assert_eq!((writes[1].1, writes[1].2), (2_000_000, false));
        assert_eq!(writes[0].0, sync_point().annex_b_access_unit_bytes);
    }

    #[test]
    fn a_re_minted_session_with_a_new_extent_states_new_stream_facts() {
        let port = RecordingOutputPort::default();
        let mut publication = EncodedVideoPublication::new(VideoCodecElementaryStream::H264);

        publication.publish(&port, &[sync_point()], 0, (1280, 720));
        publication.session_minted();
        publication.publish(&port, &[sync_point()], 0, (640, 368));

        assert_eq!(port.stream_facts_stated.borrow().len(), 2);
    }

    #[test]
    fn a_refused_write_drops_to_the_next_sync_point_and_asks_for_one() {
        let port = RecordingOutputPort::default();
        let mut publication = EncodedVideoPublication::new(VideoCodecElementaryStream::H264);
        publication.publish(&port, &[sync_point()], 0, (1280, 720));

        *port.refuse_writes.borrow_mut() = true;
        let refused = publication.publish(&port, &[predicted()], 1, (1280, 720));
        *port.refuse_writes.borrow_mut() = false;
        let dropped = publication.publish(&port, &[predicted()], 2, (1280, 720));
        let resumed = publication.publish(&port, &[sync_point()], 3, (1280, 720));

        assert!(refused.next_frame_must_be_a_sync_point);
        assert!(refused.refusal.is_some());
        assert_eq!(dropped, EncodedVideoPublicationOutcome::default());
        assert_eq!(resumed, EncodedVideoPublicationOutcome::default());
        let stamps: Vec<i64> = port.writes.borrow().iter().map(|write| write.1).collect();
        assert_eq!(stamps, [0, 3]);
        assert_eq!(publication.access_units_dropped_after_a_refusal(), 2);
    }

    #[test]
    fn refused_stream_facts_drop_the_frame_and_are_stated_at_the_next_sync_point() {
        let port = RecordingOutputPort::default();
        let mut publication = EncodedVideoPublication::new(VideoCodecElementaryStream::H264);

        *port.refuse_stream_facts.borrow_mut() = true;
        let refused = publication.publish(&port, &[sync_point()], 0, (1280, 720));
        *port.refuse_stream_facts.borrow_mut() = false;
        publication.publish(&port, &[sync_point()], 1, (1280, 720));

        assert!(refused.next_frame_must_be_a_sync_point);
        assert_eq!(port.stream_facts_stated.borrow().len(), 1);
        let stamps: Vec<i64> = port.writes.borrow().iter().map(|write| write.1).collect();
        assert_eq!(stamps, [1]);
    }

    #[test]
    fn an_h265_port_states_the_hvc1_string_its_first_sync_point_spells() {
        let port = RecordingOutputPort::default();
        let mut publication = EncodedVideoPublication::new(VideoCodecElementaryStream::H265);
        let h265_sync_point = EncodedVideoAccessUnit {
            annex_b_access_unit_bytes: [
                &[0, 0, 0, 1][..],
                &[
                    0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03,
                    0x00, 0x00, 0x03, 0x00, 0x5D, 0xA0,
                ],
                &[0, 0, 1, 0x26, 0x01, 0xAF],
            ]
            .concat(),
            is_sync_point: true,
        };

        publication.publish(&port, &[h265_sync_point], 0, (1920, 1088));

        assert_eq!(
            *port.stream_facts_stated.borrow(),
            [(
                "packets".to_owned(),
                "{\"rfc6381_codec\":\"hvc1.1.6.L93.90\",\"coded_width\":1920,\"coded_height\":1088}"
                    .to_owned()
            )]
        );
    }
}
