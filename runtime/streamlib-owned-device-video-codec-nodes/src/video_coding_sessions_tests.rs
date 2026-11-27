// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::mpsc;

use crate::video_coding_sessions::{
    VideoCodingSessionCommand, VideoCodingSessionNotOpened, VideoCodingSessionsOfTheThread,
};

/// A session standing in for an encoder or decoder: the bitrate it was opened at, and whether
/// its decoder has opened.
#[derive(Debug, PartialEq)]
struct SessionStandIn {
    bitrate_bps: u32,
    takes_frames: bool,
}

type Answer = mpsc::Receiver<Result<(), VideoCodingSessionNotOpened>>;

fn open(
    commands: &mpsc::Sender<VideoCodingSessionCommand<u32>>,
    number: u64,
    bitrate_bps: u32,
) -> Answer {
    let (answer, answered) = mpsc::sync_channel(1);
    commands
        .send(VideoCodingSessionCommand::Open {
            number,
            settings: bitrate_bps,
            answer,
        })
        .unwrap();
    answered
}

fn close(commands: &mpsc::Sender<VideoCodingSessionCommand<u32>>, number: u64) {
    commands
        .send(VideoCodingSessionCommand::Close { number })
        .unwrap();
}

/// Opens every session it is asked for, refusing a bitrate of 0 as a codec refuses a setting.
fn codes(session: &SessionStandIn) -> bool {
    session.takes_frames
}

fn open_unless_zero(
    bitrate_bps: u32,
    _another_codes: bool,
) -> Result<SessionStandIn, VideoCodingSessionNotOpened> {
    if bitrate_bps == 0 {
        return Err(VideoCodingSessionNotOpened::Refused(
            "H264Encoder: the video codec host refused the session: bitrate_bps is 0".to_owned(),
        ));
    }
    Ok(SessionStandIn {
        bitrate_bps,
        takes_frames: true,
    })
}

fn newest_bitrate(sessions: &mut VideoCodingSessionsOfTheThread<SessionStandIn>) -> Option<u32> {
    sessions.newest_mut().map(|session| session.bitrate_bps)
}

#[test]
fn a_session_opened_beside_another_takes_every_frame_from_then_on() {
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let first = open(&commands, 1, 2_000_000);
    sessions.take_the_commands(&received, codes, open_unless_zero);
    assert_eq!(first.try_recv(), Ok(Ok(())));
    assert_eq!(newest_bitrate(&mut sessions), Some(2_000_000));

    let second = open(&commands, 2, 6_000_000);
    sessions.take_the_commands(&received, codes, open_unless_zero);
    assert_eq!(second.try_recv(), Ok(Ok(())));
    assert_eq!(newest_bitrate(&mut sessions), Some(6_000_000));
    assert!(
        sessions.is_open(1),
        "the old session stays open until its build lets it go"
    );

    close(&commands, 1);
    sessions.take_the_commands(&received, codes, open_unless_zero);
    assert!(!sessions.is_open(1));
    assert_eq!(newest_bitrate(&mut sessions), Some(6_000_000));
}

#[test]
fn a_refused_session_is_answered_with_its_refusal_and_the_old_one_keeps_coding() {
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let _first = open(&commands, 1, 2_000_000);
    let refused = open(&commands, 2, 0);
    sessions.take_the_commands(&received, codes, open_unless_zero);
    assert!(matches!(
        refused.try_recv(),
        Ok(Err(VideoCodingSessionNotOpened::Refused(refusal))) if refusal.contains("bitrate_bps is 0")
    ));
    assert!(!sessions.is_open(2));
    assert_eq!(newest_bitrate(&mut sessions), Some(2_000_000));
}

#[test]
fn closing_the_newest_session_returns_the_frames_to_the_one_before_it() {
    // A build thrown away because its input changed as it ran closes the session it opened.
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let _first = open(&commands, 1, 2_000_000);
    let _second = open(&commands, 2, 4_000_000);
    close(&commands, 2);
    sessions.take_the_commands(&received, codes, open_unless_zero);
    assert_eq!(newest_bitrate(&mut sessions), Some(2_000_000));
}

#[test]
fn the_opener_is_told_whether_another_session_is_open() {
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let mut told = Vec::new();
    let _first = open(&commands, 1, 2_000_000);
    let _second = open(&commands, 2, 4_000_000);
    sessions.take_the_commands(&received, codes, |bitrate_bps, another_codes| {
        told.push(another_codes);
        open_unless_zero(bitrate_bps, another_codes)
    });
    assert_eq!(told, [false, true]);
}

#[test]
fn a_session_whose_decoder_has_not_opened_does_not_count_as_another_coding() {
    // A decoder refused for now as the only session retries, and so does one opened beside it
    // while it decodes nothing.
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let mut told = Vec::new();
    let _first = open(&commands, 1, 2_000_000);
    let _second = open(&commands, 2, 4_000_000);
    sessions.take_the_commands(&received, codes, |bitrate_bps, another_codes| {
        told.push(another_codes);
        Ok(SessionStandIn {
            bitrate_bps,
            takes_frames: false,
        })
    });
    assert_eq!(told, [false, false]);
}

