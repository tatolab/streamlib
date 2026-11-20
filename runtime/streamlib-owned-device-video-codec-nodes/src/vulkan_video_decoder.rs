// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! H.264 or H.265 decode with Vulkan Video: each access unit is decoded in
//! the machine's video codec host, on the one decode device it shares among
//! every decoder on the GPU. Each picture lands in one of the host's NV12
//! picture buffers, whose DMA-BUF the host sends once; the runtime adopts
//! the buffer, converts the picture into a texture of the node's output pool,
//! and that texture is what the published `VideoFrame` names.

use std::collections::HashMap;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::time::Instant;

use streamlib_consumer_rhi::runtime_vulkan_device_uuid_named_at_spawn;
use tatolab_module_sdk::{
    ModuleHost, ModuleHostColorInfo, ModuleHostForeignDmaBuf, ModuleHostNodeOutputTextureRequest,
    ModuleHostOutputPort, ModuleHostReadMessage, ModuleHostSurface,
};

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_group_entry_gate::{
    EncodedVideoGroupEntryDisposition, EncodedVideoGroupEntryGate,
};
use crate::image_frame_on_the_video_port::{
    H273ColorVuiWireNames, image_frame_payload_descriptor_of_a_decoded_picture,
};
use crate::video_codec_host_protocol::{
    DecodedPictureBufferInTheHost, MOST_PICTURE_BUFFERS_A_DECODE_SENDS,
    VIDEO_CODEC_HOST_PROTOCOL_VERSION, VideoAccessUnitToDecode, VideoCodecHostRequest,
    VideoDecodeGroupStart, VideoDecodeSessionRequest, VideoPictureDecodedInTheHost,
};
use crate::video_codec_host_session_of_the_node::{
    VideoCodecHostSessionConnection, VideoCodecHostSessionOfTheNode, VideoCodecHostSessionOpening,
};
use crate::video_codec_retry_gate::VideoCodecRefusal;
use crate::video_decoder_settings::VideoDecoderSettings;

/// The node output pool decoded pictures are converted into.
const DECODED_PICTURE_POOL_KEY: &str = "decoded_pictures";
const DECODED_PICTURE_TEXTURE_FORMAT: &str = "rgba8_unorm";
const DECODED_PICTURE_TEXTURE_USAGE: [&str; 2] = ["storage_binding", "texture_binding"];
/// Textures the pool rotates before reusing one a reader released.
const DECODED_PICTURE_POOL_ROTATION_DEPTH: u32 = 4;

/// `DRM_FORMAT_MOD_LINEAR`: the host's picture buffers are linear.
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

const DECODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// The layout a picture occupies in its buffer; the runtime adopts a buffer
/// once per layout it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodedPictureLayout {
    width: u32,
    height: u32,
    plane_offsets_bytes: [u64; 2],
    plane_strides_bytes: [u64; 2],
    pixel_format_wire_name: &'static str,
}

/// One of the host's picture buffers: its DMA-BUF, and the runtime's
/// adoption of it for the layout of the last picture in it.
struct PictureBufferOfTheHost {
    dma_buf: OwnedFd,
    adopted: Option<(DecodedPictureLayout, ModuleHostSurface)>,
}

/// A video decoder node's native half: its session in the video codec host
/// and the host's picture buffers the runtime adopted.
pub struct VulkanVideoDecoder {
    node: VideoCodecNodeIdentity,
    module_host: ModuleHost,
    session: VideoCodecHostSessionOfTheNode,
    group_entry_gate: EncodedVideoGroupEntryGate,
    picture_buffers: HashMap<(u64, usize), PictureBufferOfTheHost>,
    frames_decoded: u64,
}

impl VulkanVideoDecoder {
    /// Open a decode session in the machine's video codec host on the GPU
    /// the runtime named: refused for now while the host cannot be reached
    /// or the GPU lacks a resource another of its clients may free.
    pub fn open(
        node: VideoCodecNodeIdentity,
        module_host: ModuleHost,
        settings: VideoDecoderSettings,
    ) -> Result<Self, VideoCodecRefusal> {
        let node_type_name = node.node_type_name;
        let opening_started = Instant::now();
        let vulkan_device_uuid =
            runtime_vulkan_device_uuid_named_at_spawn().map_err(|refusal| {
                VideoCodecRefusal::ForGood(format!("{node_type_name}: {refusal}"))
            })?;
        let session = VideoCodecHostSessionOfTheNode::open(
            node,
            &module_host,
            VideoCodecHostSessionOpening::Decode(VideoDecodeSessionRequest {
                protocol_version: VIDEO_CODEC_HOST_PROTOCOL_VERSION,
                elementary_stream: node.elementary_stream.into(),
                settings,
                vulkan_device_uuid,
            }),
        )?;
        tracing::info!(
            opened_ms = opening_started.elapsed().as_secs_f64() * 1_000.0,
            "{node_type_name}: entering the stream at its next group start"
        );
        Ok(Self {
            node,
            module_host,
            session,
            group_entry_gate: EncodedVideoGroupEntryGate::default(),
            picture_buffers: HashMap::new(),
            frames_decoded: 0,
        })
    }

