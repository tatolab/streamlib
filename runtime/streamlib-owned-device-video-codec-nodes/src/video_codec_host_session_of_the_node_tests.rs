// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::VideoCodecNodeIdentity;
use crate::video_codec_host_protocol::{
    VIDEO_CODEC_HOST_PROTOCOL_VERSION, VideoCodecHostConnection, VideoCodecHostReply,
    VideoCodecHostRequest, VideoEncodeSessionRequest,
};
use crate::video_codec_host_session_of_the_node::{
    VIDEO_CODEC_HOST_REOPEN_INTERVAL, VideoCodecHostSession, VideoCodecHostSessionConnection,
    VideoCodecHostSessionOpening,
};
use crate::video_codec_retry_gate::VideoCodecRefusal;
use crate::video_encode_on_a_device::{VideoFrameEncodeOutcome, VideoFrameToEncode};
use crate::video_encoder_settings::VideoEncoderSettings;

const NODE: VideoCodecNodeIdentity = VideoCodecNodeIdentity::H264_ENCODER;

fn an_opening() -> VideoCodecHostSessionOpening {
    VideoCodecHostSessionOpening::Encode(VideoEncodeSessionRequest {
        protocol_version: VIDEO_CODEC_HOST_PROTOCOL_VERSION,
        elementary_stream: NODE.elementary_stream.into(),
        settings: VideoEncoderSettings::default(),
        vulkan_device_uuid: [1; 16],
        surface_share_socket_path: "/run/surface-share.sock".to_string(),
    })
}

/// Encode `frame` in `session` the way an encoder node does: as a sync
/// point on a session opened again.
fn encode(
    session: &mut VideoCodecHostSession,
    frame: VideoFrameToEncode,
) -> Result<VideoFrameEncodeOutcome, String> {
    let opened_again = session.connected()? == VideoCodecHostSessionConnection::OpenedAgain;
    let request = VideoCodecHostRequest::EncodeFrame {
        frame,
        as_a_sync_point: opened_again,
    };
    let (reply, _) = session.ask(&request, 0)?;
    reply.into_the_encode()
}

fn a_frame(timestamp_ns: i64) -> VideoFrameToEncode {
    VideoFrameToEncode {
        surface_id: "pool:0".to_string(),
        width: 64,
        height: 64,
        timestamp_ns,
        color_vui: Default::default(),
    }
}

/// A stand-in video codec host: it opens each session it is asked for, reports
/// every frame's sync-point flag on `frames_seen`, answers it as not checked
/// out, and drops a session's connection after `frames_per_session` frames.
fn a_stand_in_video_codec_host(
    listener: UnixListener,
    sessions: usize,
    frames_per_session: usize,
) -> (JoinHandle<()>, mpsc::Receiver<(usize, bool)>) {
    let (frames_seen, frames_seen_here) = mpsc::channel();
    let serving = std::thread::spawn(move || {
        for session_number in 0..sessions {
            let (socket, _) = listener.accept().unwrap();
            let mut connection = VideoCodecHostConnection::over(socket);
            let Some(VideoCodecHostRequest::OpenEncodeSession(_)) = connection.receive().unwrap()
            else {
                panic!("a session's first message opens it");
            };
            connection
                .send(&VideoCodecHostReply::SessionOpened {
                    device_name: "stand-in".to_string(),
                })
                .unwrap();
            for _ in 0..frames_per_session {
                let Some(VideoCodecHostRequest::EncodeFrame {
                    as_a_sync_point, ..
                }) = connection.receive().unwrap()
                else {
                    panic!("every later message is a frame");
                };
                frames_seen.send((session_number, as_a_sync_point)).unwrap();
                connection
                    .send(&VideoCodecHostReply::Encode(
                        VideoFrameEncodeOutcome::NotCheckedOut,
                    ))
                    .unwrap();
            }
        }
    });
    (serving, frames_seen_here)
}

