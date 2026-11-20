// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One encoder node's session on a Vulkan Video encode device: each frame
//! checked out over surface-share, imported onto the device and encoded
//! there. The device is the machine's encode host's, shared by every
//! session it runs on that GPU, or one the node's helper opened for itself.

use std::collections::HashMap;
use std::os::fd::{FromRawFd as _, IntoRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, ConsumerVulkanTexture, H273ColorVui, OwnedVulkanVideoCodecDevice,
    PixelFormat, TextureFormat, VulkanLayout,
};
use streamlib_surface_client::{
    CheckedOutPixelBuffer, CheckedOutPixelBufferMemory, CheckedOutSurface, CheckedOutTexture,
    CheckedOutTextureMemory, check_out_surface_on_surface_share, connect_to_surface_share_socket,
    pool_slot_key_of_surface_id, publish_image_layout_on_surface_share,
    release_check_out_on_surface_share,
};
use streamlib_vulkan_video::{
    Preset, RgbaPixelBufferVideoEncoder, SimpleEncoder, SimpleEncoderConfig, VideoError,
    VulkanVideoCodecDevice,
};

use crate::VideoCodecNodeIdentity;
use crate::encoded_video_publication::EncodedVideoAccessUnit;
use crate::image_frame_on_the_video_port::ImageFrameReadByTheEncoder;
use crate::video_encode_session_mint_gate::{
    ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL, EncodeSessionMintGate,
};
use crate::video_encoder_settings::{VideoEncodeSessionShape, VideoEncoderSettings};

/// Imports kept, one per producer pool slot; a producer's pool is far smaller.
const MOST_SURFACE_IMPORTS_KEPT: usize = 32;

/// One frame to encode: the published surface its pixels are checked out
/// from, and what its descriptor states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFrameToEncode {
    pub surface_id: String,
    pub width: u32,
    pub height: u32,
    /// Monotonic nanoseconds, the stamp the media frame carries.
    pub timestamp_ns: i64,
    #[serde(with = "H273ColorVuiAsItCrosses")]
    pub color_vui: H273ColorVui,
}

/// [`H273ColorVui`]'s fields, as a frame carries them to the encode host.
#[derive(Serialize, Deserialize)]
#[serde(remote = "H273ColorVui")]
struct H273ColorVuiAsItCrosses {
    primaries: Option<u8>,
    transfer: Option<u8>,
    matrix: Option<u8>,
    full_range: Option<bool>,
}

impl VideoFrameToEncode {
    /// The frame an encoder node read off its `video` port.
    pub fn of_the_frame_read(frame: &ImageFrameReadByTheEncoder) -> Self {
        Self {
            surface_id: frame.surface_id().to_owned(),
            width: frame.width,
            height: frame.height,
            timestamp_ns: frame.timestamp_ns,
            color_vui: frame.color_vui,
        }
    }

    fn surface_id(&self) -> &str {
        &self.surface_id
    }
}

/// What encoding one frame came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoFrameEncodeOutcome {
    Encoded(VideoFrameEncoded),
    /// Its producer recycled the surface before the check-out, so it was
    /// skipped.
    NotCheckedOut,
    /// No session is minted and no mint may be tried yet, so the frame is
    /// discarded.
    DiscardedWithoutASession,
}

/// One frame's access units and the session they were coded in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFrameEncoded {
    pub access_units: Vec<EncodedVideoAccessUnit>,
    /// The session's coded extent, aligned up from the frame's.
    pub coded_extent: (u32, u32),
    /// Whether this frame minted a new session, so its first access unit
    /// opens a new coded sequence.
    pub minted_a_session: bool,
}

/// Why no encoder session was minted.
#[derive(Debug)]
pub(crate) enum EncodeSessionNotMinted {
    /// The GPU's driver holds as many video sessions as it allows; a mint
    /// can succeed once another session ends.
    AtTheVideoSessionLimit(String),
    /// Any other failure, which a later mint would meet again.
    Failed(String),
}

/// The encoder session and the extent it was minted for.
struct VideoEncodeSessionMintedFromAFrame {
    encoder: RgbaPixelBufferVideoEncoder,
    minted_width: u32,
    minted_height: u32,
}

/// One node's session on an encode device: the device, its surface-share
/// connection, the surfaces imported onto the device and the session
/// minted from the frames.
pub struct VideoEncodeOnADevice {
    node: VideoCodecNodeIdentity,
    settings: VideoEncoderSettings,
    device: Arc<OwnedVulkanVideoCodecDevice>,
    surface_share_connection: UnixStream,
    imported_pixel_buffers: HashMap<String, ConsumerVulkanBuffer>,
    imported_textures: HashMap<String, ConsumerVulkanTexture>,
    minted_session: Option<VideoEncodeSessionMintedFromAFrame>,
    session_mint_gate: EncodeSessionMintGate,
    next_frame_must_be_a_sync_point: bool,
    /// Set when a session is minted and cleared once a frame's outcome
    /// reports it, so a mint whose first frame failed is still reported.
    session_minted_and_not_yet_reported: bool,
    frames_not_checked_out: u64,
}

