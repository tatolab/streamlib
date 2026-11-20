// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What an encoder node and the machine's encode host say over one Unix
//! socket per node: the node opens one session, then sends each frame it
//! reads and waits for its access units. Each message is a little-endian
//! `u32` length and that many bytes of MessagePack.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use streamlib_consumer_rhi::H273ColorVui;
use streamlib_video_bitstream::VideoCodecElementaryStream;

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_publication::EncodedVideoAccessUnit;
use crate::video_encode_on_a_device::{
    VideoFrameEncodeOutcome, VideoFrameEncoded, VideoFrameToEncode,
};
use crate::video_encoder_settings::VideoEncoderSettings;

/// The protocol both ends speak; a session asking for another is refused.
pub const VIDEO_ENCODE_HOST_PROTOCOL_VERSION: u32 = 1;

/// The longest message either end reads; a frame's access units are far
/// smaller.
const MOST_VIDEO_ENCODE_HOST_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// What a node sends the encode host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoEncodeHostRequest {
    /// The first message on a connection.
    OpenSession(VideoEncodeHostSessionRequest),
    /// One frame to encode, answered before the next is sent.
    EncodeFrame {
        frame: VideoEncodeHostFrame,
        as_a_sync_point: bool,
    },
}

/// The session a node opens: the codec, its settings, the GPU it encodes on
/// and the surface-share service its frames are checked out from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncodeHostSessionRequest {
    pub protocol_version: u32,
    pub elementary_stream: VideoEncodeHostElementaryStream,
    pub settings: VideoEncodeHostSettings,
    pub vulkan_device_uuid: [u8; 16],
    pub surface_share_socket_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoEncodeHostElementaryStream {
    H264,
    H265,
}

impl VideoEncodeHostElementaryStream {
    pub fn of_the_node(node: VideoCodecNodeIdentity) -> Self {
        match node.elementary_stream {
            VideoCodecElementaryStream::H264 => Self::H264,
            VideoCodecElementaryStream::H265 => Self::H265,
        }
    }

    /// The encoder node that codes this elementary stream.
    pub fn encoder_node(self) -> VideoCodecNodeIdentity {
        match self {
            Self::H264 => VideoCodecNodeIdentity::H264_ENCODER,
            Self::H265 => VideoCodecNodeIdentity::H265_ENCODER,
        }
    }
}

/// [`VideoEncoderSettings`] as they cross.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncodeHostSettings {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub bitrate_bps: Option<u32>,
    pub keyframe_interval_seconds: Option<u32>,
    pub effort_level: Option<u32>,
}

impl From<VideoEncoderSettings> for VideoEncodeHostSettings {
    fn from(settings: VideoEncoderSettings) -> Self {
        Self {
            width: settings.width,
            height: settings.height,
            fps: settings.fps,
            bitrate_bps: settings.bitrate_bps,
            keyframe_interval_seconds: settings.keyframe_interval_seconds,
            effort_level: settings.effort_level,
        }
    }
}

impl From<VideoEncodeHostSettings> for VideoEncoderSettings {
    fn from(settings: VideoEncodeHostSettings) -> Self {
        Self {
            width: settings.width,
            height: settings.height,
            fps: settings.fps,
            bitrate_bps: settings.bitrate_bps,
            keyframe_interval_seconds: settings.keyframe_interval_seconds,
            effort_level: settings.effort_level,
        }
    }
}

/// [`VideoFrameToEncode`] as it crosses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncodeHostFrame {
    pub surface_id: String,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: i64,
    pub color_primaries: Option<u8>,
    pub color_transfer: Option<u8>,
    pub color_matrix: Option<u8>,
    pub color_full_range: Option<bool>,
}

impl From<&VideoFrameToEncode> for VideoEncodeHostFrame {
    fn from(frame: &VideoFrameToEncode) -> Self {
        Self {
            surface_id: frame.surface_id.clone(),
            width: frame.width,
            height: frame.height,
            timestamp_ns: frame.timestamp_ns,
            color_primaries: frame.color_vui.primaries,
            color_transfer: frame.color_vui.transfer,
            color_matrix: frame.color_vui.matrix,
            color_full_range: frame.color_vui.full_range,
        }
    }
}

impl From<VideoEncodeHostFrame> for VideoFrameToEncode {
    fn from(frame: VideoEncodeHostFrame) -> Self {
        Self {
            surface_id: frame.surface_id,
            width: frame.width,
            height: frame.height,
            timestamp_ns: frame.timestamp_ns,
            color_vui: H273ColorVui {
                primaries: frame.color_primaries,
                transfer: frame.color_transfer,
                matrix: frame.color_matrix,
                full_range: frame.color_full_range,
            },
        }
    }
}

