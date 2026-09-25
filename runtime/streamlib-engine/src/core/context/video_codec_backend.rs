// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video codec seam: the one path anything opens a hardware video encode
//! or decode session through.
//!
//! It sits beside the video device seam and composes the same first-arm-that-
//! opens walk, so the codec built-ins are written against session shapes that
//! name no platform, with each platform's codec API an arm behind them. The
//! wire either side of a session is Annex-B, whatever the arm speaks inside.

use std::sync::{Arc, OnceLock};

use super::GpuContextFullAccess;
use super::device_backend_probe_chain::{
    DeviceBackendArm, first_device_backend_arm_that_opens_among,
};
use super::refusing_null_video_codec_backend::RefusingNullVideoCodecBackend;
use crate::core::Result;
use crate::core::color::H273ColorVui;
use crate::core::rhi::{PixelBuffer, PublishedPixelBufferFrameId};

/// The elementary stream a codec session encodes or decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecElementaryStream {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    H265,
}

/// What a caller asks a backend to open an encode session for.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoEncodeSessionRequest {
    /// The elementary stream the session produces.
    pub elementary_stream: VideoCodecElementaryStream,
    /// Source width in pixels.
    pub width: u32,
    /// Source height in pixels.
    pub height: u32,
    /// The rate the session's rate control and sync-point cadence assume.
    pub frames_per_second: u32,
    /// Target bitrate in bits per second. `None`: constant-quality encoding
    /// at the arm's balanced preset.
    pub bitrate_bps: Option<u32>,
    /// Seconds between sync points.
    pub keyframe_interval_seconds: u32,
    /// The arm's encoder-effort index. `None`: the arm's default.
    pub effort_level: Option<u32>,
    /// The colour the parameter sets' VUI signals. `None` emits no colour
    /// description block.
    pub color_vui: Option<H273ColorVui>,
}

/// The published surface one encode call reads, as a video-frame bag names it.
#[derive(Debug, Clone, Copy)]
pub struct VideoEncodeSourceSurface<'a> {
    /// The bag's `surface_id`.
    pub surface_id: &'a str,
    /// The bag's per-frame texture-layout override, when it carries one.
    pub texture_layout: Option<i32>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// The frame's timestamp, carried through to the access units it yields.
    pub timestamp_ns: i64,
}

/// One access unit an encode session completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedVideoAccessUnitFromSession {
    /// The access unit in Annex-B. A sync point carries its parameter sets in
    /// front, so it is a self-sufficient decode entry point.
    pub annex_b_access_unit_bytes: Vec<u8>,
    /// Whether a decoder can begin at this access unit.
    pub is_sync_point: bool,
    /// The timestamp of the source frame this access unit encodes, when the
    /// arm carried one through.
    pub timestamp_ns: Option<i64>,
}

/// An encode session a backend opened.
///
/// Streaming shape on every arm: no reordered frames, a sync point at the
/// requested cadence, parameter sets in front of every sync point.
pub trait VideoEncodeSession: Send {
    /// The codec-aligned extent the stream is coded at, before the
    /// parameter sets' conformance crop.
    fn coded_extent(&self) -> (u32, u32);

    /// Encode one published surface, handing back every access unit it
    /// completed.
    fn encode_published_surface(
        &mut self,
        source: &VideoEncodeSourceSurface<'_>,
    ) -> Result<Vec<EncodedVideoAccessUnitFromSession>>;
}

/// What a caller asks a backend to open a decode session for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDecodeSessionRequest {
    /// The elementary stream the session consumes.
    pub elementary_stream: VideoCodecElementaryStream,
    /// Upper bound on the coded width. `0`, paired with a `0` height:
    /// detected from the stream's first parameter sets.
    pub max_width: u32,
    /// Upper bound on the coded height, paired with [`Self::max_width`].
    pub max_height: u32,
}

/// One picture a decode session reconstructed, already in a pooled `Rgba32`
/// pixel buffer at the stream's conformance-window extent.
#[derive(Debug)]
pub struct DecodedVideoPictureInPooledPixelBuffer {
    /// The id the pooled pixel buffer publishes the picture under — what a
    /// bag's `surface_id` carries.
    pub published_pixel_buffer_frame_id: PublishedPixelBufferFrameId,
    /// The pooled slot, held so the pool cannot rotate it out before the
    /// picture's bag is written.
    pub pixel_buffer: PixelBuffer,
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
}

/// A decode session a backend opened.
pub trait VideoDecodeSession: Send {
    /// Decode one Annex-B access unit, pushing each picture it completed onto
    /// `decoded_pictures_in_completion_order` as it completes, so a failure
    /// part way through leaves the caller holding every picture before it.
    fn decode_annex_b_access_unit(
        &mut self,
        annex_b_access_unit_bytes: &[u8],
        decoded_pictures_in_completion_order: &mut Vec<DecodedVideoPictureInPooledPixelBuffer>,
    ) -> Result<()>;