#[test]
fn a_lost_session_is_opened_again_after_the_interval_and_its_first_frame_is_a_sync_point() {
    let socket_directory = tempfile::tempdir().unwrap();
    let endpoint = socket_directory.path().join("video-codec-host.sock");
    let (serving, frames_seen) =
        a_stand_in_video_codec_host(UnixListener::bind(&endpoint).unwrap(), 2, 1);
    let mut session = VideoCodecHostSession::open(NODE, &endpoint, an_opening()).unwrap();

    assert_eq!(
        encode(&mut session, a_frame(1)),
        Ok(VideoFrameEncodeOutcome::NotCheckedOut)
    );
    assert_eq!(frames_seen.recv().unwrap(), (0, false));

    let refusal = encode(&mut session, a_frame(2)).unwrap_err();
    assert!(
        refusal.contains("closed the session's connection")
            || refusal.contains("connection was lost"),
        "{refusal}"
    );
    let refusal = encode(&mut session, a_frame(3)).unwrap_err();
    assert_eq!(
        refusal,
        "H264Encoder: the video codec host's connection was lost, and it is opened again 1s \
         after the last opening",
        "a frame inside the reopen interval is refused without a connect"
    );

    std::thread::sleep(VIDEO_CODEC_HOST_REOPEN_INTERVAL);
    assert_eq!(
        encode(&mut session, a_frame(4)),
        Ok(VideoFrameEncodeOutcome::NotCheckedOut)
    );
    assert_eq!(
        frames_seen.recv().unwrap(),
        (1, true),
        "the first frame of a session opened again is a sync point"
    );
    serving.join().unwrap();
}

#[test]
fn a_refused_session_is_named_with_the_hosts_reason() {
    let socket_directory = tempfile::tempdir().unwrap();
    let endpoint = socket_directory.path().join("video-codec-host.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    let refusing = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let mut connection = VideoCodecHostConnection::over(socket);
        let _: Option<VideoCodecHostRequest> = connection.receive().unwrap();
        connection
            .send(&VideoCodecHostReply::Refused {
                refusal: "the Vulkan Video encode device was refused".to_string(),
            })
            .unwrap();
    });
    let Some(VideoCodecRefusal::ForGood(refusal)) =
        VideoCodecHostSession::open(NODE, &endpoint, an_opening()).err()
    else {
        panic!("a refused session is refused for good");
    };
    assert_eq!(
        refusal,
        "H264Encoder: the video codec host refused the session: the Vulkan Video encode device was \
         refused"
    );
    refusing.join().unwrap();
}

#[test]
fn no_host_listening_is_refused_for_now_naming_the_socket() {
    let socket_directory = tempfile::tempdir().unwrap();
    let endpoint = socket_directory.path().join("video-codec-host.sock");
    let Some(VideoCodecRefusal::ForNow(refusal)) =
        VideoCodecHostSession::open(NODE, &endpoint, an_opening()).err()
    else {
        panic!("a host not listening yet may listen at a later try");
    };
    assert!(
        refusal.starts_with(&format!(
            "H264Encoder: the video codec host at {} refused a connection",
            endpoint.display()
        )),
        "{refusal}"
    );
}

#[test]
fn a_session_the_host_refuses_for_now_is_refused_for_now() {
    let socket_directory = tempfile::tempdir().unwrap();
    let endpoint = socket_directory.path().join("video-codec-host.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    let refusing = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let mut connection = VideoCodecHostConnection::over(socket);
        let _: Option<VideoCodecHostRequest> = connection.receive().unwrap();
        connection
            .send(&VideoCodecHostReply::RefusedForNow {
                refusal: "BAR1 is full".to_string(),
            })
            .unwrap();
    });
    let Some(VideoCodecRefusal::ForNow(refusal)) =
        VideoCodecHostSession::open(NODE, &endpoint, an_opening()).err()
    else {
        panic!("a refusal for now stays one");
    };
    assert_eq!(
        refusal,
        "H264Encoder: the video codec host refused the session for now: BAR1 is full"
    );
    refusing.join().unwrap();
}
