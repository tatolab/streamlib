// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Cursor;

use streamlib_consumer_rhi::H273ColorVui;

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_publication::EncodedVideoAccessUnit;
use crate::video_encode_host_protocol::{
    VIDEO_ENCODE_HOST_PROTOCOL_VERSION, VideoEncodeHostElementaryStream, VideoEncodeHostFrame,
    VideoEncodeHostReply, VideoEncodeHostRequest, VideoEncodeHostSessionRequest,
    receive_video_encode_host_message, send_video_encode_host_message,
};
use crate::video_encode_on_a_device::{
    VideoFrameEncodeOutcome, VideoFrameEncoded, VideoFrameToEncode,
};
use crate::video_encoder_settings::VideoEncoderSettings;

fn a_frame() -> VideoFrameToEncode {
    VideoFrameToEncode {
        surface_id: "pool:3".to_string(),
        width: 1920,
        height: 1080,
        timestamp_ns: -7,
        color_vui: H273ColorVui {
            primaries: Some(1),
            transfer: Some(13),
            matrix: None,
            full_range: Some(true),
        },
    }
}

#[test]
fn a_session_and_a_frame_cross_as_they_were_sent() {
    let settings = VideoEncoderSettings {
        fps: Some(30),
        bitrate_bps: Some(6_000_000),
        ..Default::default()
    };
    let requests = [
        VideoEncodeHostRequest::OpenSession(VideoEncodeHostSessionRequest {
            protocol_version: VIDEO_ENCODE_HOST_PROTOCOL_VERSION,
            elementary_stream: VideoEncodeHostElementaryStream::of_the_node(
                VideoCodecNodeIdentity::H265_ENCODER,
            ),
            settings: settings.into(),
            vulkan_device_uuid: [9; 16],
            surface_share_socket_path: "/run/surface-share.sock".to_string(),
        }),
        VideoEncodeHostRequest::EncodeFrame {
            frame: VideoEncodeHostFrame::from(&a_frame()),
            as_a_sync_point: true,
        },
    ];
    let mut socket = Vec::new();
    for request in &requests {
        send_video_encode_host_message(&mut socket, request).unwrap();
    }
    let mut socket = Cursor::new(socket);
    for request in &requests {
        let received = receive_video_encode_host_message::<VideoEncodeHostRequest>(&mut socket);
        assert_eq!(received.unwrap().as_ref(), Some(request));
    }
    assert_eq!(
        receive_video_encode_host_message::<VideoEncodeHostRequest>(&mut socket).unwrap(),
        None,
        "a socket closed between messages reads as the end"
    );

    let VideoEncodeHostRequest::OpenSession(session) = &requests[0] else {
        unreachable!()
    };
    assert_eq!(
        session.elementary_stream.encoder_node(),
        VideoCodecNodeIdentity::H265_ENCODER
    );
    assert_eq!(VideoEncoderSettings::from(session.settings), settings);
    let VideoEncodeHostRequest::EncodeFrame { frame, .. } = &requests[1] else {
        unreachable!()
    };
    assert_eq!(VideoFrameToEncode::from(frame.clone()), a_frame());
}

#[test]
fn an_encode_comes_back_as_the_host_coded_it() {
    let encoded = VideoFrameEncodeOutcome::Encoded(VideoFrameEncoded {
        access_units: vec![
            EncodedVideoAccessUnit {
                annex_b_access_unit_bytes: vec![0, 0, 0, 1, 0x67, 0x42],
                is_sync_point: true,
            },
            EncodedVideoAccessUnit {
                annex_b_access_unit_bytes: vec![0, 0, 1, 0x41],
                is_sync_point: false,
            },
        ],
        coded_extent: (1920, 1088),
        minted_a_session: true,
    });
    for outcome in [
        Ok(encoded),
        Ok(VideoFrameEncodeOutcome::NotCheckedOut),
        Ok(VideoFrameEncodeOutcome::DiscardedAfterAFailedMint),
        Err("H264Encoder: encode failed: device lost".to_string()),
    ] {
        let mut socket = Vec::new();
        send_video_encode_host_message(
            &mut socket,
            &VideoEncodeHostReply::of_the_encode(outcome.clone()),
        )
        .unwrap();
        let reply =
            receive_video_encode_host_message::<VideoEncodeHostReply>(&mut Cursor::new(socket))
                .unwrap()
                .unwrap();
        assert_eq!(reply.into_the_encode(), outcome);
    }
}

#[test]
fn a_session_opening_is_not_taken_for_an_encode() {
    let refusal = VideoEncodeHostReply::SessionOpened {
        device_name: "NVIDIA GeForce RTX 3090".to_string(),
    }
    .into_the_encode()
    .unwrap_err();
    assert_eq!(
        refusal,
        "the encode host answered a frame as a session's opening"
    );
}

#[test]
fn a_message_longer_than_the_protocol_carries_is_refused_unread() {
    let mut socket = Cursor::new(u32::MAX.to_le_bytes().to_vec());
    let refusal =
        receive_video_encode_host_message::<VideoEncodeHostReply>(&mut socket).unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("longer than the 67108864 bytes the encode host protocol carries"),
        "{refusal}"
    );
}