/// What the encode host answers each request with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoEncodeHostReply {
    /// The session is open on the named device.
    SessionOpened {
        device_name: String,
    },
    Encoded {
        access_units: Vec<VideoEncodeHostAccessUnit>,
        coded_width: u32,
        coded_height: u32,
        minted_a_session: bool,
    },
    NotCheckedOut,
    DiscardedAfterAFailedMint,
    /// The request was refused, and why.
    Refused {
        refusal: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncodeHostAccessUnit {
    #[serde(with = "serde_bytes")]
    pub annex_b_access_unit_bytes: Vec<u8>,
    pub is_sync_point: bool,
}

impl VideoEncodeHostReply {
    /// The reply to an encode that came to `encoded`.
    pub fn of_the_encode(encoded: Result<VideoFrameEncodeOutcome, String>) -> Self {
        match encoded {
            Ok(VideoFrameEncodeOutcome::Encoded(encoded)) => Self::Encoded {
                coded_width: encoded.coded_extent.0,
                coded_height: encoded.coded_extent.1,
                minted_a_session: encoded.minted_a_session,
                access_units: encoded
                    .access_units
                    .into_iter()
                    .map(|access_unit| VideoEncodeHostAccessUnit {
                        annex_b_access_unit_bytes: access_unit.annex_b_access_unit_bytes,
                        is_sync_point: access_unit.is_sync_point,
                    })
                    .collect(),
            },
            Ok(VideoFrameEncodeOutcome::NotCheckedOut) => Self::NotCheckedOut,
            Ok(VideoFrameEncodeOutcome::DiscardedAfterAFailedMint) => {
                Self::DiscardedAfterAFailedMint
            }
            Err(refusal) => Self::Refused { refusal },
        }
    }

    /// What the encode this replies to came to; any other reply is refused.
    pub fn into_the_encode(self) -> Result<VideoFrameEncodeOutcome, String> {
        match self {
            Self::Encoded {
                access_units,
                coded_width,
                coded_height,
                minted_a_session,
            } => Ok(VideoFrameEncodeOutcome::Encoded(VideoFrameEncoded {
                access_units: access_units
                    .into_iter()
                    .map(|access_unit| EncodedVideoAccessUnit {
                        annex_b_access_unit_bytes: access_unit.annex_b_access_unit_bytes,
                        is_sync_point: access_unit.is_sync_point,
                    })
                    .collect(),
                coded_extent: (coded_width, coded_height),
                minted_a_session,
            })),
            Self::NotCheckedOut => Ok(VideoFrameEncodeOutcome::NotCheckedOut),
            Self::DiscardedAfterAFailedMint => {
                Ok(VideoFrameEncodeOutcome::DiscardedAfterAFailedMint)
            }
            Self::Refused { refusal } => Err(refusal),
            Self::SessionOpened { .. } => {
                Err("the encode host answered a frame as a session's opening".to_string())
            }
        }
    }
}

/// Write `message` as one framed message.
pub fn send_video_encode_host_message(
    socket: &mut impl Write,
    message: &impl Serialize,
) -> io::Result<()> {
    let encoded = rmp_serde::to_vec_named(message)
        .map_err(|failure| io::Error::new(io::ErrorKind::InvalidData, failure))?;
    if encoded.len() > MOST_VIDEO_ENCODE_HOST_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a {}-byte message is longer than the {MOST_VIDEO_ENCODE_HOST_MESSAGE_BYTES} \
                 bytes the encode host protocol carries",
                encoded.len()
            ),
        ));
    }
    let mut framed = Vec::with_capacity(4 + encoded.len());
    framed.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
    framed.extend_from_slice(&encoded);
    socket.write_all(&framed)
}

/// Read one framed message, or `None` when the other end closed between
/// messages.
pub fn receive_video_encode_host_message<Message: for<'de> Deserialize<'de>>(
    socket: &mut impl Read,
) -> io::Result<Option<Message>> {
    let mut length_prefix = [0u8; 4];
    match socket.read_exact(&mut length_prefix) {
        Ok(()) => {}
        Err(failure) if failure.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(failure) => return Err(failure),
    }
    let message_length = u32::from_le_bytes(length_prefix) as usize;
    if message_length > MOST_VIDEO_ENCODE_HOST_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a {message_length}-byte message is longer than the \
                 {MOST_VIDEO_ENCODE_HOST_MESSAGE_BYTES} bytes the encode host protocol carries"
            ),
        ));
    }
    let mut encoded = vec![0u8; message_length];
    socket.read_exact(&mut encoded)?;
    rmp_serde::from_slice(&encoded)
        .map(Some)
        .map_err(|failure| io::Error::new(io::ErrorKind::InvalidData, failure))
}
