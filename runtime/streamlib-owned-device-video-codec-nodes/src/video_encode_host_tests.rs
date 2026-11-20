// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::unix::net::UnixStream;

use crate::VideoCodecNodeIdentity;
use crate::video_encode_host::{VideoEncodeDevicesOfTheHost, serve_one_video_encode_session};
use crate::video_encode_host_protocol::{
    VIDEO_ENCODE_HOST_PROTOCOL_VERSION, VideoEncodeHostElementaryStream, VideoEncodeHostFrame,
    VideoEncodeHostReply, VideoEncodeHostRequest, VideoEncodeHostSessionRequest,
    receive_video_encode_host_message, send_video_encode_host_message,
};
use crate::video_encoder_settings::VideoEncoderSettings;

/// What the host answers `request`, sent first on a fresh connection.
fn the_hosts_answer_to_a_first(request: VideoEncodeHostRequest) -> Option<VideoEncodeHostReply> {
    let (mut node_end, host_end) = UnixStream::pair().unwrap();
    let serving = std::thread::spawn(move || {
        serve_one_video_encode_session(host_end, &VideoEncodeDevicesOfTheHost::default())
    });
    send_video_encode_host_message(&mut node_end, &request).unwrap();
    let reply = receive_video_encode_host_message::<VideoEncodeHostReply>(&mut node_end).unwrap();
    serving.join().unwrap();
    reply
}

#[test]
fn a_node_speaking_another_protocol_is_refused_before_any_device_opens() {
    let reply = the_hosts_answer_to_a_first(VideoEncodeHostRequest::OpenSession(
        VideoEncodeHostSessionRequest {
            protocol_version: VIDEO_ENCODE_HOST_PROTOCOL_VERSION + 1,
            elementary_stream: VideoEncodeHostElementaryStream::of_the_node(
                VideoCodecNodeIdentity::H264_ENCODER,
            ),
            settings: VideoEncoderSettings::default().into(),
            vulkan_device_uuid: [0; 16],
            surface_share_socket_path: "/nowhere".to_string(),
        },
    ));
    assert_eq!(
        reply,
        Some(VideoEncodeHostReply::Refused {
            refusal: format!(
                "the node speaks encode host protocol {}, and this host speaks \
                 {VIDEO_ENCODE_HOST_PROTOCOL_VERSION}",
                VIDEO_ENCODE_HOST_PROTOCOL_VERSION + 1
            )
        })
    );
}

#[test]
fn a_frame_sent_before_a_session_opened_is_refused() {
    let reply = the_hosts_answer_to_a_first(VideoEncodeHostRequest::EncodeFrame {
        frame: VideoEncodeHostFrame {
            surface_id: "pool:0".to_string(),
            width: 64,
            height: 64,
            timestamp_ns: 0,
            color_primaries: None,
            color_transfer: None,
            color_matrix: None,
            color_full_range: None,
        },
        as_a_sync_point: false,
    });
    assert_eq!(
        reply,
        Some(VideoEncodeHostReply::Refused {
            refusal: "a frame was sent before a session was opened".to_string()
        })
    );
}

#[test]
fn a_connection_closed_before_its_opening_ends_its_session_quietly() {
    let (node_end, host_end) = UnixStream::pair().unwrap();
    drop(node_end);
    serve_one_video_encode_session(host_end, &VideoEncodeDevicesOfTheHost::default());
}
