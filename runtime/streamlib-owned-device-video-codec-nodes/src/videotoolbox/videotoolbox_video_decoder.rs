// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! H.264 or H.265 decode on Apple's media engine: each picture lands on an
//! IOSurface VideoToolbox decodes into, the host adopts it, converts it into
//! a texture of the node's output pool, and that texture is what the
//! published `VideoFrame` names.

use std::collections::HashMap;
use std::time::Instant;

use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVPixelBufferGetIOSurface, CVPixelBufferGetPixelFormatType};
use objc2_io_surface::{IOSurfaceID, IOSurfaceRef};
use tatolab_module_sdk::{
    ModuleHost, ModuleHostColorInfo, ModuleHostForeignIOSurface,
    ModuleHostNodeOutputTextureRequest, ModuleHostOutputPort, ModuleHostReadMessage,
    ModuleHostSurface,
};

use super::core_video_picture_colour::h273_color_vui_of_core_video_pixel_buffer;
use super::nv12_pixel_format_wire_name_of_core_video_format;
use super::videotoolbox_decompression_session::{
    DecodedVideoToolboxPicture, VideoToolboxDecompressionSession,
};
use crate::VideoCodecNodeIdentity;
use crate::encoded_video_group_entry_gate::{
    EncodedVideoGroupEntryDisposition, EncodedVideoGroupEntryGate,
};
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

/// IOSurfaces kept adopted; VideoToolbox's own pool is far smaller.
const MOST_ADOPTED_IOSURFACES_KEPT: usize = 32;

const DECODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// An IOSurface VideoToolbox decoded into and the surface its adoption made,
/// reused while the picture's extent and format on it hold.
struct DecodedIOSurfaceAdoptedAsSurface {
    width: u32,
    height: u32,
    pixel_format_wire_name: &'static str,
    adopted_surface: ModuleHostSurface,
    /// Held so the id the adoption is keyed by names this surface while the
    /// adoption does.
    _iosurface: CFRetained<IOSurfaceRef>,
}

// SAFETY: the retained IOSurface is only kept alive here, never messaged, and
// IOSurface references may be released from any thread.
unsafe impl Send for DecodedIOSurfaceAdoptedAsSurface {}

/// A video decoder node's native half on macOS: the VideoToolbox session and
/// the adoptions of the IOSurfaces it decodes into.
pub struct VideoToolboxVideoDecoder {
    node: VideoCodecNodeIdentity,
    module_host: ModuleHost,
    decompression_session: VideoToolboxDecompressionSession,
    group_entry_gate: EncodedVideoGroupEntryGate,
    adopted_by_iosurface_id: HashMap<IOSurfaceID, DecodedIOSurfaceAdoptedAsSurface>,
    frames_decoded: u64,
}