    /// The colour the stream's parsed parameter sets signal, once any have
    /// been parsed.
    fn parsed_parameter_set_color_vui(&self) -> Option<H273ColorVui>;

    /// Drop what a broken stream left in flight, keeping the parameter sets:
    /// the next sync point re-enters the same stream.
    fn discard_in_flight_state_after_a_gap(&mut self);

    /// Drop everything, parameter sets included, so the next sync point can
    /// open a stream coded at a different extent.
    fn reset_for_new_parameter_sets(&mut self);
}

/// The video codec seam every hardware encode and decode session is opened
/// through.
pub trait VideoCodecBackend: Send + Sync {
    /// The arm's name, for the one probe log line and for error text.
    fn backend_name(&self) -> &'static str;

    /// Open an encode session. Takes the full-access context because an arm
    /// may allocate device resources for the session.
    fn open_encode_session(
        &self,
        gpu_context: &GpuContextFullAccess,
        request: &VideoEncodeSessionRequest,
    ) -> Result<Box<dyn VideoEncodeSession>>;

    /// Open a decode session. Takes the full-access context for the same
    /// reason as [`Self::open_encode_session`].
    fn open_decode_session(
        &self,
        gpu_context: &GpuContextFullAccess,
        request: &VideoDecodeSessionRequest,
    ) -> Result<Box<dyn VideoDecodeSession>>;
}

/// Shared handle to the backend the chain probed.
pub type SharedVideoCodecBackend = Arc<dyn VideoCodecBackend>;

/// One arm of the video codec chain.
type VideoCodecBackendArm = DeviceBackendArm<SharedVideoCodecBackend>;

static PROBED_VIDEO_CODEC_BACKEND: OnceLock<SharedVideoCodecBackend> = OnceLock::new();

/// Probe the video codec backend chain once per process and log the arm it
/// chose.
///
/// No configuration dial selects an arm and no environment variable overrides
/// the probe. A platform with no codec arm lands on a backend that refuses
/// every open by name, so a codec asked for there fails saying why rather than
/// never producing a frame.
pub fn probe_video_codec_backend() -> SharedVideoCodecBackend {
    Arc::clone(PROBED_VIDEO_CODEC_BACKEND.get_or_init(|| {
        let backend = first_video_codec_backend_arm_that_opens();
        tracing::info!(
            video_codec_backend = backend.backend_name(),
            "video codec backend chain probed"
        );
        backend
    }))
}

/// Take the first arm that opens, logging each demotion with the reason that
/// caused it, or the refusing backend once every arm has declined.
fn first_video_codec_backend_arm_that_opens() -> SharedVideoCodecBackend {
    first_device_backend_arm_that_opens_among(
        platform_video_codec_backend_arms(),
        |backend_name, reason| {
            tracing::info!(
                video_codec_backend = backend_name,
                %reason,
                "video codec backend chain: demoting to the next arm"
            );
        },
    )
    .unwrap_or_else(|| Arc::new(RefusingNullVideoCodecBackend))
}

/// The chain's real arms: Vulkan Video, else — once it has declined — the
/// refusing backend the walk falls through to.
#[cfg(target_os = "linux")]
fn platform_video_codec_backend_arms() -> Vec<VideoCodecBackendArm> {
    use crate::vulkan::video::vulkan_video_codec_backend::VulkanVideoCodecBackend;

    vec![VideoCodecBackendArm::named("vulkan-video", || {
        Ok(Arc::new(VulkanVideoCodecBackend) as SharedVideoCodecBackend)
    })]
}

/// No codec arm serves this platform; the walk falls through to the refusing
/// backend.
#[cfg(not(target_os = "linux"))]
fn platform_video_codec_backend_arms() -> Vec<VideoCodecBackendArm> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_video_codec_chain_is_probed_once_and_hands_back_the_same_backend_every_time() {
        let first = probe_video_codec_backend();
        let second = probe_video_codec_backend();
        assert!(
            Arc::ptr_eq(&first, &second),
            "the chain is probed once per process, so every caller shares one backend"
        );
    }

    #[test]
    fn the_video_codec_chain_always_lands_on_an_arm_whether_or_not_the_platform_codes() {
        let backend = probe_video_codec_backend();
        assert!(
            ["vulkan-video", "refusing-null"].contains(&backend.backend_name()),
            "the chain resolved to an arm nothing declares: {}",
            backend.backend_name()
        );
    }

    /// Read off the list the probe itself walks rather than restated beside
    /// it, so an arm inserted in the wrong place fails here.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_video_codec_chain_offers_vulkan_video_before_falling_through_to_the_refusing_backend()
     {
        let arm_names: Vec<&str> = platform_video_codec_backend_arms()
            .iter()
            .map(|arm| arm.backend_name)
            .collect();
        assert_eq!(arm_names, ["vulkan-video"]);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_platform_with_no_codec_arm_falls_straight_through_to_the_refusing_backend() {
        assert!(platform_video_codec_backend_arms().is_empty());
        assert_eq!(probe_video_codec_backend().backend_name(), "refusing-null");
    }
}
