// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! H.264 or H.265 encode on Apple's media engine: the host converts each
//! published frame on the GPU into an NV12 IOSurface from the session's own
//! pool, and VideoToolbox encodes that surface.

use std::collections::HashMap;
use std::time::Instant;

use objc2_core_foundation::CFRetained;
use objc2_core_video::CVPixelBufferGetIOSurface;
use objc2_io_surface::{IOSurfaceID, IOSurfaceRef};
use streamlib_consumer_rhi::H273ColorVui;
use streamlib_consumer_rhi::h273_color_vui::color_vui_every_engine_encoder_writes;
use tatolab_module_sdk::{
    ModuleHost, ModuleHostColorInfo, ModuleHostForeignIOSurface, ModuleHostSurface,
};

use super::videotoolbox_compression_session::{
    COMPRESSION_SESSION_POOL_PIXEL_FORMAT_WIRE_NAME, VideoToolboxCompressionSession,
    VideoToolboxCompressionSessionRequest,
};
use crate::VideoCodecNodeIdentity;
use crate::encoded_video_publication::{EncodedVideoOutputPort, EncodedVideoPublication};
use crate::image_frame_on_the_video_port::{H273ColorVuiWireNames, ImageFrameReadByTheEncoder};
use crate::video_encoder_settings::VideoEncoderSettings;

/// Pool IOSurfaces kept adopted; the session's pool is far smaller.
const MOST_ADOPTED_IOSURFACES_KEPT: usize = 32;

const ENCODE_PROGRESS_LOG_INTERVAL_FRAMES: u64 = 300;

/// A pool IOSurface the session hands frames in, and the surface its adoption
/// made.
struct PoolIOSurfaceAdoptedAsSurface {
    adopted_surface: ModuleHostSurface,
    /// Held so the id the adoption is keyed by names this surface while the
    /// adoption does.
    _iosurface: CFRetained<IOSurfaceRef>,
}

// SAFETY: the retained IOSurface is only kept alive here, never messaged, and
// IOSurface references may be released from any thread.
unsafe impl Send for PoolIOSurfaceAdoptedAsSurface {}

/// The session, the extent it was minted for, the colour its pool's pixels
/// are written in, and the adoptions of its pool's surfaces.
struct CompressionSessionMintedFromAFrame {
    compression_session: VideoToolboxCompressionSession,
    minted_width: u32,
    minted_height: u32,
    encoded_color_info: H273ColorVuiWireNames,
    adopted_pool_surfaces_by_iosurface_id: HashMap<IOSurfaceID, PoolIOSurfaceAdoptedAsSurface>,
}

/// A video encoder node's native half on macOS: the session minted from the
/// frames it receives, and the publication of what it codes.
pub struct VideoToolboxVideoEncoder {
    node: VideoCodecNodeIdentity,
    module_host: ModuleHost,
    settings: VideoEncoderSettings,
    minted_session: Option<CompressionSessionMintedFromAFrame>,
    session_mint_already_failed: bool,
    next_frame_must_be_a_sync_point: bool,
    publication: EncodedVideoPublication,
    frames_encoded: u64,
    frames_not_converted: u64,
    publication_refusals: u64,
}

impl VideoToolboxVideoEncoder {
    /// An encoder whose session mints from the first frame, refusing by name
    /// a setting VideoToolbox has no property for.
    pub fn open(
        node: VideoCodecNodeIdentity,
        module_host: ModuleHost,
        settings: VideoEncoderSettings,
    ) -> Result<Self, String> {
        let node_type_name = node.node_type_name;
        if let Some(effort_level) = settings.effort_level {
            return Err(format!(
                "{node_type_name} on VideoToolbox refuses effort_level = {effort_level}: \
                 VideoToolbox exposes no encoder-effort index to map it onto. Leave \
                 effort_level unset on macOS."
            ));
        }
        tracing::info!("{node_type_name}: encoding on VideoToolbox");
        Ok(Self {
            node,
            module_host,
            settings,
            minted_session: None,
            session_mint_already_failed: false,
            next_frame_must_be_a_sync_point: false,
            publication: EncodedVideoPublication::new(node.elementary_stream),
            frames_encoded: 0,
            frames_not_converted: 0,
            publication_refusals: 0,
        })
    }

    /// Encode `frame` and publish its access units on `encoded_video`. A
    /// frame its producer recycled before the conversion is skipped; a frame
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
        self.mint_unless_a_session_matches(frame)?;
        let session = self
            .minted_session
            .as_mut()
            .ok_or_else(|| format!("{node_type_name}: no compression session"))?;
        let pool_pixel_buffer = session
            .compression_session
            .pixel_buffer_from_the_session_pool()
            .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
        let pool_iosurface =
            CVPixelBufferGetIOSurface(Some(&pool_pixel_buffer)).ok_or_else(|| {
                format!("{node_type_name}: the compression session's pool handed out no IOSurface")
            })?;
        let converted = convert_the_frame_into_the_pool_surface(
            &self.module_host,
            session,
            frame,
            pool_iosurface,
        );
        if let Err(refusal) = converted {
            self.frames_not_converted += 1;
            if self.frames_not_converted.is_power_of_two() {
                tracing::warn!(
                    frames_not_converted = self.frames_not_converted,
                    "{node_type_name}: a frame could not be converted for the encoder and was \
                     skipped: {refusal}"
                );
            }
            return Ok(());
        }
        let access_units = session
            .compression_session
            .encode_pool_pixel_buffer(&pool_pixel_buffer, self.next_frame_must_be_a_sync_point)
            .map_err(|encode_failure| {
                format!("{node_type_name}: encode failed: {encode_failure}")
            })?;
        self.next_frame_must_be_a_sync_point = false;