    /// Decode one media frame and publish the pictures it completes on
    /// `video`, stamped with its timestamp. A failure gives up the rest of
    /// its group, and a session opened again enters the stream at the next
    /// group start with none of the old host's buffers.
    pub fn decode_and_publish(
        &mut self,
        media_frame: &ModuleHostReadMessage,
        video_output: &ModuleHostOutputPort,
    ) -> Result<(), String> {
        if self.session.connected()? == VideoCodecHostSessionConnection::OpenedAgain {
            self.picture_buffers.clear();
            self.group_entry_gate
                .discard_the_rest_of_the_current_group();
        }
        let disposition = self.group_entry_gate.admit_media_frame(
            media_frame.starts_group(),
            media_frame.lost_since_last_read(),
        );
        if disposition == EncodedVideoGroupEntryDisposition::DiscardUntilTheNextGroupStart {
            return Ok(());
        }
        let decoded =
            self.decode_and_publish_an_admitted_media_frame(media_frame, disposition, video_output);
        if decoded.is_err() {
            self.group_entry_gate
                .discard_the_rest_of_the_current_group();
        }
        decoded
    }

    /// Frames lost on the link and discarded waiting for a group start, for
    /// the node's teardown report.
    pub fn report_at_teardown(&self) {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            frames_lost_on_the_link = self.group_entry_gate.media_frames_lost_on_the_link(),
            frames_discarded_awaiting_a_group_start = self
                .group_entry_gate
                .media_frames_discarded_awaiting_a_group_start(),
            stream_entries = self.group_entry_gate.group_starts_entered_at(),
            "{}: stopped",
            self.node.node_type_name
        );
    }

    fn decode_and_publish_an_admitted_media_frame(
        &mut self,
        media_frame: &ModuleHostReadMessage,
        disposition: EncodedVideoGroupEntryDisposition,
        video_output: &ModuleHostOutputPort,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        let group_start = media_frame.starts_group().then_some(match disposition {
            EncodedVideoGroupEntryDisposition::EnterAtThisGroupStart => {
                VideoDecodeGroupStart::EnteringTheStream
            }
            _ => VideoDecodeGroupStart::ContinuingTheStream,
        });
        let request = VideoCodecHostRequest::DecodeAccessUnit(VideoAccessUnitToDecode {
            annex_b_access_unit: media_frame.payload().to_vec(),
            group_start,
        });
        let (reply, picture_buffer_dma_bufs) = self
            .session
            .ask(&request, MOST_PICTURE_BUFFERS_A_DECODE_SENDS)?;
        let decoded = reply.into_the_decode()?;
        if picture_buffer_dma_bufs.len() != decoded.picture_buffers_sent() {
            return Err(format!(
                "{node_type_name}: the video codec host named {} new picture buffers and sent {}",
                decoded.picture_buffers_sent(),
                picture_buffer_dma_bufs.len()
            ));
        }
        let mut picture_buffer_dma_bufs = picture_buffer_dma_bufs.into_iter();
        for picture in &decoded.pictures {
            if picture.picture_buffer_is_sent_with_it {
                let dma_buf = picture_buffer_dma_bufs
                    .next()
                    .expect("one DMA-BUF arrived for each picture buffer sent");
                self.keep_a_picture_buffer_sent(picture.picture_buffer, dma_buf);
            }
            self.publish_one_picture(picture, media_frame.timestamp_ns(), video_output)?;
        }
        Ok(())
    }

    /// Keep the DMA-BUF of a buffer the host sent; a buffer of a newer
    /// generation retires every buffer of the older ones.
    fn keep_a_picture_buffer_sent(
        &mut self,
        picture_buffer: DecodedPictureBufferInTheHost,
        dma_buf: OwnedFd,
    ) {
        self.picture_buffers
            .retain(|&(generation, _), _| generation >= picture_buffer.generation);
        self.picture_buffers.insert(
            (picture_buffer.generation, picture_buffer.slot_index),
            PictureBufferOfTheHost {
                dma_buf,
                adopted: None,
            },
        );
    }

    fn publish_one_picture(
        &mut self,
        picture: &VideoPictureDecodedInTheHost,
        timestamp_ns: i64,
        video_output: &ModuleHostOutputPort,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        let color_info = H273ColorVuiWireNames::of_h273_color_vui(&picture.color_vui);
        let layout = DecodedPictureLayout {
            width: picture.width,
            height: picture.height,
            plane_offsets_bytes: picture.plane_offsets_bytes,
            plane_strides_bytes: picture.plane_strides_bytes,
            pixel_format_wire_name: match color_info.range {
                Some("full") => "nv12_full_range",
                _ => "nv12_video_range",
            },
        };
        let adopted_surface = adopted_for_its_layout(
            node_type_name,
            &self.module_host,
            &mut self.picture_buffers,
            picture.picture_buffer,
            layout,
        )?;
        let decoded_picture_texture = self
            .module_host
            .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
                pool_key: DECODED_PICTURE_POOL_KEY,
                rotation_depth: DECODED_PICTURE_POOL_ROTATION_DEPTH,
                width: picture.width,
                height: picture.height,
                texture_format: DECODED_PICTURE_TEXTURE_FORMAT,
                usage: &DECODED_PICTURE_TEXTURE_USAGE,
            })
            .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        self.module_host
            .convert_surface_into(
                adopted_surface,
                &decoded_picture_texture,
                &ModuleHostColorInfo {
                    primaries: color_info.primaries,
                    transfer: color_info.transfer,
                    matrix: color_info.matrix,
                    range: color_info.range,
                },
            )
            .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        let image_frame = image_frame_payload_descriptor_of_a_decoded_picture(
            &decoded_picture_texture,
            DECODED_PICTURE_TEXTURE_FORMAT,
            picture.width,
            picture.height,
            &picture.color_vui,
        )
        .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        video_output
            .write_described(&[], Some(timestamp_ns), true, &image_frame)
            .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;

        self.frames_decoded += 1;
        if self.frames_decoded == 1 {
            tracing::info!(
                width = picture.width,
                height = picture.height,
                ?color_info,
                "{node_type_name}: first frame decoded"
            );
        } else if self
            .frames_decoded
            .is_multiple_of(DECODE_PROGRESS_LOG_INTERVAL_FRAMES)
        {
            tracing::info!(
                frames_decoded = self.frames_decoded,
                "{node_type_name}: decode progress"
            );
        }
        Ok(())
    }
}

