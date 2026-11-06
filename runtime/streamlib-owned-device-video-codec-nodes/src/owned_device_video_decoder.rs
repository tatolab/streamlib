// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! H.264 or H.265 decode on a Vulkan device this module owns: each picture
//! lands in an NV12 DMA-BUF the decoder exports, the host adopts it, converts
//! it into a texture of the node's output pool, and that texture is what the
//! published `VideoFrame` names.

use std::os::fd::AsRawFd as _;
use std::sync::Arc;
use std::time::Instant;

use streamlib_consumer_rhi::{
    OwnedVulkanVideoCodecDevice, VulkanVideoCodecDirection,
    runtime_vulkan_device_uuid_named_at_spawn,
};
use streamlib_video_bitstream::annex_b_access_unit::{
    ParameterSetsFromAnnexBAccessUnit, parameter_sets_of_annex_b_access_unit,
};
use streamlib_vulkan_video::{
    DecodedPictureDelivery, ExportedNv12DmaBufPicture, SimpleDecodedFrame, SimpleDecoder,
    SimpleDecoderConfig, VulkanVideoCodecDevice,
};
use tatolab_module_sdk::{
    ModuleHost, ModuleHostColorInfo, ModuleHostForeignDmaBuf, ModuleHostNodeOutputTextureRequest,
    ModuleHostOutputPort, ModuleHostReadMessage, ModuleHostSurface,
};

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_group_entry_gate::{
    EncodedVideoGroupEntryDisposition, EncodedVideoGroupEntryGate,
};
use crate::hang_legacy_container_media_frame::annex_b_access_unit_of_a_hang_legacy_container_media_frame;
use crate::image_frame_on_the_video_port::{
    H273ColorVuiWireNames, image_frame_payload_descriptor_of_a_decoded_picture,
};
use crate::video_decoder_settings::VideoDecoderSettings;

/// The node output pool decoded pictures are converted into.
const DECODED_PICTURE_POOL_KEY: &str = "decoded_pictures";
const DECODED_PICTURE_TEXTURE_FORMAT: &str = "rgba8_unorm";
const DECODED_PICTURE_TEXTURE_USAGE: [&str; 2] = ["storage_binding", "texture_binding"];
/// Textures the pool rotates before reusing one a reader released.
const DECODED_PICTURE_POOL_ROTATION_DEPTH: u32 = 4;

/// `DRM_FORMAT_MOD_LINEAR`: the exported pictures are linear buffers.
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

const DECODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// One exported picture buffer the host adopted, reused while the decoder's
/// ring and the picture's layout in it hold.
struct AdoptedExportedNv12Picture {
    ring_generation: u64,
    ring_slot_index: usize,
    width: u32,
    height: u32,
    plane_offsets_bytes: [u64; 2],
    pixel_format_wire_name: &'static str,
    adopted_surface: ModuleHostSurface,
}

/// A video decoder node's native half: the owned device's decoder and the
/// adoptions of the buffers it hands pictures out in.
pub struct OwnedDeviceVideoDecoder {
    node: VideoCodecNodeIdentity,
    module_host: ModuleHost,
    decoder: SimpleDecoder,
    group_entry_gate: EncodedVideoGroupEntryGate,
    /// The parameter sets the decoder was last fed; a group start carrying
    /// others means the producer renegotiated.
    session_parameter_sets: Option<ParameterSetsFromAnnexBAccessUnit>,
    adopted_pictures: Vec<AdoptedExportedNv12Picture>,
    frames_decoded: u64,
}

