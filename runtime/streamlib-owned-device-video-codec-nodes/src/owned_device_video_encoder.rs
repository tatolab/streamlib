// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! H.264 or H.265 encode on a Vulkan device this module owns: each published frame,
//! a pixel buffer or a texture, is checked out over surface-share, imported
//! onto the owned device and encoded there, so a fault in the video engine
//! ends this helper and nothing else.

use std::collections::HashMap;
use std::os::fd::{FromRawFd as _, IntoRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, ConsumerVulkanTexture, OwnedVulkanVideoCodecDevice, PixelFormat,
    TextureFormat, VulkanLayout, VulkanVideoCodecDirection,
    runtime_vulkan_device_uuid_named_at_spawn,
};
use streamlib_surface_client::{
    CheckedOutPixelBuffer, CheckedOutPixelBufferMemory, CheckedOutSurface, CheckedOutTexture,
    CheckedOutTextureMemory, check_out_surface_on_surface_share, connect_to_surface_share_socket,
    pool_slot_key_of_surface_id, publish_image_layout_on_surface_share,
    release_check_out_on_surface_share,
};
use streamlib_vulkan_video::{
    Preset, RgbaPixelBufferVideoEncoder, SimpleEncoder, SimpleEncoderConfig, VulkanVideoCodecDevice,
};

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_publication::{
    EncodedVideoAccessUnit, EncodedVideoOutputPort, EncodedVideoPublication,
};
use crate::image_frame_on_the_video_port::ImageFrameReadByTheEncoder;
use crate::video_encoder_settings::{VideoEncodeSessionShape, VideoEncoderSettings};

/// The variable a processor interpreter is handed the surface-share socket in.
const SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_SURFACE_SOCKET";

/// Imports kept, one per producer pool slot; a producer's pool is far smaller.
const MOST_SURFACE_IMPORTS_KEPT: usize = 32;

const ENCODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// The encoder session and the extent it was minted for.
struct VideoEncodeSessionMintedFromAFrame {
    encoder: RgbaPixelBufferVideoEncoder,
    minted_width: u32,
    minted_height: u32,
}

/// A video encoder node's native half: the owned device, its surface-share
/// connection, and the session minted from the frames it receives.
pub struct OwnedDeviceVideoEncoder {
    node: VideoCodecNodeIdentity,
    settings: VideoEncoderSettings,
    owned_device: Arc<OwnedVulkanVideoCodecDevice>,
    surface_share_connection: UnixStream,
    imported_pixel_buffers: HashMap<String, ConsumerVulkanBuffer>,
    imported_textures: HashMap<String, ConsumerVulkanTexture>,
    minted_session: Option<VideoEncodeSessionMintedFromAFrame>,
    session_mint_already_failed: bool,
    next_frame_must_be_a_sync_point: bool,
    publication: EncodedVideoPublication,
    frames_encoded: u64,
    frames_not_checked_out: u64,
    publication_refusals: u64,
}

