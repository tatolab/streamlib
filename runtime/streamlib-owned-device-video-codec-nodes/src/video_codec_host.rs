// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The machine's video codec host: one Vulkan Video encode device and one
//! decode device per GPU, shared by every encoder and decoder node's session
//! on it, so the GPU time-slices one context per direction instead of one per
//! node, and each device's BAR1 share is taken once. Each connection is one
//! node's session on its own thread.

use std::collections::HashMap;
use std::os::fd::AsFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use streamlib_consumer_rhi::{
    ConsumerRhiError, OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection,
};

use crate::video_codec_host_protocol::{
    VIDEO_CODEC_HOST_PROTOCOL_VERSION, VideoAccessUnitToDecode, VideoCodecHostConnection,
    VideoCodecHostReply, VideoCodecHostRequest, VideoDecodeSessionRequest,
    VideoEncodeSessionRequest,
};
use crate::video_codec_retry_gate::VideoCodecRefusal;
use crate::video_decode_on_a_device::{VideoDecodeOnADevice, why_the_decode_device_was_refused};
use crate::video_encode_on_a_device::VideoEncodeOnADevice;

/// Which device a session codes on: its GPU's `deviceUUID` and the
/// direction it codes in.
type VideoCodecDeviceKey = ([u8; 16], VulkanVideoCodecDirection);

/// The devices the host opened, one per GPU and direction, each kept while a
/// session codes on it.
#[derive(Default)]
pub struct VideoCodecDevicesOfTheHost {
    devices: Mutex<HashMap<VideoCodecDeviceKey, Weak<OwnedVulkanVideoCodecDevice>>>,
}

impl VideoCodecDevicesOfTheHost {
    /// The device coding in `direction` on the GPU whose `deviceUUID` is
    /// `vulkan_device_uuid`, opened by the first session that asks for it.
    pub fn the_device_on_the_gpu(
        &self,
        vulkan_device_uuid: [u8; 16],
        direction: VulkanVideoCodecDirection,
    ) -> Result<Arc<OwnedVulkanVideoCodecDevice>, ConsumerRhiError> {
        let mut devices = self
            .devices
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(device) = devices
            .get(&(vulkan_device_uuid, direction))
            .and_then(Weak::upgrade)
        {
            return Ok(device);
        }
        let device = Arc::new(
            OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
                vulkan_device_uuid,
                direction,
            )?,
        );
        tracing::info!(
            device = device.consumer_vulkan_device().name(),
            ?direction,
            "the video codec host opened a device"
        );
        devices.insert((vulkan_device_uuid, direction), Arc::downgrade(&device));
        Ok(device)
    }
}

/// Serve every connection `listener` accepts as one node's session.
pub fn serve_video_codec_sessions(listener: UnixListener) {
    let devices = Arc::new(VideoCodecDevicesOfTheHost::default());
    let mut session_number: u64 = 0;
    for connection in listener.incoming() {
        let connection = match connection {
            Ok(connection) => connection,
            Err(failure) => {
                tracing::warn!("a codec session's connection was not accepted: {failure}");
                continue;
            }
        };
        session_number += 1;
        let devices = Arc::clone(&devices);
        let served = std::thread::Builder::new()
            .name(format!("codec-session-{session_number}"))
            .spawn(move || serve_one_video_codec_session(connection, &devices));
        if let Err(failure) = served {
            tracing::warn!("codec session {session_number} could not be served: {failure}");
        }
    }
}

/// One node's open session, on the heap: a decoder's state is large.
enum VideoCodecSessionInTheHost {
    Encode(Box<VideoEncodeOnADevice>),
    Decode(Box<VideoDecodeOnADevice>),
}

