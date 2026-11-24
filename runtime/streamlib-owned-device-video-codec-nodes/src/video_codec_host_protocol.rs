// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a video codec node and the machine's video codec host say over one
//! Unix socket per node: the node opens one encode or decode session, then
//! sends each frame or access unit it reads and waits for what coding it came
//! to. Each message is a little-endian `u32` length and that many bytes of
//! MessagePack; a decode's reply also carries, as `SCM_RIGHTS`, the DMA-BUF
//! of each of the host's picture buffers the session has not been sent yet.

use std::io::{self, Read, Write};
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use streamlib_consumer_rhi::h273_color_vui::H273ColorVui;
use streamlib_surface_client::{read_with_fds_attached, write_all_with_fds_attached};
use streamlib_video_bitstream::VideoCodecElementaryStream;

use crate::VideoCodecNodeIdentity;
use crate::video_decoder_settings::VideoDecoderSettings;
use crate::video_encode_on_a_device::{VideoFrameEncodeOutcome, VideoFrameToEncode};
use crate::video_encoder_settings::VideoEncoderSettings;

/// The protocol both ends speak; a session asking for another is refused.
pub const VIDEO_CODEC_HOST_PROTOCOL_VERSION: u32 = 4;

/// The longest message either end reads; a frame's access units are far
/// smaller.
const MOST_VIDEO_CODEC_HOST_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// The most picture buffers one decode's reply sends: a flush hands back at
/// most the 16 pictures an H.264 or H.265 decoded picture buffer holds and
/// the one it was decoding, each in a buffer of its own.
pub const MOST_PICTURE_BUFFERS_A_DECODE_SENDS: usize = 32;

/// What a node sends the video codec host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecHostRequest {
    /// The first message on an encoder node's connection.
    OpenEncodeSession(VideoEncodeSessionRequest),
    /// One frame to encode, answered before the next is sent.
    EncodeFrame {
        frame: VideoFrameToEncode,
        as_a_sync_point: bool,
    },
    /// The first message on a decoder node's connection.
    OpenDecodeSession(VideoDecodeSessionRequest),
    /// One access unit to decode, answered before the next is sent. Sending
    /// it gives back every picture the previous reply named.
    DecodeAccessUnit(VideoAccessUnitToDecode),
}

/// The encode session a node opens: the codec, its settings, the GPU it
/// encodes on and the surface-share service its frames are checked out from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncodeSessionRequest {
    pub protocol_version: u32,
    pub elementary_stream: VideoCodecHostElementaryStream,
    pub settings: VideoEncoderSettings,
    pub vulkan_device_uuid: [u8; 16],
    pub surface_share_socket_path: String,
}

/// The decode session a node opens: the codec, its settings and the GPU it
/// decodes on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoDecodeSessionRequest {
    pub protocol_version: u32,
    pub elementary_stream: VideoCodecHostElementaryStream,
    pub settings: VideoDecoderSettings,
    pub vulkan_device_uuid: [u8; 16],
}

/// [`VideoCodecElementaryStream`] as it crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecHostElementaryStream {
    H264,
    H265,
}

impl From<VideoCodecElementaryStream> for VideoCodecHostElementaryStream {
    fn from(elementary_stream: VideoCodecElementaryStream) -> Self {
        match elementary_stream {
            VideoCodecElementaryStream::H264 => Self::H264,
            VideoCodecElementaryStream::H265 => Self::H265,
        }
    }
}

impl VideoCodecHostElementaryStream {
    /// The encoder node that codes this elementary stream.
    pub fn encoder_node(self) -> VideoCodecNodeIdentity {
        match self {
            Self::H264 => VideoCodecNodeIdentity::H264_ENCODER,
            Self::H265 => VideoCodecNodeIdentity::H265_ENCODER,
        }
    }

