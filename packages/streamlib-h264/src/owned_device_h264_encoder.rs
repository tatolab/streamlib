// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! H.264 encode on a Vulkan device this module owns: each published frame is
//! checked out over surface-share, imported onto the owned device and encoded
//! there, so a fault in the video engine ends this helper and nothing else.

use std::collections::HashMap;
use std::os::fd::{FromRawFd as _, IntoRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, OwnedVulkanVideoCodecDevice, PixelFormat, VulkanVideoCodecDirection,
    runtime_vulkan_device_uuid_named_at_spawn,
};
use streamlib_surface_client::{
    CheckedOutPixelBuffer, CheckedOutPixelBufferMemory, check_out_pixel_buffer_on_surface_share,
    connect_to_surface_share_socket, release_check_out_on_surface_share,
};
use streamlib_vulkan_video::{
    Codec, Preset, RgbaPixelBufferVideoEncoder, SimpleEncoder, SimpleEncoderConfig,
    VulkanVideoCodecDevice,
};

use crate::encoded_video_publication::{
    EncodedH264AccessUnit, EncodedVideoOutputPort, EncodedVideoPublication,
};
use crate::h264_encoder_settings::{H264EncodeSessionShape, H264EncoderSettings};
use crate::video_frame_message::VideoFrameMessage;

/// The variable a processor interpreter is handed the surface-share socket in.
const SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_SURFACE_SOCKET";

/// Imports kept per surface id; a producer's pool is far smaller.
const MOST_PIXEL_BUFFER_IMPORTS_KEPT: usize = 32;

const ENCODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// The encoder session and the extent it was minted for.
struct H264EncodeSessionMintedFromAFrame {
    encoder: RgbaPixelBufferVideoEncoder,
    minted_width: u32,
    minted_height: u32,
}

/// `H264Encoder`'s native half: the owned device, its surface-share
/// connection, and the session minted from the frames it receives.
pub struct OwnedDeviceH264Encoder {
    settings: H264EncoderSettings,
    owned_device: Arc<OwnedVulkanVideoCodecDevice>,
    surface_share_connection: UnixStream,
    imported_pixel_buffers: HashMap<String, ConsumerVulkanBuffer>,
    minted_session: Option<H264EncodeSessionMintedFromAFrame>,
    session_mint_already_failed: bool,
    next_frame_must_be_a_sync_point: bool,
    publication: EncodedVideoPublication,
    frames_encoded: u64,
    frames_not_checked_out: u64,
    publication_refusals: u64,
}