/// Open the session the connection's first message asks for, then answer
/// each frame or access unit it sends until it closes. A session that finds
/// its device lost ends the host, so the runtime starts it again on fresh
/// devices and every node opens its session again.
pub fn serve_one_video_codec_session(connection: UnixStream, devices: &VideoCodecDevicesOfTheHost) {
    let mut connection = VideoCodecHostConnection::over(connection);
    let opening = match connection.receive::<VideoCodecHostRequest>() {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(failure) => {
            tracing::warn!("a codec session's opening could not be read: {failure}");
            return;
        }
    };
    let (node_type_name, opened) = match opening {
        VideoCodecHostRequest::OpenEncodeSession(session_request) => (
            session_request
                .elementary_stream
                .encoder_node()
                .node_type_name,
            open_an_encode_session(&session_request, devices)
                .map(|encode| VideoCodecSessionInTheHost::Encode(Box::new(encode))),
        ),
        VideoCodecHostRequest::OpenDecodeSession(session_request) => (
            session_request
                .elementary_stream
                .decoder_node()
                .node_type_name,
            open_a_decode_session(&session_request, devices)
                .map(|decode| VideoCodecSessionInTheHost::Decode(Box::new(decode))),
        ),
        VideoCodecHostRequest::EncodeFrame { .. } => {
            refuse_the_session(
                &mut connection,
                VideoCodecRefusal::ForGood("a frame was sent before a session was opened".into()),
            );
            return;
        }
        VideoCodecHostRequest::DecodeAccessUnit(_) => {
            refuse_the_session(
                &mut connection,
                VideoCodecRefusal::ForGood(
                    "an access unit was sent before a session was opened".into(),
                ),
            );
            return;
        }
    };
    let mut session = match opened {
        Ok(session) => session,
        Err(refusal) => {
            refuse_the_session(&mut connection, refusal);
            return;
        }
    };
    let device_name = match &session {
        VideoCodecSessionInTheHost::Encode(encode) => encode.device_name(),
        VideoCodecSessionInTheHost::Decode(decode) => decode.device_name(),
    };
    if connection
        .send(&VideoCodecHostReply::SessionOpened { device_name })
        .is_err()
    {
        return;
    }
    tracing::info!("{node_type_name}: a session opened on the video codec host");
    loop {
        let request = match connection.receive::<VideoCodecHostRequest>() {
            Ok(Some(request)) => request,
            Ok(None) => break,
            Err(failure) => {
                tracing::warn!("{node_type_name}: a request could not be read: {failure}");
                break;
            }
        };
        let answered = match (&mut session, request) {
            (
                VideoCodecSessionInTheHost::Encode(encode),
                VideoCodecHostRequest::EncodeFrame {
                    frame,
                    as_a_sync_point,
                },
            ) => {
                let encoded = encode.encode(&frame, as_a_sync_point);
                if let Err(refusal) = &encoded {
                    end_the_host_if_the_device_was_lost(refusal);
                }
                connection.send(&VideoCodecHostReply::of_the_encode(encoded))
            }
            (
                VideoCodecSessionInTheHost::Decode(decode),
                VideoCodecHostRequest::DecodeAccessUnit(access_unit),
            ) => answer_a_decode(&mut connection, decode, &access_unit),
            (_, VideoCodecHostRequest::OpenEncodeSession(_))
            | (_, VideoCodecHostRequest::OpenDecodeSession(_)) => {
                connection.send(&VideoCodecHostReply::Refused {
                    refusal: "this connection's session is already open".to_string(),
                })
            }
            (VideoCodecSessionInTheHost::Encode(_), VideoCodecHostRequest::DecodeAccessUnit(_)) => {
                connection.send(&VideoCodecHostReply::Refused {
                    refusal: "an encode session decodes no access unit".to_string(),
                })
            }
            (VideoCodecSessionInTheHost::Decode(_), VideoCodecHostRequest::EncodeFrame { .. }) => {
                connection.send(&VideoCodecHostReply::Refused {
                    refusal: "a decode session encodes no frame".to_string(),
                })
            }
        };
        if let Err(failure) = answered {
            tracing::warn!("{node_type_name}: a request could not be answered: {failure}");
            break;
        }
    }
    tracing::info!("{node_type_name}: a session closed on the video codec host");
}