#[test]
fn a_session_whose_build_stopped_waiting_is_closed_by_the_close_that_follows_it() {
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let answer = open(&commands, 1, 2_000_000);
    drop(answer);
    close(&commands, 1);
    sessions.take_the_commands(&received, codes, open_unless_zero);
    assert!(!sessions.is_open(1));
    assert_eq!(newest_bitrate(&mut sessions), None);
}

/// A decoder's sessions and the channel its builds open and close them through.
struct DecoderSessions {
    commands: mpsc::Sender<VideoCodingSessionCommand<u32>>,
    received: mpsc::Receiver<VideoCodingSessionCommand<u32>>,
    sessions: VideoCodingSessionsOfTheThread<SessionStandIn>,
}

impl DecoderSessions {
    fn new() -> Self {
        let (commands, received) = mpsc::channel();
        Self {
            commands,
            received,
            sessions: VideoCodingSessionsOfTheThread::default(),
        }
    }

    /// Open session `number`, its decoder open or not as `takes_frames` says.
    fn open(&mut self, number: u64, takes_frames: bool) {
        let _ = open(&self.commands, number, number as u32);
        self.sessions
            .take_the_commands(&self.received, codes, |bitrate_bps, _| {
                Ok(SessionStandIn {
                    bitrate_bps,
                    takes_frames,
                })
            });
    }

    fn close(&mut self, number: u64) {
        close(&self.commands, number);
        self.sessions
            .take_the_commands(&self.received, codes, open_unless_zero);
    }

    /// The number of the session decoding a media frame.
    fn decoding(&mut self, starts_group: bool) -> Option<u32> {
        self.sessions
            .the_session_decoding_a_media_frame(starts_group, |session| session.takes_frames)
            .map(|session| session.bitrate_bps)
    }
}

#[test]
fn a_decoder_stays_with_its_session_until_a_newer_one_can_take_a_group_start() {
    let mut decoder = DecoderSessions::new();
    decoder.open(1, true);
    assert_eq!(decoder.decoding(true), Some(1));
    decoder.open(2, true);
    assert_eq!(decoder.decoding(false), Some(1));
    assert_eq!(decoder.decoding(true), Some(2));
    assert_eq!(decoder.decoding(false), Some(2));
}

#[test]
fn a_newer_decoder_session_that_cannot_take_frames_yet_leaves_the_stream_with_the_old_one() {
    let mut decoder = DecoderSessions::new();
    decoder.open(1, true);
    assert_eq!(decoder.decoding(true), Some(1));
    decoder.open(2, false);
    assert_eq!(decoder.decoding(true), Some(1));
}

#[test]
fn a_decoding_session_let_go_before_the_newer_one_takes_a_group_start_decodes_until_it_does() {
    // Mental-revert: drop a session at its close, as a build lets the old value go as soon as
    // the new one is built, and the frames before the next group start reach no decoder.
    let mut decoder = DecoderSessions::new();
    decoder.open(1, true);
    assert_eq!(decoder.decoding(true), Some(1));
    decoder.open(2, true);
    decoder.close(1);
    assert!(decoder.sessions.is_open(1));
    assert_eq!(decoder.decoding(false), Some(1));
    assert_eq!(decoder.decoding(true), Some(2));
    assert!(
        !decoder.sessions.is_open(1),
        "dropped once the newer one took over"
    );
}

#[test]
fn a_session_let_go_with_no_newer_one_waiting_is_dropped_at_its_close() {
    let mut decoder = DecoderSessions::new();
    decoder.open(1, true);
    assert_eq!(decoder.decoding(true), Some(1));
    decoder.open(2, true);
    decoder.close(1);
    decoder.close(2);
    assert!(!decoder.sessions.is_open(1));
    assert!(!decoder.sessions.is_open(2));
    assert_eq!(decoder.decoding(true), None);
}

#[test]
fn with_no_session_decoding_the_newest_that_takes_frames_takes_any_media_frame() {
    let mut decoder = DecoderSessions::new();
    decoder.open(1, true);
    decoder.open(2, true);
    assert_eq!(decoder.decoding(false), Some(2));
}

#[test]
fn once_the_session_decoding_is_closed_the_newest_takes_any_media_frame() {
    let mut decoder = DecoderSessions::new();
    decoder.open(1, true);
    decoder.open(2, true);
    assert_eq!(decoder.decoding(false), Some(2));
    decoder.close(2);
    assert_eq!(decoder.decoding(false), Some(1));
}

#[test]
fn with_no_session_that_takes_frames_a_media_frame_is_discarded() {
    let mut decoder = DecoderSessions::new();
    decoder.open(1, false);
    assert_eq!(decoder.decoding(true), None);
}
