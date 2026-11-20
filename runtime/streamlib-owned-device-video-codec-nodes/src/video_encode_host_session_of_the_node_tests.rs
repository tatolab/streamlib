// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::VideoCodecNodeIdentity;
use crate::video_encode_host_protocol::{
    VIDEO_ENCODE_HOST_PROTOCOL_VERSION, VideoEncodeHostConnection, VideoEncodeHostReply,
    VideoEncodeHostRequest, VideoEncodeHostSessionRequest,
};
use crate::video_encode_host_session_of_the_node::{
    ENCODE_HOST_REOPEN_INTERVAL, VideoEncodeHostSession,
};
use crate::video_encode_on_a_device::{VideoFrameEncodeOutcome, VideoFrameToEncode};
use crate::video_encoder_settings::VideoEncoderSettings;

const NODE: VideoCodecNodeIdentity = VideoCodecNodeIdentity::H264_ENCODER;

fn a_session_request() -> VideoEncodeHostSessionRequest {
    VideoEncodeHostSessionRequest {
        protocol_version: VIDEO_ENCODE_HOST_PROTOCOL_VERSION,
        elementary_stream: NODE.elementary_stream.into(),
        settings: VideoEncoderSettings::default(),
        vulkan_device_uuid: [1; 16],
        surface_share_socket_path: "/run/surface-share.sock".to_string(),
    }
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

/// A stand-in encode host: it opens each session it is asked for, reports
/// every frame's sync-point flag on `frames_seen`, answers it as not checked
/// out, and drops a session's connection after `frames_per_session` frames.
fn a_stand_in_encode_host(
    listener: UnixListener,
    sessions: usize,
    frames_per_session: usize,
) -> (JoinHandle<()>, mpsc::Receiver<(usize, bool)>) {
    let (frames_seen, frames_seen_here) = mpsc::channel();
    let serving = std::thread::spawn(move || {
        for session_number in 0..sessions {
            let (socket, _) = listener.accept().unwrap();
            let mut connection = VideoEncodeHostConnection::over(socket);
            let Some(VideoEncodeHostRequest::OpenSession(_)) = connection.receive().unwrap() else {
                panic!("a session's first message opens it");
            };
            connection
                .send(&VideoEncodeHostReply::SessionOpened {
                    device_name: "stand-in".to_string(),
                })
                .unwrap();
            for _ in 0..frames_per_session {
                let Some(VideoEncodeHostRequest::EncodeFrame {
                    as_a_sync_point, ..
                }) = connection.receive().unwrap()
                else {
                    panic!("every later message is a frame");
                };
                frames_seen.send((session_number, as_a_sync_point)).unwrap();
                connection
                    .send(&VideoEncodeHostReply::Encode(
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
    let endpoint = socket_directory.path().join("video-encode-host.sock");
    let (serving, frames_seen) =
        a_stand_in_encode_host(UnixListener::bind(&endpoint).unwrap(), 2, 1);
    let mut session = VideoEncodeHostSession::open(NODE, &endpoint, a_session_request()).unwrap();

    assert_eq!(
        session.encode(a_frame(1), false),
        Ok(VideoFrameEncodeOutcome::NotCheckedOut)
    );
    assert_eq!(frames_seen.recv().unwrap(), (0, false));

    let refusal = session.encode(a_frame(2), false).unwrap_err();
    assert!(
        refusal.contains("closed the session's connection")
            || refusal.contains("connection was lost"),
        "{refusal}"
    );
    let refusal = session.encode(a_frame(3), false).unwrap_err();
    assert_eq!(
        refusal, "H264Encoder: the encode host's connection was lost, so the frame was dropped",
        "a frame inside the reopen interval is refused without a connect"
    );

    std::thread::sleep(ENCODE_HOST_REOPEN_INTERVAL);
    assert_eq!(
        session.encode(a_frame(4), false),
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
    let endpoint = socket_directory.path().join("video-encode-host.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    let refusing = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let mut connection = VideoEncodeHostConnection::over(socket);
        let _: Option<VideoEncodeHostRequest> = connection.receive().unwrap();
        connection
            .send(&VideoEncodeHostReply::Refused {
                refusal: "the Vulkan Video encode device was refused".to_string(),
            })
            .unwrap();
    });
    let refusal = VideoEncodeHostSession::open(NODE, &endpoint, a_session_request())
        .err()
        .unwrap();
    assert_eq!(
        refusal,
        "H264Encoder: the encode host refused the session: the Vulkan Video encode device was \
         refused"
    );
    refusing.join().unwrap();
}

#[test]
fn no_host_listening_is_refused_naming_the_socket() {
    let socket_directory = tempfile::tempdir().unwrap();
    let endpoint = socket_directory.path().join("video-encode-host.sock");
    let refusal = VideoEncodeHostSession::open(NODE, &endpoint, a_session_request())
        .err()
        .unwrap();
    assert!(
        refusal.starts_with(&format!(
            "H264Encoder: the encode host at {} refused a connection",
            endpoint.display()
        )),
        "{refusal}"
    );
}
