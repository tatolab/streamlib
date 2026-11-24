// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{Cursor, Read, Write};

use streamlib_consumer_rhi::H273ColorVui;

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_publication::EncodedVideoAccessUnit;
use crate::video_codec_host_protocol::{
    DecodedPictureBufferInTheHost, VIDEO_CODEC_HOST_PROTOCOL_VERSION, VideoAccessUnitDecoded,
    VideoAccessUnitToDecode, VideoCodecHostConnection, VideoCodecHostReply, VideoCodecHostRequest,
    VideoDecodeGroupStart, VideoDecodeSessionRequest, VideoEncodeSessionRequest,
    VideoPictureDecodedInTheHost,
};
use crate::video_decoder_settings::VideoDecoderSettings;
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
    let mut sending = VideoCodecHostConnection::over(Cursor::new(Vec::new()));
    for message in messages {
        sending.send(message).unwrap();
    }
    let written = std::mem::take(sending.socket_mut().get_mut());
    let mut receiving = VideoCodecHostConnection::over(Cursor::new(written));
    (0..=messages.len())
        .map(|_| receiving.receive::<Message>().unwrap())
        .collect()
}

#[test]
fn a_session_and_a_frame_cross_as_they_were_sent() {
    let requests = vec![
        VideoCodecHostRequest::OpenEncodeSession(VideoEncodeSessionRequest {
            protocol_version: VIDEO_CODEC_HOST_PROTOCOL_VERSION,
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
        VideoCodecHostRequest::EncodeFrame {
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
    let VideoCodecHostRequest::OpenEncodeSession(session) = &requests[0] else {
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
        Ok(VideoFrameEncodeOutcome::DiscardedWithoutASession),
        Ok(VideoFrameEncodeOutcome::RefusedForItsFormat {
            refusal: "H264Encoder: surface \"s\" is a rgba16_float texture".to_string(),
        }),
        Err("H264Encoder: encode failed: device lost".to_string()),
    ];
    let replies: Vec<VideoCodecHostReply> = outcomes
        .iter()
        .cloned()
        .map(VideoCodecHostReply::of_the_encode)
        .collect();
    for (reply, outcome) in read_back(&replies).into_iter().zip(outcomes) {
        assert_eq!(reply.unwrap().into_the_encode(), outcome);
    }
}

#[test]
fn a_session_opening_is_not_taken_for_an_encode() {
    let refusal = VideoCodecHostReply::SessionOpened {
        device_name: "NVIDIA GeForce RTX 3090".to_string(),
    }
    .into_the_encode()
    .unwrap_err();
    assert_eq!(
        refusal,
        "the video codec host answered a frame as a session's opening"
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
    let mut connection = VideoCodecHostConnection::over(ScriptedSocket {
        incoming: Cursor::new(u32::MAX.to_le_bytes().to_vec()),
    });
    let refusal = connection.receive::<VideoCodecHostReply>().unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("longer than the 67108864 bytes the video codec host protocol carries"),
        "{refusal}"
    );
}

fn a_decoded_picture(slot_index: usize, sent_with_it: bool) -> VideoPictureDecodedInTheHost {
    VideoPictureDecodedInTheHost {
        picture_buffer: DecodedPictureBufferInTheHost {
            generation: 2,
            slot_index,
            allocation_byte_size: 3_133_440,
        },
        picture_buffer_is_sent_with_it: sent_with_it,
        width: 1920,
        height: 1080,
        plane_offsets_bytes: [0, 2_088_960],
        plane_strides_bytes: [1920, 1920],
        color_vui: H273ColorVui {
            primaries: Some(1),
            transfer: Some(1),
            matrix: Some(1),
            full_range: Some(false),
        },
    }
}

#[test]
fn a_decode_session_and_its_access_units_cross_as_they_were_sent() {
    let requests = vec![
        VideoCodecHostRequest::OpenDecodeSession(VideoDecodeSessionRequest {
            protocol_version: VIDEO_CODEC_HOST_PROTOCOL_VERSION,
            elementary_stream: VideoCodecNodeIdentity::H264_DECODER
                .elementary_stream
                .into(),
            settings: VideoDecoderSettings {
                max_width: Some(1920),
                max_height: Some(1088),
            },
            vulkan_device_uuid: [9; 16],
        }),
        VideoCodecHostRequest::DecodeAccessUnit(VideoAccessUnitToDecode {
            annex_b_access_unit: vec![0, 0, 0, 1, 0x67, 0x42],
            group_start: Some(VideoDecodeGroupStart::EnteringTheStream),
        }),
        VideoCodecHostRequest::DecodeAccessUnit(VideoAccessUnitToDecode {
            annex_b_access_unit: vec![0, 0, 0, 1, 0x41],
            group_start: None,
        }),
    ];
    let read: Vec<_> = read_back(&requests).into_iter().flatten().collect();
    assert_eq!(read, requests);
}

#[test]
fn a_decode_reply_carries_each_new_picture_buffer_as_an_fd_in_picture_order() {
    use std::io::Seek as _;
    use std::os::fd::AsFd as _;

    let (node_end, host_end) = std::os::unix::net::UnixStream::pair().unwrap();
    let memfd_with = |contents: &[u8]| {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(contents).unwrap();
        file
    };
    let first_buffer = memfd_with(b"slot 0");
    let third_buffer = memfd_with(b"slot 2");
    let decoded = VideoAccessUnitDecoded {
        pictures: vec![
            a_decoded_picture(0, true),
            a_decoded_picture(1, false),
            a_decoded_picture(2, true),
        ],
    };
    let mut host = VideoCodecHostConnection::over(host_end);
    host.send_with_fds(
        &VideoCodecHostReply::Decode(decoded.clone()),
        &[first_buffer.as_fd(), third_buffer.as_fd()],
    )
    .unwrap();
    drop(host);

    let mut node = VideoCodecHostConnection::over(node_end);
    let (reply, fds) = node
        .receive_with_fds::<VideoCodecHostReply>(decoded.picture_buffers_sent())
        .unwrap()
        .unwrap();
    assert_eq!(reply.into_the_decode().unwrap(), decoded);
    let contents: Vec<String> = fds
        .into_iter()
        .map(|fd| {
            let mut file = std::fs::File::from(fd);
            file.rewind().unwrap();
            let mut contents = String::new();
            file.read_to_string(&mut contents).unwrap();
            contents
        })
        .collect();
    assert_eq!(contents, ["slot 0", "slot 2"]);
    assert!(
        node.receive_with_fds::<VideoCodecHostReply>(0)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_decode_refused_for_now_or_for_good_comes_back_as_its_refusal() {
    for reply in [
        VideoCodecHostReply::Refused {
            refusal: "H264Decoder: decode failed".to_string(),
        },
        VideoCodecHostReply::RefusedForNow {
            refusal: "H264Decoder: decode failed".to_string(),
        },
    ] {
        assert_eq!(
            reply.into_the_decode().unwrap_err(),
            "H264Decoder: decode failed"
        );
    }
}

#[test]
fn an_encode_is_not_taken_for_a_decode() {
    let refusal = VideoCodecHostReply::Encode(VideoFrameEncodeOutcome::NotCheckedOut)
        .into_the_decode()
        .unwrap_err();
    assert_eq!(
        refusal,
        "the video codec host answered an access unit with an encode"
    );
}