    /// The decoder node that decodes this elementary stream.
    pub fn decoder_node(self) -> VideoCodecNodeIdentity {
        match self {
            Self::H264 => VideoCodecNodeIdentity::H264_DECODER,
            Self::H265 => VideoCodecNodeIdentity::H265_DECODER,
        }
    }
}

/// One Annex-B access unit to decode, and whether it starts a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoAccessUnitToDecode {
    #[serde(with = "serde_bytes")]
    pub annex_b_access_unit: Vec<u8>,
    pub group_start: Option<VideoDecodeGroupStart>,
}

/// How a node reached the group start an access unit opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoDecodeGroupStart {
    /// The stream ran on into it unbroken.
    ContinuingTheStream,
    /// The node enters the stream here, after a loss or a refused decode.
    EnteringTheStream,
}

/// What the video codec host answers each request with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecHostReply {
    /// The session is open on the named device.
    SessionOpened { device_name: String },
    /// What encoding the frame came to.
    Encode(VideoFrameEncodeOutcome),
    /// The pictures decoding the access unit completed.
    Decode(VideoAccessUnitDecoded),
    /// The request was refused, and a later one would meet the same refusal.
    Refused { refusal: String },
    /// The request was refused for want of something another client of the
    /// GPU may free, so a later one may succeed.
    RefusedForNow { refusal: String },
}

impl VideoCodecHostReply {
    /// The reply to an encode that came to `encoded`.
    pub fn of_the_encode(encoded: Result<VideoFrameEncodeOutcome, String>) -> Self {
        match encoded {
            Ok(outcome) => Self::Encode(outcome),
            Err(refusal) => Self::Refused { refusal },
        }
    }

    /// What the encode this replies to came to.
    pub fn into_the_encode(self) -> Result<VideoFrameEncodeOutcome, String> {
        match self {
            Self::Encode(outcome) => Ok(outcome),
            Self::Refused { refusal } | Self::RefusedForNow { refusal } => Err(refusal),
            Self::SessionOpened { .. } => {
                Err("the video codec host answered a frame as a session's opening".to_string())
            }
            Self::Decode(_) => {
                Err("the video codec host answered a frame with a decode".to_string())
            }
        }
    }

    /// What the decode this replies to came to.
    pub fn into_the_decode(self) -> Result<VideoAccessUnitDecoded, String> {
        match self {
            Self::Decode(decoded) => Ok(decoded),
            Self::Refused { refusal } | Self::RefusedForNow { refusal } => Err(refusal),
            Self::SessionOpened { .. } => Err(
                "the video codec host answered an access unit as a session's opening".to_string(),
            ),
            Self::Encode(_) => {
                Err("the video codec host answered an access unit with an encode".to_string())
            }
        }
    }
}

/// The pictures one access unit's decode completed, in output order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoAccessUnitDecoded {
    pub pictures: Vec<VideoPictureDecodedInTheHost>,
}

impl VideoAccessUnitDecoded {
    /// How many of the pictures arrive with their picture buffer's DMA-BUF.
    pub fn picture_buffers_sent(&self) -> usize {
        self.pictures
            .iter()
            .filter(|picture| picture.picture_buffer_is_sent_with_it)
            .count()
    }
}

/// One decoded picture: the NV12 planes it occupies in one of the host's
/// picture buffers, its display window and its colour. The host writes no
/// picture into that buffer again until the node's next request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoPictureDecodedInTheHost {
    pub picture_buffer: DecodedPictureBufferInTheHost,
    /// Whether this picture's buffer's DMA-BUF arrives with this reply, in
    /// the order of the pictures it is sent with. A buffer is sent once per
    /// session, the first time a picture lands in it.
    pub picture_buffer_is_sent_with_it: bool,
    pub width: u32,
    pub height: u32,
    pub plane_offsets_bytes: [u64; 2],
    pub plane_strides_bytes: [u64; 2],
    #[serde(with = "H273ColorVuiAsItCrosses")]
    pub color_vui: H273ColorVui,
}

