// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One decoder node's session on a Vulkan Video decode device the machine's
//! video codec host shares among every session on that GPU: each access unit
//! decoded into an NV12 picture buffer the decoder exports, each buffer sent
//! to the node once, and the pictures of the last decode held until the
//! node's next request, so no picture is written into a buffer the node may
//! still be reading.

use std::collections::HashSet;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use streamlib_consumer_rhi::{ConsumerRhiError, OwnedVulkanVideoCodecDevice};
use streamlib_video_bitstream::annex_b_access_unit::{
    ParameterSetsFromAnnexBAccessUnit, parameter_sets_of_annex_b_access_unit,
};
use streamlib_vulkan_video::{
    DecodedPictureDelivery, SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig,
    VulkanVideoCodecDevice,
};

use crate::VideoCodecNodeIdentity;
use crate::video_codec_host_protocol::{
    DecodedPictureBufferInTheHost, MOST_PICTURE_BUFFERS_A_DECODE_SENDS, VideoAccessUnitDecoded,
    VideoAccessUnitToDecode, VideoDecodeGroupStart, VideoPictureDecodedInTheHost,
};
use crate::video_codec_retry_gate::VideoCodecRefusal;
use crate::video_decoder_settings::VideoDecoderSettings;

/// Why the shared decode device was refused: for now when the GPU lacked a
/// resource another of its clients may free, such as the BAR1 aperture.
pub(crate) fn why_the_decode_device_was_refused(
    node_type_name: &str,
    refusal: ConsumerRhiError,
) -> VideoCodecRefusal {
    let message =
        format!("{node_type_name}: the Vulkan Video decode device was refused: {refusal}");
    match refusal {
        ConsumerRhiError::GpuOutOfResourcesForNow(_) => VideoCodecRefusal::ForNow(message),
        _ => VideoCodecRefusal::ForGood(message),
    }
}

/// One access unit's decode as the session sends it: its pictures, and the
/// DMA-BUF of each picture buffer sent with them, in the pictures' order.
pub struct VideoAccessUnitDecodedToSend {
    pub decoded: VideoAccessUnitDecoded,
    pub picture_buffer_dma_bufs: Vec<Arc<OwnedFd>>,
}

/// One node's decode session on the host's shared decode device.
pub struct VideoDecodeOnADevice {
    node: VideoCodecNodeIdentity,
    device: Arc<OwnedVulkanVideoCodecDevice>,
    decoder: SimpleDecoder,
    /// The parameter sets the decoder was last fed; a group start carrying
    /// others means the producer renegotiated.
    session_parameter_sets: Option<ParameterSetsFromAnnexBAccessUnit>,
    /// The pictures the last reply named, whose buffers the decoder writes
    /// no picture into while they are held.
    pictures_held_for_the_node: Vec<SimpleDecodedFrame>,
    /// The picture buffers whose DMA-BUF this session already sent.
    picture_buffers_sent: HashSet<(u64, usize)>,
}

impl VideoDecodeOnADevice {
    /// Mint a decoder for `node`'s codec on `device`.
    pub fn open(
        node: VideoCodecNodeIdentity,
        settings: VideoDecoderSettings,
        device: Arc<OwnedVulkanVideoCodecDevice>,
    ) -> Result<Self, VideoCodecRefusal> {
        let node_type_name = node.node_type_name;
        let (max_width, max_height) = settings.decoded_picture_buffer_extent_cap(node_type_name);
        let codec_device: Arc<dyn VulkanVideoCodecDevice> = device.clone();
        let decoder = SimpleDecoder::new(
            codec_device,
            SimpleDecoderConfig {
                codec: node.vulkan_video_codec(),
                max_width,
                max_height,
                decoded_picture_delivery: DecodedPictureDelivery::Nv12InAnExportedDmaBuf,
                ..SimpleDecoderConfig::default()
            },
        )
        .map_err(|refusal| {
            VideoCodecRefusal::ForGood(format!(
                "{node_type_name}: the decoder could not be minted: {refusal}"
            ))
        })?;
        Ok(Self {
            node,
            device,
            decoder,
            session_parameter_sets: None,
            pictures_held_for_the_node: Vec::new(),
            picture_buffers_sent: HashSet::new(),
        })
    }

    /// The name of the device this session decodes on.
    pub fn device_name(&self) -> String {
        self.device.consumer_vulkan_device().name().to_string()
    }

