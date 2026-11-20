// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What an encoder node and the machine's encode host say over one Unix
//! socket per node: the node opens one session, then sends each frame it
//! reads and waits for what encoding it came to. Each message is a
//! little-endian `u32` length and that many bytes of MessagePack.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use streamlib_video_bitstream::VideoCodecElementaryStream;

use crate::VideoCodecNodeIdentity;
use crate::video_encode_on_a_device::{VideoFrameEncodeOutcome, VideoFrameToEncode};
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
        frame: VideoFrameToEncode,
        as_a_sync_point: bool,
    },
}

/// The session a node opens: the codec, its settings, the GPU it encodes on
/// and the surface-share service its frames are checked out from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncodeHostSessionRequest {
    pub protocol_version: u32,
    pub elementary_stream: VideoEncodeHostElementaryStream,
    pub settings: VideoEncoderSettings,
    pub vulkan_device_uuid: [u8; 16],
    pub surface_share_socket_path: String,
}

/// [`VideoCodecElementaryStream`] as it crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoEncodeHostElementaryStream {
    H264,
    H265,
}

impl From<VideoCodecElementaryStream> for VideoEncodeHostElementaryStream {
    fn from(elementary_stream: VideoCodecElementaryStream) -> Self {
        match elementary_stream {
            VideoCodecElementaryStream::H264 => Self::H264,
            VideoCodecElementaryStream::H265 => Self::H265,
        }
    }
}

impl VideoEncodeHostElementaryStream {
    /// The encoder node that codes this elementary stream.
    pub fn encoder_node(self) -> VideoCodecNodeIdentity {
        match self {
            Self::H264 => VideoCodecNodeIdentity::H264_ENCODER,
            Self::H265 => VideoCodecNodeIdentity::H265_ENCODER,
        }
    }
}

/// What the encode host answers each request with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoEncodeHostReply {
    /// The session is open on the named device.
    SessionOpened { device_name: String },
    /// What encoding the frame came to.
    Encode(VideoFrameEncodeOutcome),
    /// The request was refused, and why.
    Refused { refusal: String },
}

impl VideoEncodeHostReply {
    /// The reply to an encode that came to `encoded`.
    pub fn of_the_encode(encoded: Result<VideoFrameEncodeOutcome, String>) -> Self {
        match encoded {
            Ok(outcome) => Self::Encode(outcome),
            Err(refusal) => Self::Refused { refusal },
        }
    }

    /// What the encode this replies to came to; a session's opening is
    /// refused.
    pub fn into_the_encode(self) -> Result<VideoFrameEncodeOutcome, String> {
        match self {
            Self::Encode(outcome) => Ok(outcome),
            Self::Refused { refusal } => Err(refusal),
            Self::SessionOpened { .. } => {
                Err("the encode host answered a frame as a session's opening".to_string())
            }
        }
    }
}

/// One end of an encode host connection, framing each message it sends and
/// receives through one buffer it keeps.
pub struct VideoEncodeHostConnection<Socket> {
    socket: Socket,
    message_bytes: Vec<u8>,
}

impl<Socket: Read + Write> VideoEncodeHostConnection<Socket> {
    pub fn over(socket: Socket) -> Self {
        Self {
            socket,
            message_bytes: Vec::new(),
        }
    }

    pub fn socket_mut(&mut self) -> &mut Socket {
        &mut self.socket
    }

    /// Write `message` as one framed message.
    pub fn send(&mut self, message: &impl Serialize) -> io::Result<()> {
        self.message_bytes.clear();
        self.message_bytes.extend_from_slice(&[0; 4]);
        rmp_serde::encode::write(&mut self.message_bytes, message)
            .map_err(|failure| io::Error::new(io::ErrorKind::InvalidData, failure))?;
        let message_length = self.message_bytes.len() - 4;
        let length_prefix = u32::try_from(message_length)
            .ok()
            .filter(|_| message_length <= MOST_VIDEO_ENCODE_HOST_MESSAGE_BYTES)
            .ok_or_else(|| a_message_longer_than_the_protocol_carries(message_length))?;
        self.message_bytes[..4].copy_from_slice(&length_prefix.to_le_bytes());
        self.socket.write_all(&self.message_bytes)
    }

    /// Read one framed message, or `None` when the other end closed between
    /// messages.
    pub fn receive<Message: DeserializeOwned>(&mut self) -> io::Result<Option<Message>> {
        let mut length_prefix = [0u8; 4];
        match self.socket.read_exact(&mut length_prefix) {
            Ok(()) => {}
            Err(failure) if failure.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(failure) => return Err(failure),
        }
        let message_length = u32::from_le_bytes(length_prefix) as usize;
        if message_length > MOST_VIDEO_ENCODE_HOST_MESSAGE_BYTES {
            return Err(a_message_longer_than_the_protocol_carries(message_length));
        }
        self.message_bytes.clear();
        self.message_bytes.resize(message_length, 0);
        self.socket.read_exact(&mut self.message_bytes)?;
        rmp_serde::from_slice(&self.message_bytes)
            .map(Some)
            .map_err(|failure| io::Error::new(io::ErrorKind::InvalidData, failure))
    }
}

fn a_message_longer_than_the_protocol_carries(message_length: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "a {message_length}-byte message is longer than the \
             {MOST_VIDEO_ENCODE_HOST_MESSAGE_BYTES} bytes the encode host protocol carries"
        ),
    )
}