impl VideoEncodeOnADevice {
    /// Encode on `device`, checking frames out of the surface-share service
    /// at `surface_share_socket_path`.
    pub fn open(
        node: VideoCodecNodeIdentity,
        settings: VideoEncoderSettings,
        device: Arc<OwnedVulkanVideoCodecDevice>,
        surface_share_socket_path: &Path,
    ) -> Result<Self, String> {
        let node_type_name = node.node_type_name;
        let surface_share_connection = connect_to_surface_share_socket(surface_share_socket_path)
            .map_err(|connect_failure| {
            format!(
                "{node_type_name}: the surface-share socket refused a connection: {connect_failure}"
            )
        })?;
        Ok(Self {
            node,
            settings,
            device,
            surface_share_connection,
            imported_pixel_buffers: HashMap::new(),
            imported_textures: HashMap::new(),
            minted_session: None,
            session_mint_gate: EncodeSessionMintGate::default(),
            next_frame_must_be_a_sync_point: false,
            session_minted_and_not_yet_reported: false,
            frames_not_checked_out: 0,
        })
    }

    /// The name of the device this session encodes on.
    pub fn device_name(&self) -> String {
        self.device.consumer_vulkan_device().name().to_string()
    }

    /// Encode `frame`, as a sync point when `as_a_sync_point`.
    pub fn encode(
        &mut self,
        frame: &VideoFrameToEncode,
        as_a_sync_point: bool,
    ) -> Result<VideoFrameEncodeOutcome, String> {
        let node_type_name = self.node.node_type_name;
        if self.minted_session.is_none()
            && !self
                .session_mint_gate
                .a_mint_may_be_tried_at(Instant::now())
        {
            return Ok(VideoFrameEncodeOutcome::DiscardedWithoutASession);
        }
        self.next_frame_must_be_a_sync_point |= as_a_sync_point;
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
                return Ok(VideoFrameEncodeOutcome::NotCheckedOut);
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
        Ok(VideoFrameEncodeOutcome::Encoded(VideoFrameEncoded {
            access_units,
            coded_extent,
            minted_a_session: std::mem::take(&mut self.session_minted_and_not_yet_reported),
        }))
    }

    fn encode_checked_out_pixel_buffer(
        &mut self,
        frame: &VideoFrameToEncode,
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
        frame: &VideoFrameToEncode,
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
        let consumer_vulkan_device = self.device.consumer_vulkan_device();
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
                "{node_type_name}: texture {surface_id:?} did not import onto the encode device: \
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
        let consumer_vulkan_device = self.device.consumer_vulkan_device();
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
                             encode device: {import_failure}"
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
                    "{node_type_name}: surface {surface_id:?}'s opaque fd did not import onto the encode \
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
        frame: &VideoFrameToEncode,
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
                    let mints_refused = self.session_mint_gate.minted();
                    if mints_refused > 0 {
                        tracing::info!(
                            mints_refused,
                            "{node_type_name}: a video session freed on the GPU, so the encoder \
                             session was minted"
                        );
                    }
                    self.session_minted_and_not_yet_reported = true;
                    self.minted_session = Some(minted);
                }
                Err(EncodeSessionNotMinted::AtTheVideoSessionLimit(refusal)) => {
                    let mints_refused = self
                        .session_mint_gate
                        .refused_at_the_session_limit_at(Instant::now());
                    return Err(format!(
                        "{refusal}; frames are discarded and the mint is tried again every \
                         {ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL:?} until another video session \
                         on this machine ends ({mints_refused} refused so far)"
                    ));
                }
                Err(EncodeSessionNotMinted::Failed(mint_failure)) => {
                    self.session_mint_gate.failed_for_good();
                    return Err(format!("{mint_failure}; every later frame is discarded"));
                }
            }
        }
        Ok(())
    }

    fn mint_session(
        &self,
        frame: &VideoFrameToEncode,
        shape: VideoEncodeSessionShape,
    ) -> Result<VideoEncodeSessionMintedFromAFrame, EncodeSessionNotMinted> {
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
        let codec_device: Arc<dyn VulkanVideoCodecDevice> = self.device.clone();
        let encoder = SimpleEncoder::new(codec_device, config)
            .map_err(|mint_failure| why_no_session_was_minted(node_type_name, mint_failure))?;
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

/// What `mint_failure` means for an encoder of `node_type_name`: the GPU's
/// video session limit is waited out, and any other failure is for good.
pub(crate) fn why_no_session_was_minted(
    node_type_name: &str,
    mint_failure: VideoError,
) -> EncodeSessionNotMinted {
    let refusal =
        format!("{node_type_name}: the encoder session could not be minted: {mint_failure}");
    match mint_failure {
        VideoError::VideoSessionLimitReached { .. } => {
            EncodeSessionNotMinted::AtTheVideoSessionLimit(refusal)
        }
        _ => EncodeSessionNotMinted::Failed(refusal),
    }
}