    /// Decode `access_unit`, giving back the pictures the previous decode
    /// handed out first.
    pub fn decode(
        &mut self,
        access_unit: &VideoAccessUnitToDecode,
    ) -> Result<VideoAccessUnitDecodedToSend, String> {
        let node_type_name = self.node.node_type_name;
        self.pictures_held_for_the_node.clear();
        if access_unit.annex_b_access_unit.is_empty() {
            return Err(format!(
                "{node_type_name}: an empty message is no access unit"
            ));
        }
        if let Some(group_start) = access_unit.group_start {
            self.prepare_the_decoder_for_a_group_start(
                &access_unit.annex_b_access_unit,
                group_start,
            );
        }
        let pictures = self
            .decoder
            .feed(&access_unit.annex_b_access_unit)
            .map_err(|decode_failure| {
                format!("{node_type_name}: decode failed: {decode_failure}")
            })?;
        let decoded = self.the_pictures_to_send(&pictures)?;
        self.pictures_held_for_the_node = pictures;
        Ok(decoded)
    }

    /// New parameter sets reset the decoder in full; an entry after a loss
    /// with the same ones only flushes what the broken stream left.
    fn prepare_the_decoder_for_a_group_start(
        &mut self,
        annex_b_access_unit: &[u8],
        group_start: VideoDecodeGroupStart,
    ) {
        let arriving_parameter_sets =
            parameter_sets_of_annex_b_access_unit(annex_b_access_unit, self.node.elementary_stream);
        if let Some(session_parameter_sets) = self.session_parameter_sets.as_ref() {
            if !arriving_parameter_sets.is_empty()
                && *session_parameter_sets != arriving_parameter_sets
            {
                tracing::info!(
                    "{}: the producer's parameter sets changed — reconfiguring the decoder from \
                     this keyframe",
                    self.node.node_type_name
                );
                self.decoder.reset();
            } else if group_start == VideoDecodeGroupStart::EnteringTheStream {
                self.decoder.feed_discontinuity();
            }
        }
        if !arriving_parameter_sets.is_empty() {
            self.session_parameter_sets = Some(arriving_parameter_sets);
        }
    }

    /// The reply naming `pictures`, with the DMA-BUF of each buffer this
    /// session has not been sent yet, recorded as sent only once the reply
    /// is whole. A new generation of buffers forgets what was sent of the
    /// old.
    fn the_pictures_to_send(
        &mut self,
        pictures: &[SimpleDecodedFrame],
    ) -> Result<VideoAccessUnitDecodedToSend, String> {
        let node_type_name = self.node.node_type_name;
        let mut decoded = VideoAccessUnitDecoded::default();
        let mut picture_buffer_dma_bufs = Vec::new();
        let mut picture_buffers_newly_sent = Vec::new();
        for picture in pictures {
            let exported = picture.exported_nv12.as_ref().ok_or_else(|| {
                format!("{node_type_name}: the decoder handed back a picture in no exported buffer")
            })?;
            let picture_buffer = DecodedPictureBufferInTheHost {
                generation: exported.ring_generation,
                slot_index: exported.ring_slot_index,
                allocation_byte_size: exported.allocation_byte_size,
            };
            let picture_buffer_key = (picture_buffer.generation, picture_buffer.slot_index);
            let picture_buffer_is_sent_with_it =
                !self.picture_buffers_sent.contains(&picture_buffer_key)
                    && !picture_buffers_newly_sent.contains(&picture_buffer_key);
            if picture_buffer_is_sent_with_it {
                picture_buffers_newly_sent.push(picture_buffer_key);
                picture_buffer_dma_bufs.push(Arc::clone(&exported.dma_buf_fd));
            }
            decoded.pictures.push(VideoPictureDecodedInTheHost {
                picture_buffer,
                picture_buffer_is_sent_with_it,
                width: picture.width,
                height: picture.height,
                plane_offsets_bytes: exported.plane_offsets_bytes,
                plane_strides_bytes: exported.plane_strides_bytes,
                color_vui: picture.decoded_picture_color_vui,
            });
        }
        if picture_buffer_dma_bufs.len() > MOST_PICTURE_BUFFERS_A_DECODE_SENDS {
            return Err(format!(
                "{node_type_name}: one decode handed out {} new picture buffers, more than the \
                 {MOST_PICTURE_BUFFERS_A_DECODE_SENDS} a reply carries",
                picture_buffer_dma_bufs.len()
            ));
        }
        if let Some(&(latest_generation, _)) = picture_buffers_newly_sent.last() {
            self.picture_buffers_sent
                .retain(|&(generation, _)| generation == latest_generation);
        }
        self.picture_buffers_sent.extend(picture_buffers_newly_sent);
        Ok(VideoAccessUnitDecodedToSend {
            decoded,
            picture_buffer_dma_bufs,
        })
    }
}