impl VideoToolboxVideoDecoder {
    /// A decoder on the machine's hardware decoder, refused by name where
    /// there is none for the node's elementary stream.
    pub fn open(
        node: VideoCodecNodeIdentity,
        module_host: ModuleHost,
        settings: VideoDecoderSettings,
    ) -> Result<Self, String> {
        let node_type_name = node.node_type_name;
        let opening_started = Instant::now();
        let maximum_picture_extent =
            match settings.decoded_picture_buffer_extent_cap(node_type_name) {
                (0, 0) => None,
                cap => Some(cap),
            };
        let decompression_session =
            VideoToolboxDecompressionSession::open(node.elementary_stream, maximum_picture_extent)
                .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        tracing::info!(
            opened_ms = opening_started.elapsed().as_secs_f64() * 1_000.0,
            "{node_type_name}: decoding on VideoToolbox; entering the stream at its next group \
             start"
        );
        Ok(Self {
            node,
            module_host,
            decompression_session,
            group_entry_gate: EncodedVideoGroupEntryGate::default(),
            adopted_by_iosurface_id: HashMap::new(),
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
        if disposition == EncodedVideoGroupEntryDisposition::EnterAtThisGroupStart {
            self.decompression_session
                .discard_in_flight_state_after_a_gap();
        }
        let decoded = self.decode_and_publish_an_admitted_media_frame(media_frame, video_output);
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
        video_output: &ModuleHostOutputPort,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        let annex_b_access_unit = media_frame.payload();
        if annex_b_access_unit.is_empty() {
            return Err(format!(
                "{node_type_name}: an empty message is no access unit"
            ));
        }
        let pictures = self
            .decompression_session
            .decode_annex_b_access_unit(annex_b_access_unit)
            .map_err(|decode_failure| {
                format!("{node_type_name}: decode failed: {decode_failure}")
            })?;
        for picture in pictures {
            self.publish_one_picture(&picture, media_frame.timestamp_ns(), video_output)?;
        }
        Ok(())
    }

    fn publish_one_picture(
        &mut self,
        picture: &DecodedVideoToolboxPicture,
        timestamp_ns: i64,
        video_output: &ModuleHostOutputPort,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        let core_video_pixel_format = CVPixelBufferGetPixelFormatType(&picture.pixel_buffer);
        let pixel_format_wire_name = nv12_pixel_format_wire_name_of_core_video_format(
            core_video_pixel_format,
        )
        .ok_or_else(|| {
            format!(
                "{node_type_name}: VideoToolbox decoded into CoreVideo format \
                         {core_video_pixel_format:#010x}; only 8-bit biplanar 4:2:0 is converted"
            )
        })?;
        let iosurface =
            CVPixelBufferGetIOSurface(Some(&picture.pixel_buffer)).ok_or_else(|| {
                format!("{node_type_name}: VideoToolbox decoded a picture with no IOSurface")
            })?;
        let decoded_picture_color_vui =
            h273_color_vui_of_core_video_pixel_buffer(&picture.pixel_buffer);
        let published_color_info =
            H273ColorVuiWireNames::of_h273_color_vui(&decoded_picture_color_vui);
        let conversion_color_info = H273ColorVuiWireNames::of_h273_color_vui(
            &decoded_picture_color_vui
                .with_decoded_picture_matrix_and_range_defaults(picture.width, picture.height),
        );
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
        let adopted_iosurface_id = self.adopt_unless_already_adopted(
            iosurface,
            picture.width,
            picture.height,
            pixel_format_wire_name,
        )?;
        let adopted_surface = &self
            .adopted_by_iosurface_id
            .get(&adopted_iosurface_id)
            .ok_or_else(|| {
                format!("IOSurface {adopted_iosurface_id} is missing from the adopted set")
            })?
            .adopted_surface;
        self.module_host
            .convert_surface_into(
                adopted_surface,
                &decoded_picture_texture,
                &ModuleHostColorInfo {
                    primaries: conversion_color_info.primaries,
                    transfer: conversion_color_info.transfer,
                    matrix: conversion_color_info.matrix,
                    range: conversion_color_info.range,
                },
            )
            .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        let image_frame = image_frame_payload_descriptor_of_a_decoded_picture(
            &decoded_picture_texture,
            DECODED_PICTURE_TEXTURE_FORMAT,
            picture.width,
            picture.height,
            &decoded_picture_color_vui,
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
                ?published_color_info,
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

    /// Adopt `iosurface` for a picture of this extent and format unless it
    /// already is, answering the id its adoption is kept under. An adoption
    /// past the bound starts the set over.
    fn adopt_unless_already_adopted(
        &mut self,
        iosurface: CFRetained<IOSurfaceRef>,
        width: u32,
        height: u32,
        pixel_format_wire_name: &'static str,
    ) -> Result<IOSurfaceID, String> {
        let iosurface_id = iosurface.id();
        let still_adopted_as_this_picture = self
            .adopted_by_iosurface_id
            .get(&iosurface_id)
            .is_some_and(|adopted| {
                (
                    adopted.width,
                    adopted.height,
                    adopted.pixel_format_wire_name,
                ) == (width, height, pixel_format_wire_name)
            });
        if !still_adopted_as_this_picture {
            self.adopted_by_iosurface_id.remove(&iosurface_id);
            let adopted_surface = self
                .module_host
                .import_iosurface(&ModuleHostForeignIOSurface {
                    iosurface: CFRetained::as_ptr(&iosurface).as_ptr().cast_const().cast(),
                    pixel_format: pixel_format_wire_name,
                    width,
                    height,
                })
                .map_err(|refusal| {
                    format!(
                        "{}: the host did not adopt a decoded picture's IOSurface: {refusal}",
                        self.node.node_type_name
                    )
                })?;
            if self.adopted_by_iosurface_id.len() >= MOST_ADOPTED_IOSURFACES_KEPT {
                self.adopted_by_iosurface_id.clear();
            }
            self.adopted_by_iosurface_id.insert(
                iosurface_id,
                DecodedIOSurfaceAdoptedAsSurface {
                    width,
                    height,
                    pixel_format_wire_name,
                    adopted_surface,
                    _iosurface: iosurface,
                },
            );
        }
        Ok(iosurface_id)
    }
}
