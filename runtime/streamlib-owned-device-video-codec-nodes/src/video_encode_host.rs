// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The machine's encode host: one Vulkan Video encode device per GPU, shared
//! by every encoder node's session on it, so the GPU time-slices one more
//! context instead of one per encoder. Each connection is one node's session
//! on its own thread.

use std::collections::HashMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use streamlib_consumer_rhi::{OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection};

use crate::video_encode_host_protocol::{
    VIDEO_ENCODE_HOST_PROTOCOL_VERSION, VideoEncodeHostConnection, VideoEncodeHostReply,
    VideoEncodeHostRequest, VideoEncodeHostSessionRequest,
};
use crate::video_encode_on_a_device::VideoEncodeOnADevice;

/// The encode devices the host opened, one per GPU, each kept while a
/// session encodes on it.
#[derive(Default)]
pub struct VideoEncodeDevicesOfTheHost {
    devices_by_vulkan_device_uuid: Mutex<HashMap<[u8; 16], Weak<OwnedVulkanVideoCodecDevice>>>,
}

impl VideoEncodeDevicesOfTheHost {
    /// The encode device on the GPU whose `deviceUUID` is
    /// `vulkan_device_uuid`, opened by the first session on it.
    pub fn the_device_on_the_gpu(
        &self,
        vulkan_device_uuid: [u8; 16],
    ) -> Result<Arc<OwnedVulkanVideoCodecDevice>, String> {
        let mut devices = self
            .devices_by_vulkan_device_uuid
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(device) = devices.get(&vulkan_device_uuid).and_then(Weak::upgrade) {
            return Ok(device);
        }
        let device = Arc::new(
            OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
                vulkan_device_uuid,
                VulkanVideoCodecDirection::Encode,
            )
            .map_err(|refusal| format!("the Vulkan Video encode device was refused: {refusal}"))?,
        );
        tracing::info!(
            device = device.consumer_vulkan_device().name(),
            "the encode host opened an encode device"
        );
        devices.insert(vulkan_device_uuid, Arc::downgrade(&device));
        Ok(device)
    }
}

/// Serve every connection `listener` accepts as one node's session.
pub fn serve_video_encode_sessions(listener: UnixListener) {
    let devices = Arc::new(VideoEncodeDevicesOfTheHost::default());
    let mut session_number: u64 = 0;
    for connection in listener.incoming() {
        let connection = match connection {
            Ok(connection) => connection,
            Err(failure) => {
                tracing::warn!("an encode session's connection was not accepted: {failure}");
                continue;
            }
        };
        session_number += 1;
        let devices = Arc::clone(&devices);
        let served = std::thread::Builder::new()
            .name(format!("encode-session-{session_number}"))
            .spawn(move || serve_one_video_encode_session(connection, &devices));
        if let Err(failure) = served {
            tracing::warn!("encode session {session_number} could not be served: {failure}");
        }
    }
}

/// Open the session the connection's first message asks for, then encode
/// each frame it sends until it closes. An encode that finds the device lost
/// ends the host, so the runtime starts it again on a fresh device and every
/// node opens its session again.
pub fn serve_one_video_encode_session(
    connection: UnixStream,
    devices: &VideoEncodeDevicesOfTheHost,
) {
    let mut connection = VideoEncodeHostConnection::over(connection);
    let session_request = match connection.receive::<VideoEncodeHostRequest>() {
        Ok(Some(VideoEncodeHostRequest::OpenSession(session_request))) => session_request,
        Ok(Some(VideoEncodeHostRequest::EncodeFrame { .. })) => {
            refuse_the_session(
                &mut connection,
                "a frame was sent before a session was opened".to_string(),
            );
            return;
        }
        Ok(None) => return,
        Err(failure) => {
            tracing::warn!("an encode session's opening could not be read: {failure}");
            return;
        }
    };
    let mut session = match open_the_session(&session_request, devices) {
        Ok(session) => session,
        Err(refusal) => {
            refuse_the_session(&mut connection, refusal);
            return;
        }
    };
    let node_type_name = session_request
        .elementary_stream
        .encoder_node()
        .node_type_name;
    let opened = VideoEncodeHostReply::SessionOpened {
        device_name: session.device_name(),
    };
    if connection.send(&opened).is_err() {
        return;
    }
    tracing::info!("{node_type_name}: an encode session opened on the encode host");
    loop {
        let request = match connection.receive::<VideoEncodeHostRequest>() {
            Ok(Some(request)) => request,
            Ok(None) => break,
            Err(failure) => {
                tracing::warn!("{node_type_name}: a frame could not be read: {failure}");
                break;
            }
        };
        let reply = match request {
            VideoEncodeHostRequest::EncodeFrame {
                frame,
                as_a_sync_point,
            } => {
                let encoded = session.encode(&frame, as_a_sync_point);
                if let Err(refusal) = &encoded
                    && refusal.contains(DEVICE_LOST_IN_A_REFUSAL)
                {
                    end_the_host_on_a_lost_device(refusal);
                }
                VideoEncodeHostReply::of_the_encode(encoded)
            }
            VideoEncodeHostRequest::OpenSession(_) => VideoEncodeHostReply::Refused {
                refusal: "this connection's session is already open".to_string(),
            },
        };
        if let Err(failure) = connection.send(&reply) {
            tracing::warn!("{node_type_name}: an encode could not be answered: {failure}");
            break;
        }
    }
    tracing::info!("{node_type_name}: an encode session closed on the encode host");
}

/// How a refusal names a lost Vulkan device.
const DEVICE_LOST_IN_A_REFUSAL: &str = "DEVICE_LOST";

/// The status the host ends with when its device is lost.
const EXIT_STATUS_OF_A_LOST_DEVICE: i32 = 70;

fn end_the_host_on_a_lost_device(refusal: &str) -> ! {
    tracing::error!(
        "the encode device was lost, so the encode host ends and every session on it opens \
         again on the next host: {refusal}"
    );
    std::process::exit(EXIT_STATUS_OF_A_LOST_DEVICE)
}

fn open_the_session(
    session_request: &VideoEncodeHostSessionRequest,
    devices: &VideoEncodeDevicesOfTheHost,
) -> Result<VideoEncodeOnADevice, String> {
    if session_request.protocol_version != VIDEO_ENCODE_HOST_PROTOCOL_VERSION {
        return Err(format!(
            "the node speaks encode host protocol {}, and this host speaks \
             {VIDEO_ENCODE_HOST_PROTOCOL_VERSION}",
            session_request.protocol_version
        ));
    }
    let node = session_request.elementary_stream.encoder_node();
    let device = devices
        .the_device_on_the_gpu(session_request.vulkan_device_uuid)
        .map_err(|refusal| format!("{}: {refusal}", node.node_type_name))?;
    VideoEncodeOnADevice::open(
        node,
        session_request.settings,
        device,
        Path::new(&session_request.surface_share_socket_path),
    )
}

fn refuse_the_session(connection: &mut VideoEncodeHostConnection<UnixStream>, refusal: String) {
    tracing::warn!("an encode session was refused: {refusal}");
    let _ = connection.send(&VideoEncodeHostReply::Refused { refusal });
}