/// The runtime's adoption of `picture_buffer` for `layout`, adopted
/// first when the buffer is new or the last picture in it was laid out
/// otherwise.
fn adopted_for_its_layout<'kept>(
    node_type_name: &str,
    module_host: &ModuleHost,
    picture_buffers: &'kept mut HashMap<(u64, usize), PictureBufferOfTheHost>,
    picture_buffer: DecodedPictureBufferInTheHost,
    layout: DecodedPictureLayout,
) -> Result<&'kept ModuleHostSurface, String> {
    let kept = picture_buffers
        .get_mut(&(picture_buffer.generation, picture_buffer.slot_index))
        .ok_or_else(|| {
            format!(
                "{node_type_name}: the video codec host named picture buffer {} of \
                 generation {} without ever sending it",
                picture_buffer.slot_index, picture_buffer.generation
            )
        })?;
    let adopted_for_this_layout = kept
        .adopted
        .as_ref()
        .is_some_and(|(adopted_layout, _)| *adopted_layout == layout);
    if !adopted_for_this_layout {
        let adopted_surface = module_host
            .import_dma_buf(&ModuleHostForeignDmaBuf {
                fd: kept.dma_buf.as_raw_fd(),
                width: layout.width,
                height: layout.height,
                pixel_format: layout.pixel_format_wire_name,
                allocation_byte_size: picture_buffer.allocation_byte_size,
                plane_offsets_bytes: &layout.plane_offsets_bytes,
                plane_strides_bytes: &layout.plane_strides_bytes,
                drm_format_modifier: DRM_FORMAT_MOD_LINEAR,
            })
            .map_err(|refusal| {
                format!(
                    "{node_type_name}: the runtime did not adopt a decoded picture's \
                     DMA-BUF: {refusal}"
                )
            })?;
        kept.adopted = Some((layout, adopted_surface));
    }
    Ok(&kept
        .adopted
        .as_ref()
        .expect("the buffer was adopted for this layout")
        .1)
}