impl OwnedDeviceVideoDecoder {
    /// Create the owned device on the GPU the runtime named and a decoder on
    /// it, refused by name on a device with no Vulkan Video decode.
    pub fn open(
        node: VideoCodecNodeIdentity,
        module_host: ModuleHost,
        settings: VideoDecoderSettings,
    ) -> Result<Self, String> {
        let node_type_name = node.node_type_name;
        let opening_started = Instant::now();
        let runtime_vulkan_device_uuid = runtime_vulkan_device_uuid_named_at_spawn()
            .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        let owned_device = Arc::new(
            OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
                runtime_vulkan_device_uuid,
                VulkanVideoCodecDirection::Decode,
            )
            .map_err(|refusal| {
                format!(
                    "{node_type_name}: the owned Vulkan Video decode device was refused: {refusal}"
                )
            })?,
        );
        let device_name = owned_device.consumer_vulkan_device().name().to_owned();
        let (max_width, max_height) = settings.decoded_picture_buffer_extent_cap(node_type_name);
        let codec_device: Arc<dyn VulkanVideoCodecDevice> = owned_device;
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
            format!("{node_type_name}: the decoder could not be minted: {refusal}")
        })?;
        tracing::info!(
            device = device_name,
            opened_ms = opening_started.elapsed().as_secs_f64() * 1_000.0,
            "{node_type_name}: decoding on an owned Vulkan Video device; entering the stream at \
             its next group start"
        );
        Ok(Self {
            node,
            module_host,
            decoder,
            group_entry_gate: EncodedVideoGroupEntryGate::default(),
            session_parameter_sets: None,
            adopted_pictures: Vec::new(),
            frames_decoded: 0,
        })
    }

    /// Decode one media frame and publish the pictures it completes on
    /// `video`, stamped with its timestamp. A failure gives up the rest of
    /// its group.
    pub fn decode_and_publish(
        &mut self,
        media_frame: &ModuleHostReadMessage,
        video_output: &ModuleHostOutputPort,
    ) -> Result<(), String> {
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
        let annex_b_access_unit =
            annex_b_access_unit_of_a_hang_legacy_container_media_frame(media_frame.payload())
                .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        if media_frame.starts_group() {
            self.prepare_the_decoder_for_a_group_start(annex_b_access_unit, disposition);
        }
        let pictures = self
            .decoder
            .feed(annex_b_access_unit)
            .map_err(|decode_failure| {
                format!("{node_type_name}: decode failed: {decode_failure}")
            })?;
        for picture in pictures {
            self.publish_one_picture(&picture, media_frame.timestamp_ns(), video_output)?;
        }
        Ok(())
    }

    /// New parameter sets reset the decoder in full; a re-entry after a loss
    /// with the same ones only flushes what the broken stream left.
    fn prepare_the_decoder_for_a_group_start(
        &mut self,
        annex_b_access_unit: &[u8],
        disposition: EncodedVideoGroupEntryDisposition,
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
            } else if disposition == EncodedVideoGroupEntryDisposition::EnterAtThisGroupStart {
                self.decoder.feed_discontinuity();
            }
        }
        if !arriving_parameter_sets.is_empty() {
            self.session_parameter_sets = Some(arriving_parameter_sets);
        }
    }

    fn publish_one_picture(
        &mut self,
        picture: &SimpleDecodedFrame,
        timestamp_ns: i64,
        video_output: &ModuleHostOutputPort,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        let exported = picture.exported_nv12.as_ref().ok_or_else(|| {
            format!("{node_type_name}: the decoder handed back a picture in no exported buffer")
        })?;
        let color_info =
            H273ColorVuiWireNames::of_h273_color_vui(&picture.decoded_picture_color_vui);
        let pixel_format_wire_name = match color_info.range {
            Some("full") => "nv12_full_range",
            _ => "nv12_video_range",
        };
        let adopted_index = self.adopt_unless_already_adopted(
            exported,
            picture.width,
            picture.height,
            pixel_format_wire_name,
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
                &self.adopted_pictures[adopted_index].adopted_surface,
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
            &picture.decoded_picture_color_vui,
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

    /// The index of the adoption `exported` resolves to, adopting it first
    /// when the ring was reallocated or the picture's layout changed.
    fn adopt_unless_already_adopted(
        &mut self,
        exported: &ExportedNv12DmaBufPicture,
        width: u32,
        height: u32,
        pixel_format_wire_name: &'static str,
    ) -> Result<usize, String> {
        let matches = |adopted: &AdoptedExportedNv12Picture| {
            adopted.ring_generation == exported.ring_generation
                && adopted.ring_slot_index == exported.ring_slot_index
                && (adopted.width, adopted.height) == (width, height)
                && adopted.plane_offsets_bytes == exported.plane_offsets_bytes
                && adopted.pixel_format_wire_name == pixel_format_wire_name
        };
        if let Some(index) = self.adopted_pictures.iter().position(matches) {
            return Ok(index);
        }
        self.adopted_pictures.retain(|adopted| {
            adopted.ring_generation == exported.ring_generation
                && adopted.ring_slot_index != exported.ring_slot_index
        });
        let adopted_surface = self
            .module_host
            .import_dma_buf(&ModuleHostForeignDmaBuf {
                fd: exported.dma_buf_fd.as_raw_fd(),
                width,
                height,
                pixel_format: pixel_format_wire_name,
                allocation_byte_size: exported.allocation_byte_size,
                plane_offsets_bytes: &exported.plane_offsets_bytes,
                plane_strides_bytes: &exported.plane_strides_bytes,
                drm_format_modifier: DRM_FORMAT_MOD_LINEAR,
            })
            .map_err(|refusal| {
                format!(
                    "{}: the host did not adopt a decoded picture's DMA-BUF: {refusal}",
                    self.node.node_type_name
                )
            })?;
        self.adopted_pictures.push(AdoptedExportedNv12Picture {
            ring_generation: exported.ring_generation,
            ring_slot_index: exported.ring_slot_index,
            width,
            height,
            plane_offsets_bytes: exported.plane_offsets_bytes,
            pixel_format_wire_name,
            adopted_surface,
        });
        Ok(self.adopted_pictures.len() - 1)
    }
}