impl OwnedDeviceH264Encoder {
    /// Create the owned device on the GPU the runtime named and connect to
    /// its surface-share service.
    pub fn open(settings: H264EncoderSettings) -> Result<Self, String> {
        let opening_started = Instant::now();
        let runtime_vulkan_device_uuid = runtime_vulkan_device_uuid_named_at_spawn()
            .map_err(|refusal| format!("H264Encoder: {refusal}"))?;
        let owned_device = Arc::new(
            OwnedVulkanVideoCodecDevice::new_on_the_device_the_runtime_named(
                runtime_vulkan_device_uuid,
                VulkanVideoCodecDirection::Encode,
            )
            .map_err(|refusal| {
                format!("H264Encoder: the owned Vulkan Video encode device was refused: {refusal}")
            })?,
        );
        let owned_device_created_ms = opening_started.elapsed().as_secs_f64() * 1_000.0;
        let surface_share_socket_path = std::env::var_os(SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE)
            .ok_or_else(|| {
                format!(
                    "H264Encoder: {SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE} is not set, so no \
                     published frame can be checked out"
                )
            })?;
        let surface_share_connection = connect_to_surface_share_socket(Path::new(
            &surface_share_socket_path,
        ))
        .map_err(|connect_failure| {
            format!("H264Encoder: the surface-share socket refused a connection: {connect_failure}")
        })?;
        tracing::info!(
            owned_device_created_ms,
            device = owned_device.consumer_vulkan_device().name(),
            "H264Encoder: encoding on an owned Vulkan Video device"
        );
        Ok(Self {
            settings,
            owned_device,
            surface_share_connection,
            imported_pixel_buffers: HashMap::new(),
            minted_session: None,
            session_mint_already_failed: false,
            next_frame_must_be_a_sync_point: false,
            publication: EncodedVideoPublication::default(),
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
        frame: &VideoFrameMessage,
        encoded_video: &dyn EncodedVideoOutputPort,
    ) -> Result<(), String> {
        if self.minted_session.is_none() && self.session_mint_already_failed {
            return Ok(());
        }
        let checked_out = match check_out_pixel_buffer_on_surface_share(
            &self.surface_share_connection,
            &frame.surface_id,
        ) {
            Ok(checked_out) => checked_out,
            Err(refusal) => {
                self.frames_not_checked_out += 1;
                if self.frames_not_checked_out.is_power_of_two() {
                    tracing::warn!(
                        frames_not_checked_out = self.frames_not_checked_out,
                        "H264Encoder: a frame could not be checked out and was skipped: {refusal}"
                    );
                }
                return Ok(());
            }
        };
        let encoded = self.encode_checked_out_pixel_buffer(frame, checked_out);
        if let Err(release_refusal) =
            release_check_out_on_surface_share(&self.surface_share_connection, &frame.surface_id)
        {
            tracing::warn!("H264Encoder: {release_refusal}");
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
                    "H264Encoder: {refusal}; dropping to the next sync point and encoding the \
                     next frame as one"
                );
            }
        }

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("H264Encoder: first frame encoded");
        } else if self
            .frames_encoded
            .is_multiple_of(ENCODE_PROGRESS_LOG_INTERVAL_FRAMES)
        {
            tracing::info!(
                frames_encoded = self.frames_encoded,
                "H264Encoder: encode progress"
            );
        }
        Ok(())
    }

    fn encode_checked_out_pixel_buffer(
        &mut self,
        frame: &VideoFrameMessage,
        checked_out: CheckedOutPixelBuffer,
    ) -> Result<Vec<EncodedH264AccessUnit>, String> {
        let pixel_format = PixelFormat::parse_wire_name(&checked_out.format_wire_name)
            .map_err(|refusal| format!("H264Encoder: surface {:?}: {refusal}", frame.surface_id))?;
        if pixel_format != PixelFormat::Rgba32 {
            return Err(format!(
                "H264Encoder: surface {:?} is a {} pixel buffer; the encoder reads rgba32",
                frame.surface_id, checked_out.format_wire_name
            ));
        }
        let (width, height, bytes_per_row) = (
            checked_out.width,
            checked_out.height,
            checked_out.bytes_per_row,
        );
        self.import_unless_already_imported(&frame.surface_id, checked_out)?;
        self.mint_unless_a_session_matches(frame, width, height)?;
        let session = self
            .minted_session
            .as_mut()
            .ok_or_else(|| "H264Encoder: no encoder session".to_owned())?;
        if self.next_frame_must_be_a_sync_point {
            session.encoder.encoder_mut().force_idr();
        }
        let pixel_buffer = self
            .imported_pixel_buffers
            .get(&frame.surface_id)
            .ok_or_else(|| {
                format!(
                    "H264Encoder: surface {:?} was not imported",
                    frame.surface_id
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
            .map_err(|encode_failure| format!("H264Encoder: encode failed: {encode_failure}"))?;
        self.next_frame_must_be_a_sync_point = false;
        Ok(packets
            .into_iter()
            .map(|packet| EncodedH264AccessUnit {
                annex_b_access_unit_bytes: packet.data,
                is_sync_point: packet.is_keyframe,
            })
            .collect())
    }

    fn import_unless_already_imported(
        &mut self,
        surface_id: &str,
        checked_out: CheckedOutPixelBuffer,
    ) -> Result<(), String> {
        if self.imported_pixel_buffers.contains_key(surface_id) {
            return Ok(());
        }
        let consumer_vulkan_device = self.owned_device.consumer_vulkan_device();
        let plane0_size = checked_out.plane_sizes.first().copied().unwrap_or(0);
        let imported = match checked_out.memory {
            CheckedOutPixelBufferMemory::DmaBuf { plane_fds } => {
                let [plane_fd] = <[OwnedFd; 1]>::try_from(plane_fds).map_err(|delivered| {
                    format!(
                        "H264Encoder: surface {surface_id:?} crossed with {} DMA-BUF planes; an \
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
                        "H264Encoder: surface {surface_id:?}'s DMA-BUF did not import onto the \
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
                    "H264Encoder: surface {surface_id:?}'s opaque fd did not import onto the owned \
                     device at memory type {vk_memory_type_index}: {import_failure}"
                )
            })?,
        };
        if self.imported_pixel_buffers.len() >= MOST_PIXEL_BUFFER_IMPORTS_KEPT {
            self.imported_pixel_buffers.clear();
        }
        self.imported_pixel_buffers
            .insert(surface_id.to_owned(), imported);
        Ok(())
    }

    fn mint_unless_a_session_matches(
        &mut self,
        frame: &VideoFrameMessage,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if let Some(stale_session) = self
            .minted_session
            .take_if(|minted| (minted.minted_width, minted.minted_height) != (width, height))
        {
            tracing::info!(
                minted_width = stale_session.minted_width,
                minted_height = stale_session.minted_height,
                frame_width = width,
                frame_height = height,
                "H264Encoder: upstream extent changed — re-minting the encoder session"
            );
        }
        if self.minted_session.is_none() {
            let shape = self
                .settings
                .session_shape_for_frame(width, height, frame.fps);
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
        frame: &VideoFrameMessage,
        shape: H264EncodeSessionShape,
    ) -> Result<H264EncodeSessionMintedFromAFrame, String> {
        let color_vui = frame
            .color_info
            .as_ref()
            .map(|color_info| color_info.to_h273_color_vui())
            .transpose()
            .map_err(|refusal| format!("H264Encoder: {refusal}"))?
            .filter(|color_vui| color_vui.is_video_signal_type_block_needed());
        let config = SimpleEncoderConfig {
            width: shape.width,
            height: shape.height,
            fps: shape.frames_per_second,
            codec: Codec::H264,
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
            format!("H264Encoder: the encoder session could not be minted: {mint_failure}")
        })?;
        let (coded_width, coded_height) = encoder.aligned_extent();
        tracing::info!(
            width = shape.width,
            height = shape.height,
            fps = shape.frames_per_second,
            coded_width,
            coded_height,
            session_minted_ms = mint_started.elapsed().as_secs_f64() * 1_000.0,
            "H264Encoder: session minted from the first frame"
        );
        Ok(H264EncodeSessionMintedFromAFrame {
            encoder: RgbaPixelBufferVideoEncoder::new(encoder),
            minted_width: shape.width,
            minted_height: shape.height,
        })
    }
}