/// Which of the host's picture buffers a picture is in: stable while the
/// decoded extent holds, with a new generation of buffers for each new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DecodedPictureBufferInTheHost {
    pub generation: u64,
    pub slot_index: usize,
    pub allocation_byte_size: u64,
}

/// [`H273ColorVui`]'s fields, as a frame or a picture carries them.
#[derive(Serialize, Deserialize)]
#[serde(remote = "H273ColorVui")]
pub(crate) struct H273ColorVuiAsItCrosses {
    primaries: Option<u8>,
    transfer: Option<u8>,
    matrix: Option<u8>,
    full_range: Option<bool>,
}

/// One end of a video codec host connection, framing each message it sends
/// and receives through one buffer it keeps.
pub struct VideoCodecHostConnection<Socket> {
    socket: Socket,
    message_bytes: Vec<u8>,
}

impl<Socket: Read + Write> VideoCodecHostConnection<Socket> {
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
        self.frame(message)?;
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
        self.read_the_message_after(length_prefix).map(Some)
    }

    /// Frame `message` into the connection's buffer.
    fn frame(&mut self, message: &impl Serialize) -> io::Result<()> {
        self.message_bytes.clear();
        self.message_bytes.extend_from_slice(&[0; 4]);
        rmp_serde::encode::write(&mut self.message_bytes, message)
            .map_err(|failure| io::Error::new(io::ErrorKind::InvalidData, failure))?;
        let message_length = self.message_bytes.len() - 4;
        let length_prefix = u32::try_from(message_length)
            .ok()
            .filter(|_| message_length <= MOST_VIDEO_CODEC_HOST_MESSAGE_BYTES)
            .ok_or_else(|| a_message_longer_than_the_protocol_carries(message_length))?;
        self.message_bytes[..4].copy_from_slice(&length_prefix.to_le_bytes());
        Ok(())
    }

    /// Read and decode the message `length_prefix` announced.
    fn read_the_message_after<Message: DeserializeOwned>(
        &mut self,
        length_prefix: [u8; 4],
    ) -> io::Result<Message> {
        let message_length = u32::from_le_bytes(length_prefix) as usize;
        if message_length > MOST_VIDEO_CODEC_HOST_MESSAGE_BYTES {
            return Err(a_message_longer_than_the_protocol_carries(message_length));
        }
        self.message_bytes.clear();
        self.message_bytes.resize(message_length, 0);
        self.socket.read_exact(&mut self.message_bytes)?;
        rmp_serde::from_slice(&self.message_bytes)
            .map_err(|failure| io::Error::new(io::ErrorKind::InvalidData, failure))
    }
}

impl VideoCodecHostConnection<UnixStream> {
    /// Write `message` as one framed message with `fds` attached to it.
    pub fn send_with_fds(
        &mut self,
        message: &impl Serialize,
        fds: &[BorrowedFd<'_>],
    ) -> io::Result<()> {
        self.frame(message)?;
        write_all_with_fds_attached(&self.socket, &self.message_bytes, fds)
    }

    /// Read one framed message and the fds attached to it, up to `max_fds`
    /// of them, or `None` when the other end closed between messages.
    pub fn receive_with_fds<Message: DeserializeOwned>(
        &mut self,
        max_fds: usize,
    ) -> io::Result<Option<(Message, Vec<OwnedFd>)>> {
        let mut length_prefix = [0u8; 4];
        let (bytes_read, fds) = read_with_fds_attached(&self.socket, &mut length_prefix, max_fds)?;
        if bytes_read == 0 {
            return Ok(None);
        }
        self.socket.read_exact(&mut length_prefix[bytes_read..])?;
        let message = self.read_the_message_after(length_prefix)?;
        Ok(Some((message, fds)))
    }
}

fn a_message_longer_than_the_protocol_carries(message_length: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "a {message_length}-byte message is longer than the \
             {MOST_VIDEO_CODEC_HOST_MESSAGE_BYTES} bytes the video codec host protocol carries"
        ),
    )
}
