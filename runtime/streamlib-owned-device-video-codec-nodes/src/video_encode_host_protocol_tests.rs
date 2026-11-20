// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{Cursor, Read, Write};

use streamlib_consumer_rhi::H273ColorVui;

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_publication::EncodedVideoAccessUnit;
use crate::video_encode_host_protocol::{
    VIDEO_ENCODE_HOST_PROTOCOL_VERSION, VideoEncodeHostConnection, VideoEncodeHostReply,
    VideoEncodeHostRequest, VideoEncodeHostSessionRequest,
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

/// Send each of `messages` on one connection, then read them back on
/// another over the bytes written.
fn read_back<Message>(messages: &[Message]) -> Vec<Option<Message>>
where
    Message: serde::Serialize + serde::de::DeserializeOwned,
{
    let mut sending = VideoEncodeHostConnection::over(Cursor::new(Vec::new()));
    for message in messages {
        sending.send(message).unwrap();
    }
    let written = std::mem::take(sending.socket_mut().get_mut());
    let mut receiving = VideoEncodeHostConnection::over(Cursor::new(written));
    (0..=messages.len())
        .map(|_| receiving.receive::<Message>().unwrap())
        .collect()
}

#[test]
fn a_session_and_a_frame_cross_as_they_were_sent() {
    let requests = vec![
        VideoEncodeHostRequest::OpenSession(VideoEncodeHostSessionRequest {
            protocol_version: VIDEO_ENCODE_HOST_PROTOCOL_VERSION,
            elementary_stream: VideoCodecNodeIdentity::H265_ENCODER
                .elementary_stream
                .into(),
            settings: VideoEncoderSettings {
                fps: Some(30),
                bitrate_bps: Some(6_000_000),
                ..Default::default()
            },
            vulkan_device_uuid: [9; 16],
            surface_share_socket_path: "/run/surface-share.sock".to_string(),
        }),
        VideoEncodeHostRequest::EncodeFrame {
            frame: a_frame(),
            as_a_sync_point: true,
        },
    ];
    let received = read_back(&requests);
    assert_eq!(
        &received[..2],
        &[Some(requests[0].clone()), Some(requests[1].clone())]
    );
    assert_eq!(
        received[2], None,
        "a socket closed between messages reads as the end"
    );
    let VideoEncodeHostRequest::OpenSession(session) = &requests[0] else {
        unreachable!()
    };
    assert_eq!(
        session.elementary_stream.encoder_node(),
        VideoCodecNodeIdentity::H265_ENCODER
    );
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
    let outcomes = [
        Ok(encoded),
        Ok(VideoFrameEncodeOutcome::NotCheckedOut),
        Ok(VideoFrameEncodeOutcome::DiscardedAfterAFailedMint),
        Err("H264Encoder: encode failed: device lost".to_string()),
    ];
    let replies: Vec<VideoEncodeHostReply> = outcomes
        .iter()
        .cloned()
        .map(VideoEncodeHostReply::of_the_encode)
        .collect();
    for (reply, outcome) in read_back(&replies).into_iter().zip(outcomes) {
        assert_eq!(reply.unwrap().into_the_encode(), outcome);
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

/// A socket whose reads come from `incoming` and whose writes are kept.
struct ScriptedSocket {
    incoming: Cursor<Vec<u8>>,
}

impl Read for ScriptedSocket {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.incoming.read(buffer)
    }
}

impl Write for ScriptedSocket {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_message_longer_than_the_protocol_carries_is_refused_unread() {
    let mut connection = VideoEncodeHostConnection::over(ScriptedSocket {
        incoming: Cursor::new(u32::MAX.to_le_bytes().to_vec()),
    });
    let refusal = connection.receive::<VideoEncodeHostReply>().unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("longer than the 67108864 bytes the encode host protocol carries"),
        "{refusal}"
    );
}
