// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! H.264 or H.265 encode with Vulkan Video: each frame is encoded in the
//! machine's encode host, on the one encode device it shares among every
//! encoder on the GPU, and its access units are published here. A runtime
//! that offers no encode host leaves the node to encode on a device of its
//! own.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use streamlib_consumer_rhi::{
    OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection,
    runtime_vulkan_device_uuid_named_at_spawn,
};
use tatolab_module_sdk::ModuleHost;

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_publication::{EncodedVideoOutputPort, EncodedVideoPublication};
use crate::image_frame_on_the_video_port::ImageFrameReadByTheEncoder;
use crate::video_encode_host_protocol::{
    VIDEO_ENCODE_HOST_PROTOCOL_VERSION, VideoEncodeHostElementaryStream,
    VideoEncodeHostSessionRequest,
};
use crate::video_encode_host_session_of_the_node::VideoEncodeHostSessionOfTheNode;
use crate::video_encode_on_a_device::{
    VideoEncodeOnADevice, VideoFrameEncodeOutcome, VideoFrameToEncode,
};
use crate::video_encoder_settings::VideoEncoderSettings;

const SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_SURFACE_SOCKET";

const ENCODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// Where a node's frames are encoded.
enum VideoEncodeOfTheNode {
    InTheEncodeHost(VideoEncodeHostSessionOfTheNode),
    OnADeviceOfItsOwn(Box<VideoEncodeOnADevice>),
}

/// A video encoder node's native half: where its frames are encoded, and the
/// publication of their access units on its `encoded_video` port.
pub struct VulkanVideoEncoder {
    node: VideoCodecNodeIdentity,
    encode: VideoEncodeOfTheNode,
    next_frame_must_be_a_sync_point: bool,
    publication: EncodedVideoPublication,
    frames_encoded: u64,
    publication_refusals: u64,
}

impl VulkanVideoEncoder {
    /// Open a session in the machine's encode host on the GPU the runtime
    /// named, or, when the runtime offers none, a device of the node's own.
    pub fn open(
        node: VideoCodecNodeIdentity,
        module_host: &ModuleHost,
        settings: VideoEncoderSettings,
    ) -> Result<Self, String> {
        let node_type_name = node.node_type_name;
        let vulkan_device_uuid = runtime_vulkan_device_uuid_named_at_spawn()
            .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        let surface_share_socket_path = std::env::var(SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE)
            .map_err(|_| {
                format!(
                    "{node_type_name}: {SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE} is not set, so \
                     no published frame can be checked out"
                )
            })?;
        let session_request = VideoEncodeHostSessionRequest {
            protocol_version: VIDEO_ENCODE_HOST_PROTOCOL_VERSION,
            elementary_stream: VideoEncodeHostElementaryStream::of_the_node(node),
            settings: settings.into(),
            vulkan_device_uuid,
            surface_share_socket_path: surface_share_socket_path.clone(),
        };
        let encode = match VideoEncodeHostSessionOfTheNode::open(node, module_host, session_request)
        {
            Ok(session) => VideoEncodeOfTheNode::InTheEncodeHost(session),
            Err(refusal) => {
                tracing::warn!("{refusal}; encoding on a device of the node's own");
                VideoEncodeOfTheNode::OnADeviceOfItsOwn(Box::new(open_a_device_of_the_nodes_own(
                    node,
                    settings,
                    vulkan_device_uuid,
                    Path::new(&surface_share_socket_path),
                )?))
            }
        };
        Ok(Self {
            node,
            encode,
            next_frame_must_be_a_sync_point: false,
            publication: EncodedVideoPublication::new(node.elementary_stream),
            frames_encoded: 0,
            publication_refusals: 0,
        })
    }

    /// Encode `frame` and publish its access units on `encoded_video`. A
    /// frame its producer recycled before the check-out is skipped; a frame
    /// arriving after a failed mint is discarded.
    pub fn encode_and_publish(
        &mut self,
        frame: &ImageFrameReadByTheEncoder,
        encoded_video: &dyn EncodedVideoOutputPort,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        let frame_to_encode = VideoFrameToEncode::of_the_frame_read(frame);
        let as_a_sync_point = self.next_frame_must_be_a_sync_point;
        let outcome = match &mut self.encode {
            VideoEncodeOfTheNode::InTheEncodeHost(session) => {
                session.encode(&frame_to_encode, as_a_sync_point)
            }
            VideoEncodeOfTheNode::OnADeviceOfItsOwn(session) => {
                session.encode(&frame_to_encode, as_a_sync_point)
            }
        }?;
        let encoded = match outcome {
            VideoFrameEncodeOutcome::Encoded(encoded) => encoded,
            VideoFrameEncodeOutcome::NotCheckedOut
            | VideoFrameEncodeOutcome::DiscardedAfterAFailedMint => return Ok(()),
        };
        self.next_frame_must_be_a_sync_point = false;
        if encoded.minted_a_session {
            self.publication.session_minted();
        }
        let published = self.publication.publish(
            encoded_video,
            &encoded.access_units,
            frame.timestamp_ns,
            encoded.coded_extent,
        );
        if published.next_frame_must_be_a_sync_point {
            self.next_frame_must_be_a_sync_point = true;
        }
        if let Some(refusal) = published.refusal {
            self.publication_refusals += 1;
            if self.publication_refusals.is_power_of_two() {
                tracing::warn!(
                    publication_refusals = self.publication_refusals,
                    access_units_dropped = self.publication.access_units_dropped_after_a_refusal(),
                    "{node_type_name}: {refusal}; dropping to the next sync point and encoding the \
                     next frame as one"
                );
            }
        }

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("{node_type_name}: first frame encoded");
        } else if self
            .frames_encoded
            .is_multiple_of(ENCODE_PROGRESS_LOG_INTERVAL_FRAMES)
        {
            tracing::info!(
                frames_encoded = self.frames_encoded,
                "{node_type_name}: encode progress"
            );
        }
        Ok(())
    }
}

fn open_a_device_of_the_nodes_own(
    node: VideoCodecNodeIdentity,
    settings: VideoEncoderSettings,
    vulkan_device_uuid: [u8; 16],
    surface_share_socket_path: &Path,
) -> Result<VideoEncodeOnADevice, String> {
    let node_type_name = node.node_type_name;
    let opening_started = Instant::now();
    let device = Arc::new(
        OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
            vulkan_device_uuid,
            VulkanVideoCodecDirection::Encode,
        )
        .map_err(|refusal| {
            format!("{node_type_name}: the owned Vulkan Video encode device was refused: {refusal}")
        })?,
    );
    tracing::info!(
        owned_device_created_ms = opening_started.elapsed().as_secs_f64() * 1_000.0,
        device = device.consumer_vulkan_device().name(),
        "{node_type_name}: encoding on an owned Vulkan Video device"
    );
    VideoEncodeOnADevice::open(node, settings, device, surface_share_socket_path)
}