/// Decode `access_unit` and send what it came to, with the DMA-BUF of each
/// picture buffer the session has not been sent yet.
fn answer_a_decode(
    connection: &mut VideoCodecHostConnection<UnixStream>,
    decode: &mut VideoDecodeOnADevice,
    access_unit: &VideoAccessUnitToDecode,
) -> std::io::Result<()> {
    match decode.decode(access_unit) {
        Ok(to_send) => {
            let fds: Vec<_> = to_send
                .picture_buffer_dma_bufs
                .iter()
                .map(|dma_buf| dma_buf.as_fd())
                .collect();
            connection.send_with_fds(&VideoCodecHostReply::Decode(to_send.decoded), &fds)
        }
        Err(refusal) => {
            end_the_host_if_the_device_was_lost(&refusal);
            connection.send(&VideoCodecHostReply::Refused { refusal })
        }
    }
}

/// How a refusal names a lost Vulkan device.
const DEVICE_LOST_IN_A_REFUSAL: &str = "DEVICE_LOST";

/// The status the host ends with when a device is lost.
const EXIT_STATUS_OF_A_LOST_DEVICE: i32 = 70;

fn end_the_host_if_the_device_was_lost(refusal: &str) {
    if refusal.contains(DEVICE_LOST_IN_A_REFUSAL) {
        tracing::error!(
            "a video codec device was lost, so the video codec host ends and every session on \
             it opens again on the next host: {refusal}"
        );
        std::process::exit(EXIT_STATUS_OF_A_LOST_DEVICE)
    }
}

/// Refused unless the node speaks this host's protocol.
fn refuse_another_protocol(protocol_version: u32) -> Result<(), VideoCodecRefusal> {
    if protocol_version == VIDEO_CODEC_HOST_PROTOCOL_VERSION {
        return Ok(());
    }
    Err(VideoCodecRefusal::ForGood(format!(
        "the node speaks video codec host protocol {protocol_version}, and this host speaks \
         {VIDEO_CODEC_HOST_PROTOCOL_VERSION}"
    )))
}

fn open_an_encode_session(
    session_request: &VideoEncodeSessionRequest,
    devices: &VideoCodecDevicesOfTheHost,
) -> Result<VideoEncodeOnADevice, VideoCodecRefusal> {
    refuse_another_protocol(session_request.protocol_version)?;
    let node = session_request.elementary_stream.encoder_node();
    let device = devices
        .the_device_on_the_gpu(
            session_request.vulkan_device_uuid,
            VulkanVideoCodecDirection::Encode,
        )
        .map_err(|refusal| {
            VideoCodecRefusal::ForGood(format!(
                "{}: the Vulkan Video encode device was refused: {refusal}",
                node.node_type_name
            ))
        })?;
    VideoEncodeOnADevice::open(
        node,
        session_request.settings,
        device,
        Path::new(&session_request.surface_share_socket_path),
    )
    .map_err(VideoCodecRefusal::ForGood)
}

fn open_a_decode_session(
    session_request: &VideoDecodeSessionRequest,
    devices: &VideoCodecDevicesOfTheHost,
) -> Result<VideoDecodeOnADevice, VideoCodecRefusal> {
    refuse_another_protocol(session_request.protocol_version)?;
    let node = session_request.elementary_stream.decoder_node();
    let device = devices
        .the_device_on_the_gpu(
            session_request.vulkan_device_uuid,
            VulkanVideoCodecDirection::Decode,
        )
        .map_err(|refusal| why_the_decode_device_was_refused(node.node_type_name, refusal))?;
    VideoDecodeOnADevice::open(node, session_request.settings, device)
}

fn refuse_the_session(
    connection: &mut VideoCodecHostConnection<UnixStream>,
    refusal: VideoCodecRefusal,
) {
    let reply = match refusal {
        VideoCodecRefusal::ForNow(refusal) => {
            tracing::warn!("a codec session was refused for now: {refusal}");
            VideoCodecHostReply::RefusedForNow { refusal }
        }
        VideoCodecRefusal::ForGood(refusal) => {
            tracing::warn!("a codec session was refused: {refusal}");
            VideoCodecHostReply::Refused { refusal }
        }
    };
    let _ = connection.send(&reply);
}
