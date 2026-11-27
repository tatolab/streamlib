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
fn open_unless_zero(
    bitrate_bps: u32,
    _another_is_open: bool,
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
    sessions.take_the_commands(&received, open_unless_zero);
    assert_eq!(first.try_recv(), Ok(Ok(())));
    assert_eq!(newest_bitrate(&mut sessions), Some(2_000_000));

    let second = open(&commands, 2, 6_000_000);
    sessions.take_the_commands(&received, open_unless_zero);
    assert_eq!(second.try_recv(), Ok(Ok(())));
    assert_eq!(newest_bitrate(&mut sessions), Some(6_000_000));
    assert!(
        sessions.is_open(1),
        "the old session stays open until its build lets it go"
    );

    close(&commands, 1);
    sessions.take_the_commands(&received, open_unless_zero);
    assert!(!sessions.is_open(1));
    assert_eq!(newest_bitrate(&mut sessions), Some(6_000_000));
}

#[test]
fn a_refused_session_is_answered_with_its_refusal_and_the_old_one_keeps_coding() {
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let _first = open(&commands, 1, 2_000_000);
    let refused = open(&commands, 2, 0);
    sessions.take_the_commands(&received, open_unless_zero);
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
    sessions.take_the_commands(&received, open_unless_zero);
    assert_eq!(newest_bitrate(&mut sessions), Some(2_000_000));
}

#[test]
fn the_opener_is_told_whether_another_session_is_open() {
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let mut told = Vec::new();
    let _first = open(&commands, 1, 2_000_000);
    let _second = open(&commands, 2, 4_000_000);
    sessions.take_the_commands(&received, |bitrate_bps, another_is_open| {
        told.push(another_is_open);
        open_unless_zero(bitrate_bps, another_is_open)
    });
    assert_eq!(told, [false, true]);
}

#[test]
fn a_session_whose_build_stopped_waiting_is_closed_by_the_close_that_follows_it() {
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let answer = open(&commands, 1, 2_000_000);
    drop(answer);
    close(&commands, 1);
    sessions.take_the_commands(&received, open_unless_zero);
    assert!(!sessions.is_open(1));
    assert_eq!(newest_bitrate(&mut sessions), None);
}

fn sessions_numbered(
    numbers_and_whether_they_take_frames: &[(u64, bool)],
) -> VideoCodingSessionsOfTheThread<SessionStandIn> {
    let (commands, received) = mpsc::channel();
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    for (number, _) in numbers_and_whether_they_take_frames {
        let _ = open(&commands, *number, *number as u32);
    }
    sessions.take_the_commands(&received, |bitrate_bps, _| {
        Ok(SessionStandIn {
            bitrate_bps,
            takes_frames: numbers_and_whether_they_take_frames
                .iter()
                .any(|(number, takes)| *number as u32 == bitrate_bps && *takes),
        })
    });
    sessions
}

fn decoding(
    sessions: &mut VideoCodingSessionsOfTheThread<SessionStandIn>,
    decoding_with: &mut Option<u64>,
    starts_group: bool,
) -> Option<u32> {
    sessions
        .the_session_decoding_a_media_frame(decoding_with, starts_group, |session| {
            session.takes_frames
        })
        .map(|session| session.bitrate_bps)
}

#[test]
fn a_decoder_stays_with_its_session_until_a_newer_one_can_take_a_group_start() {
    let mut sessions = sessions_numbered(&[(1, true), (2, true)]);
    let mut decoding_with = Some(1);
    assert_eq!(decoding(&mut sessions, &mut decoding_with, false), Some(1));
    assert_eq!(decoding_with, Some(1));
    assert_eq!(decoding(&mut sessions, &mut decoding_with, true), Some(2));
    assert_eq!(decoding_with, Some(2));
}

#[test]
fn a_newer_decoder_session_that_cannot_take_frames_yet_leaves_the_stream_with_the_old_one() {
    let mut sessions = sessions_numbered(&[(1, true), (2, false)]);
    let mut decoding_with = Some(1);
    assert_eq!(decoding(&mut sessions, &mut decoding_with, true), Some(1));
    assert_eq!(decoding_with, Some(1));
}

#[test]
fn with_no_session_decoding_the_newest_that_takes_frames_takes_any_media_frame() {
    let mut sessions = sessions_numbered(&[(1, true), (2, true)]);
    let mut decoding_with = None;
    assert_eq!(decoding(&mut sessions, &mut decoding_with, false), Some(2));
    assert_eq!(decoding_with, Some(2));

    let mut decoding_with_a_closed_session = Some(9);
    assert_eq!(
        decoding(&mut sessions, &mut decoding_with_a_closed_session, false),
        Some(2)
    );
}

#[test]
fn with_no_session_that_takes_frames_a_media_frame_is_discarded() {
    let mut sessions = sessions_numbered(&[(1, false)]);
    let mut decoding_with = None;
    assert_eq!(decoding(&mut sessions, &mut decoding_with, true), None);
    assert_eq!(decoding_with, None);
}
