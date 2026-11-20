// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::unix::net::UnixStream;

use crate::VideoCodecNodeIdentity;
use crate::video_codec_host::{VideoCodecDevicesOfTheHost, serve_one_video_codec_session};
use crate::video_codec_host_protocol::{
    VIDEO_CODEC_HOST_PROTOCOL_VERSION, VideoAccessUnitToDecode, VideoCodecHostConnection,
    VideoCodecHostReply, VideoCodecHostRequest, VideoDecodeSessionRequest,
    VideoEncodeSessionRequest,
};
use crate::video_decoder_settings::VideoDecoderSettings;
use crate::video_encode_on_a_device::VideoFrameToEncode;
use crate::video_encoder_settings::VideoEncoderSettings;

/// What the host answers `request`, sent first on a fresh connection.
fn the_hosts_answer_to_a_first(request: VideoCodecHostRequest) -> Option<VideoCodecHostReply> {
    let (node_end, host_end) = UnixStream::pair().unwrap();
    let mut node_end = VideoCodecHostConnection::over(node_end);
    let serving = std::thread::spawn(move || {
        serve_one_video_codec_session(host_end, &VideoCodecDevicesOfTheHost::default())
    });
    node_end.send(&request).unwrap();
    let reply = node_end.receive::<VideoCodecHostReply>().unwrap();
    serving.join().unwrap();
    reply
}

#[test]
fn a_node_speaking_another_protocol_is_refused_before_any_device_opens() {
    let reply = the_hosts_answer_to_a_first(VideoCodecHostRequest::OpenEncodeSession(
        VideoEncodeSessionRequest {
            protocol_version: VIDEO_CODEC_HOST_PROTOCOL_VERSION + 1,
            elementary_stream: VideoCodecNodeIdentity::H264_ENCODER
                .elementary_stream
                .into(),
            settings: VideoEncoderSettings::default(),
            vulkan_device_uuid: [0; 16],
            surface_share_socket_path: "/nowhere".to_string(),
        },
    ));
    assert_eq!(
        reply,
        Some(VideoCodecHostReply::Refused {
            refusal: format!(
                "the node speaks video codec host protocol {}, and this host speaks \
                 {VIDEO_CODEC_HOST_PROTOCOL_VERSION}",
                VIDEO_CODEC_HOST_PROTOCOL_VERSION + 1
            )
        })
    );
}

#[test]
fn a_frame_sent_before_a_session_opened_is_refused() {
    let reply = the_hosts_answer_to_a_first(VideoCodecHostRequest::EncodeFrame {
        frame: VideoFrameToEncode {
            surface_id: "pool:0".to_string(),
            width: 64,
            height: 64,
            timestamp_ns: 0,
            color_vui: Default::default(),
        },
        as_a_sync_point: false,
    });
    assert_eq!(
        reply,
        Some(VideoCodecHostReply::Refused {
            refusal: "a frame was sent before a session was opened".to_string()
        })
    );
}

#[test]
fn a_connection_closed_before_its_opening_ends_its_session_quietly() {
    let (node_end, host_end) = UnixStream::pair().unwrap();
    drop(node_end);
    serve_one_video_codec_session(host_end, &VideoCodecDevicesOfTheHost::default());
}

#[test]
fn an_access_unit_sent_before_a_session_opened_is_refused() {
    let reply = the_hosts_answer_to_a_first(VideoCodecHostRequest::DecodeAccessUnit(
        VideoAccessUnitToDecode {
            annex_b_access_unit: vec![0, 0, 0, 1, 0x65],
            group_start: None,
        },
    ));
    assert_eq!(
        reply,
        Some(VideoCodecHostReply::Refused {
            refusal: "an access unit was sent before a session was opened".to_string()
        })
    );
}

#[test]
fn a_decoder_node_speaking_another_protocol_is_refused_before_any_device_opens() {
    let reply = the_hosts_answer_to_a_first(VideoCodecHostRequest::OpenDecodeSession(
        VideoDecodeSessionRequest {
            protocol_version: VIDEO_CODEC_HOST_PROTOCOL_VERSION - 1,
            elementary_stream: VideoCodecNodeIdentity::H265_DECODER
                .elementary_stream
                .into(),
            settings: VideoDecoderSettings::default(),
            vulkan_device_uuid: [0; 16],
        },
    ));
    assert_eq!(
        reply,
        Some(VideoCodecHostReply::Refused {
            refusal: format!(
                "the node speaks video codec host protocol {}, and this host speaks \
                 {VIDEO_CODEC_HOST_PROTOCOL_VERSION}",
                VIDEO_CODEC_HOST_PROTOCOL_VERSION - 1
            )
        })
    );
}