impl OwnedDeviceVideoEncoder {
    /// Create the owned device on the GPU the runtime named and connect to
    /// its surface-share service.
    pub fn open(
        node: VideoCodecNodeIdentity,
        settings: VideoEncoderSettings,
    ) -> Result<Self, String> {
        let node_type_name = node.node_type_name;
        let opening_started = Instant::now();
        let runtime_vulkan_device_uuid = runtime_vulkan_device_uuid_named_at_spawn()
            .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        let owned_device = Arc::new(
            OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
                runtime_vulkan_device_uuid,
                VulkanVideoCodecDirection::Encode,
            )
            .map_err(|refusal| {
                format!(
                    "{node_type_name}: the owned Vulkan Video encode device was refused: {refusal}"
                )
            })?,
        );
        let owned_device_created_ms = opening_started.elapsed().as_secs_f64() * 1_000.0;
        let surface_share_socket_path = std::env::var_os(SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE)
            .ok_or_else(|| {
                format!(
                    "{node_type_name}: {SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE} is not set, so no \
                     published frame can be checked out"
                )
            })?;
        let surface_share_connection = connect_to_surface_share_socket(Path::new(
            &surface_share_socket_path,
        ))
        .map_err(|connect_failure| {
            format!(
                "{node_type_name}: the surface-share socket refused a connection: {connect_failure}"
            )
        })?;
        tracing::info!(
            owned_device_created_ms,
            device = owned_device.consumer_vulkan_device().name(),
            "{node_type_name}: encoding on an owned Vulkan Video device"
        );
        Ok(Self {
            node,
            settings,
            owned_device,
            surface_share_connection,
            imported_pixel_buffers: HashMap::new(),
            imported_textures: HashMap::new(),
            minted_session: None,
            session_mint_already_failed: false,
            next_frame_must_be_a_sync_point: false,
            publication: EncodedVideoPublication::new(node.elementary_stream),
            frames_encoded: 0,
            frames_not_checked_out: 0,
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
        if self.minted_session.is_none() && self.session_mint_already_failed {
            return Ok(());
        }
        let checked_out = match check_out_surface_on_surface_share(
            &self.surface_share_connection,
            frame.surface_id(),
        ) {
            Ok(checked_out) => checked_out,
            Err(refusal) => {
                self.frames_not_checked_out += 1;
                if self.frames_not_checked_out.is_power_of_two() {
                    tracing::warn!(
                        frames_not_checked_out = self.frames_not_checked_out,
                        "{node_type_name}: a frame could not be checked out and was skipped: {refusal}"
                    );
                }
                return Ok(());
            }
        };
        let encoded = match checked_out {
            CheckedOutSurface::PixelBuffer(pixel_buffer) => {
                self.encode_checked_out_pixel_buffer(frame, pixel_buffer)
            }
            CheckedOutSurface::Texture(texture) => self.encode_checked_out_texture(frame, texture),
        };
        if let Err(release_refusal) =
            release_check_out_on_surface_share(&self.surface_share_connection, frame.surface_id())
        {
            tracing::warn!("{node_type_name}: {release_refusal}");
        }
        let access_units = encoded?;

        let coded_extent = self
            .minted_session
            .as_ref()
            .map(|minted| minted.encoder.encoder().aligned_extent())
            .unwrap_or((frame.width, frame.height));
        let outcome = self.publication.publish(
            encoded_video,
            &access_units,
            frame.timestamp_ns,
            coded_extent,
        );
        if outcome.next_frame_must_be_a_sync_point {
            self.next_frame_must_be_a_sync_point = true;
        }
        if let Some(refusal) = outcome.refusal {
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

    fn encode_checked_out_pixel_buffer(
        &mut self,
        frame: &ImageFrameReadByTheEncoder,
        checked_out: CheckedOutPixelBuffer,
    ) -> Result<Vec<EncodedVideoAccessUnit>, String> {
        let node_type_name = self.node.node_type_name;
        let pixel_format =
            PixelFormat::parse_wire_name(&checked_out.format_wire_name).map_err(|refusal| {
                format!(
                    "{node_type_name}: surface {:?}: {refusal}",
                    frame.surface_id()
                )
            })?;
        if pixel_format != PixelFormat::Rgba32 {
            return Err(format!(
                "{node_type_name}: surface {:?} is a {} pixel buffer; the encoder reads rgba32",
                frame.surface_id(),
                checked_out.format_wire_name
            ));
        }
        let (width, height, bytes_per_row) = (
            checked_out.width,
            checked_out.height,
            checked_out.bytes_per_row,
        );
        self.import_unless_already_imported(frame.surface_id(), checked_out)?;
        self.mint_unless_a_session_matches(frame, width, height)?;
        let session = self
            .minted_session
            .as_mut()
            .ok_or_else(|| format!("{node_type_name}: no encoder session"))?;
        if self.next_frame_must_be_a_sync_point {
            session.encoder.encoder_mut().force_idr();
        }
        let pixel_buffer = self
            .imported_pixel_buffers
            .get(pool_slot_key_of_surface_id(frame.surface_id()))
            .ok_or_else(|| {
                format!(
                    "{node_type_name}: surface {:?} was not imported",
                    frame.surface_id()
                )
            })?;
        let packets = session
            .encoder
            .encode_rgba_pixel_buffer(
                pixel_buffer,
                bytes_per_row,
                width,
                height,
                frame.timestamp_ns,
            )
            .map_err(|encode_failure| {
                format!("{node_type_name}: encode failed: {encode_failure}")
            })?;
        self.next_frame_must_be_a_sync_point = false;
        Ok(packets
            .into_iter()
            .map(|packet| EncodedVideoAccessUnit {
                annex_b_access_unit_bytes: packet.data,
                is_sync_point: packet.is_keyframe,
            })
            .collect())
    }

    /// Encode a texture in the layout its last user published, then hand it
    /// back to the producer's queue family and publish the layout it is left
    /// in, so the next reader's acquire names it.
    fn encode_checked_out_texture(
        &mut self,
        frame: &ImageFrameReadByTheEncoder,
        checked_out: CheckedOutTexture,
    ) -> Result<Vec<EncodedVideoAccessUnit>, String> {
        let node_type_name = self.node.node_type_name;
        let surface_id = frame.surface_id();
        let (width, height) = (checked_out.width, checked_out.height);
        let published_layout = VulkanLayout(checked_out.current_image_layout);
        self.import_texture_unless_already_imported(surface_id, checked_out)?;
        self.mint_unless_a_session_matches(frame, width, height)?;
        let session = self
            .minted_session
            .as_mut()
            .ok_or_else(|| format!("{node_type_name}: no encoder session"))?;
        if self.next_frame_must_be_a_sync_point {
            session.encoder.encoder_mut().force_idr();
        }
        let texture = self
            .imported_textures
            .get(pool_slot_key_of_surface_id(surface_id))
            .ok_or_else(|| format!("{node_type_name}: surface {surface_id:?} was not imported"))?;
        texture
            .acquire_from_foreign_layout(published_layout)
            .map_err(|refusal| {
                format!(
                    "{node_type_name}: texture {surface_id:?} could not be acquired from its \
                     producer: {refusal}"
                )
            })?;
        let rgba_image_view = texture.image_view().map_err(|refusal| {
            format!("{node_type_name}: texture {surface_id:?} has no image view: {refusal}")
        })?;
        let (packets, layout_left_in) = session
            .encoder
            .encoder_mut()
            .encode_rgba_image_published_in(
                texture.image(),
                rgba_image_view,
                published_layout,
                width,
                height,
                Some(frame.timestamp_ns),
            )
            .map_err(|encode_failure| {
                format!("{node_type_name}: encode failed: {encode_failure}")
            })?;
        texture
            .release_to_foreign_layout(layout_left_in, layout_left_in)
            .map_err(|refusal| {
                format!(
                    "{node_type_name}: texture {surface_id:?} could not be released to its \
                     producer: {refusal}"
                )
            })?;
        if layout_left_in != published_layout {
            publish_image_layout_on_surface_share(
                &self.surface_share_connection,
                surface_id,
                layout_left_in.0,
            )?;
        }
        self.next_frame_must_be_a_sync_point = false;
        Ok(packets
            .into_iter()
            .map(|packet| EncodedVideoAccessUnit {
                annex_b_access_unit_bytes: packet.data,
                is_sync_point: packet.is_keyframe,
            })
            .collect())
    }

    fn import_texture_unless_already_imported(
        &mut self,
        surface_id: &str,
        checked_out: CheckedOutTexture,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        if self
            .imported_textures
            .contains_key(pool_slot_key_of_surface_id(surface_id))
        {
            return Ok(());
        }
        let format = TextureFormat::from_wire_name(&checked_out.format_wire_name)
            .filter(|format| {
                matches!(
                    format,
                    TextureFormat::Rgba8Unorm
                        | TextureFormat::Rgba8UnormSrgb
                        | TextureFormat::Bgra8Unorm
                        | TextureFormat::Bgra8UnormSrgb
                )
            })
            .ok_or_else(|| {
                format!(
                    "{node_type_name}: surface {surface_id:?} is a {} texture; the encoder reads \
                     8-bit RGBA or BGRA",
                    checked_out.format_wire_name
                )
            })?;
        let consumer_vulkan_device = self.owned_device.consumer_vulkan_device();
        let (memory_fd, imported) = match checked_out.memory {
            CheckedOutTextureMemory::DmaBuf {
                memory_fd,
                drm_format_modifier,
                plane_offsets,
                plane_strides,
            } => {
                let raw_memory_fd = memory_fd.into_raw_fd();
                let imported = ConsumerVulkanTexture::import_render_target_dma_buf(
                    consumer_vulkan_device,
                    &[raw_memory_fd],
                    &plane_offsets,
                    &plane_strides,
                    drm_format_modifier,
                    checked_out.width,
                    checked_out.height,
                    format,
                    checked_out.allocation_byte_size,
                );
                (raw_memory_fd, imported)
            }
            CheckedOutTextureMemory::OpaqueFd {
                memory_fd,
                vk_memory_type_index,
                ..
            } => {
                let raw_memory_fd = memory_fd.into_raw_fd();
                let imported = ConsumerVulkanTexture::from_opaque_fd(
                    consumer_vulkan_device,
                    raw_memory_fd,
                    checked_out.width,
                    checked_out.height,
                    format,
                    checked_out.vk_image_usage_flags,
                    checked_out.allocation_byte_size,
                    vk_memory_type_index,
                );
                (raw_memory_fd, imported)
            }
        };
        let imported = imported.map_err(|import_failure| {
            // SAFETY: Vulkan refused the fd, so it is still ours alone.
            drop(unsafe { OwnedFd::from_raw_fd(memory_fd) });
            format!(
                "{node_type_name}: texture {surface_id:?} did not import onto the owned device: \
                 {import_failure}"
            )
        })?;
        if self.imported_textures.len() >= MOST_SURFACE_IMPORTS_KEPT {
            self.imported_textures.clear();
        }
        self.imported_textures
            .insert(pool_slot_key_of_surface_id(surface_id).to_owned(), imported);
        Ok(())
    }

    fn import_unless_already_imported(
        &mut self,
        surface_id: &str,
        checked_out: CheckedOutPixelBuffer,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        if self
            .imported_pixel_buffers
            .contains_key(pool_slot_key_of_surface_id(surface_id))
        {
            return Ok(());
        }
        let consumer_vulkan_device = self.owned_device.consumer_vulkan_device();
        let plane0_size = checked_out.plane_sizes.first().copied().unwrap_or(0);
        let imported = match checked_out.memory {
            CheckedOutPixelBufferMemory::DmaBuf { plane_fds } => {
                let [plane_fd] = <[OwnedFd; 1]>::try_from(plane_fds).map_err(|delivered| {
                    format!(
                        "{node_type_name}: surface {surface_id:?} crossed with {} DMA-BUF planes; an \
                         rgba32 pixel buffer has one",
                        delivered.len()
                    )
                })?;
                let plane_raw_fd = plane_fd.into_raw_fd();
                ConsumerVulkanBuffer::from_dma_buf_fds(
                    consumer_vulkan_device,
                    &[plane_raw_fd],
                    &[plane0_size],
                )
                .map_err(|import_failure| {
                    // SAFETY: Vulkan refused the fd, so it is still ours alone.
                    drop(unsafe { OwnedFd::from_raw_fd(plane_raw_fd) });
                    format!(
                        "{node_type_name}: surface {surface_id:?}'s DMA-BUF did not import onto the \
                             owned device: {import_failure}"
                    )
                })?
            }
            CheckedOutPixelBufferMemory::OpaqueFd {
                memory_fd,
                vk_memory_type_index,
                ..
            } => ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
                consumer_vulkan_device,
                memory_fd,
                plane0_size,
                vk_memory_type_index,
            )
            .map_err(|import_failure| {
                format!(
                    "{node_type_name}: surface {surface_id:?}'s opaque fd did not import onto the owned \
                     device at memory type {vk_memory_type_index}: {import_failure}"
                )
            })?,
        };
        if self.imported_pixel_buffers.len() >= MOST_SURFACE_IMPORTS_KEPT {
            self.imported_pixel_buffers.clear();
        }
        self.imported_pixel_buffers
            .insert(pool_slot_key_of_surface_id(surface_id).to_owned(), imported);
        Ok(())
    }

    fn mint_unless_a_session_matches(
        &mut self,
        frame: &ImageFrameReadByTheEncoder,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        if let Some(stale_session) = self
            .minted_session
            .take_if(|minted| (minted.minted_width, minted.minted_height) != (width, height))
        {
            tracing::info!(
                minted_width = stale_session.minted_width,
                minted_height = stale_session.minted_height,
                frame_width = width,
                frame_height = height,
                "{node_type_name}: upstream extent changed — re-minting the encoder session"
            );
        }
        if self.minted_session.is_none() {
            let shape =
                self.settings
                    .session_shape_for_frame(self.node.node_type_name, width, height);
            match self.mint_session(frame, shape) {
                Ok(minted) => {
                    self.publication.session_minted();
                    self.minted_session = Some(minted);
                }
                Err(mint_failure) => {
                    self.session_mint_already_failed = true;
                    return Err(format!("{mint_failure}; every later frame is discarded"));
                }
            }
        }
        Ok(())
    }

    fn mint_session(
        &self,
        frame: &ImageFrameReadByTheEncoder,
        shape: VideoEncodeSessionShape,
    ) -> Result<VideoEncodeSessionMintedFromAFrame, String> {
        let node_type_name = self.node.node_type_name;
        let color_vui =
            Some(frame.color_vui).filter(|color_vui| color_vui.is_video_signal_type_block_needed());
        let config = SimpleEncoderConfig {
            width: shape.width,
            height: shape.height,
            fps: shape.frames_per_second,
            codec: self.node.vulkan_video_codec(),
            preset: Preset::Medium,
            qp: None,
            bitrate_bps: self.settings.bitrate_bps,
            streaming: true,
            idr_interval_secs: shape.keyframe_interval_seconds,
            prepend_header_to_idr: Some(true),
            effort_level: self.settings.effort_level,
            color_vui,
        };
        let mint_started = Instant::now();
        let codec_device: Arc<dyn VulkanVideoCodecDevice> = self.owned_device.clone();
        let encoder = SimpleEncoder::new(codec_device, config).map_err(|mint_failure| {
            format!("{node_type_name}: the encoder session could not be minted: {mint_failure}")
        })?;
        let (coded_width, coded_height) = encoder.aligned_extent();
        tracing::info!(
            width = shape.width,
            height = shape.height,
            fps = shape.frames_per_second,
            coded_width,
            coded_height,
            session_minted_ms = mint_started.elapsed().as_secs_f64() * 1_000.0,
            "{node_type_name}: session minted from the first frame"
        );
        Ok(VideoEncodeSessionMintedFromAFrame {
            encoder: RgbaPixelBufferVideoEncoder::new(encoder),
            minted_width: shape.width,
            minted_height: shape.height,
        })
    }
}