        let outcome = self.publication.publish(
            encoded_video,
            &access_units,
            frame.timestamp_ns,
            session.compression_session.coded_extent(),
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

    fn mint_unless_a_session_matches(
        &mut self,
        frame: &ImageFrameReadByTheEncoder,
    ) -> Result<(), String> {
        let node_type_name = self.node.node_type_name;
        if let Some(stale_session) = self.minted_session.take_if(|minted| {
            (minted.minted_width, minted.minted_height) != (frame.width, frame.height)
        }) {
            tracing::info!(
                minted_width = stale_session.minted_width,
                minted_height = stale_session.minted_height,
                frame_width = frame.width,
                frame_height = frame.height,
                "{node_type_name}: upstream extent changed — re-minting the compression session"
            );
        }
        if self.minted_session.is_none() {
            match self.mint_session(frame) {
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
    ) -> Result<CompressionSessionMintedFromAFrame, String> {
        let node_type_name = self.node.node_type_name;
        let encoded_color_vui: H273ColorVui =
            color_vui_every_engine_encoder_writes(Some(frame.color_vui));
        let shape =
            self.settings
                .session_shape_for_frame(node_type_name, frame.width, frame.height);
        let mint_started = Instant::now();
        let compression_session =
            VideoToolboxCompressionSession::open(&VideoToolboxCompressionSessionRequest {
                elementary_stream: self.node.elementary_stream,
                width: shape.width,
                height: shape.height,
                frames_per_second: shape.frames_per_second,
                keyframe_interval_seconds: shape.keyframe_interval_seconds,
                bitrate_bps: self.settings.bitrate_bps,
                color_vui: encoded_color_vui,
            })
            .map_err(|mint_failure| {
                format!(
                    "{node_type_name}: the compression session could not be minted: {mint_failure}"
                )
            })?;
        let (coded_width, coded_height) = compression_session.coded_extent();
        tracing::info!(
            width = shape.width,
            height = shape.height,
            fps = shape.frames_per_second,
            coded_width,
            coded_height,
            session_minted_ms = mint_started.elapsed().as_secs_f64() * 1_000.0,
            "{node_type_name}: session minted from the first frame"
        );
        Ok(CompressionSessionMintedFromAFrame {
            compression_session,
            minted_width: shape.width,
            minted_height: shape.height,
            encoded_color_info: H273ColorVuiWireNames::of_h273_color_vui(&encoded_color_vui),
            adopted_pool_surfaces_by_iosurface_id: HashMap::new(),
        })
    }
}

/// Have the host convert `frame`'s texture into `pool_iosurface` in the colour
/// the session signals, adopting the pool surface on the first frame it
/// carries.
fn convert_the_frame_into_the_pool_surface(
    module_host: &ModuleHost,
    session: &mut CompressionSessionMintedFromAFrame,
    frame: &ImageFrameReadByTheEncoder,
    pool_iosurface: CFRetained<IOSurfaceRef>,
) -> Result<(), String> {
    let iosurface_id = pool_iosurface.id();
    if !session
        .adopted_pool_surfaces_by_iosurface_id
        .contains_key(&iosurface_id)
    {
        let adopted_surface = module_host
            .import_iosurface(&ModuleHostForeignIOSurface {
                iosurface: CFRetained::as_ptr(&pool_iosurface)
                    .as_ptr()
                    .cast_const()
                    .cast(),
                pixel_format: COMPRESSION_SESSION_POOL_PIXEL_FORMAT_WIRE_NAME,
                width: session.minted_width,
                height: session.minted_height,
            })
            .map_err(|refusal| format!("the host did not adopt a pool IOSurface: {refusal}"))?;
        if session.adopted_pool_surfaces_by_iosurface_id.len() >= MOST_ADOPTED_IOSURFACES_KEPT {
            session.adopted_pool_surfaces_by_iosurface_id.clear();
        }
        session.adopted_pool_surfaces_by_iosurface_id.insert(
            iosurface_id,
            PoolIOSurfaceAdoptedAsSurface {
                adopted_surface,
                _iosurface: pool_iosurface,
            },
        );
    }
    let adopted_pool_surface = &session
        .adopted_pool_surfaces_by_iosurface_id
        .get(&iosurface_id)
        .ok_or_else(|| format!("IOSurface {iosurface_id} is missing from the adopted set"))?
        .adopted_surface;
    let encoded_color_info = session.encoded_color_info;
    module_host
        .convert_surface_into(
            frame.lent_pooled_frame(),
            adopted_pool_surface,
            &ModuleHostColorInfo {
                primaries: encoded_color_info.primaries,
                transfer: encoded_color_info.transfer,
                matrix: encoded_color_info.matrix,
                range: encoded_color_info.range,
            },
        )
        .map_err(|refusal| refusal.to_string())
}
