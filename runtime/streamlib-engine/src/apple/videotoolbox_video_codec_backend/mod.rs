// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video codec seam's VideoToolbox arm: hardware H.264 and H.265 encode
//! and decode on Apple Silicon's media engine.
//!
//! Frames stay on the GPU and on IOSurfaces both ways, with no host copy. An
//! encode converts the published frame into an NV12 surface from the
//! session's own pool; a decode's pictures come back on IOSurfaces and land in
//! the pool through the camera's own conversion. The wire either side is
//! Annex-B, converted at this arm's edge.

mod videotoolbox_decode_session;
mod videotoolbox_encode_session;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_media::{CMVideoCodecType, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC};
use objc2_video_toolbox::VTIsHardwareDecodeSupported;

use crate::core::context::{
    GpuContextFullAccess, VideoCodecBackend, VideoCodecElementaryStream, VideoDecodeSession,
    VideoDecodeSessionRequest, VideoEncodeKnobs, VideoEncodeSession, VideoEncodeSessionRequest,
};
use crate::core::{Error, Result};

use videotoolbox_decode_session::VideoToolboxDecodeSession;
use videotoolbox_encode_session::VideoToolboxEncodeSession;

/// VideoToolbox hardware encode and decode.
pub(crate) struct VideoToolboxVideoCodecBackend;

impl VideoCodecBackend for VideoToolboxVideoCodecBackend {
    fn backend_name(&self) -> &'static str {
        "videotoolbox"
    }

    fn refuse_encode_knobs_this_arm_does_not_honour(
        &self,
        elementary_stream: VideoCodecElementaryStream,
        knobs: &VideoEncodeKnobs,
    ) -> Result<()> {
        if let Some(effort_level) = knobs.effort_level {
            return Err(Error::Configuration(format!(
                "{elementary_stream:?} encode on VideoToolbox refuses effort_level = \
                 {effort_level}: VideoToolbox exposes no encoder-effort index to map it onto. \
                 Leave effort_level unset on macOS."
            )));
        }
        if knobs.keyframe_interval_seconds == 0 {
            return Err(Error::Configuration(format!(
                "{elementary_stream:?} encode on VideoToolbox refuses keyframe_interval_seconds \
                 = 0: the streaming shape puts a sync point on a cadence, so the interval must \
                 be at least one second."
            )));
        }
        Ok(())
    }

    fn open_encode_session(
        &self,
        gpu_context: &GpuContextFullAccess,
        request: &VideoEncodeSessionRequest,
    ) -> Result<Box<dyn VideoEncodeSession>> {
        self.refuse_encode_knobs_this_arm_does_not_honour(
            request.elementary_stream,
            &request.knobs,
        )?;
        Ok(Box::new(VideoToolboxEncodeSession::open(
            gpu_context,
            request,
        )?))
    }

    fn open_decode_session(
        &self,
        gpu_context: &GpuContextFullAccess,
        request: &VideoDecodeSessionRequest,
    ) -> Result<Box<dyn VideoDecodeSession>> {
        // The VideoToolbox session itself opens at the stream's first
        // parameter sets; a machine with no hardware decoder is refused here,
        // at the block's `setup()`, rather than at the first sync point.
        // SAFETY: a CoreMedia codec type.
        if !unsafe {
            VTIsHardwareDecodeSupported(core_media_codec_type_of(request.elementary_stream))
        } {
            return Err(Error::GpuError(format!(
                "VideoToolbox has no hardware {:?} decoder on this machine",
                request.elementary_stream
            )));
        }
        Ok(Box::new(VideoToolboxDecodeSession::open(
            gpu_context.host_inner().limited_access(),
            request,
        )))
    }
}

/// The CoreMedia codec type an elementary stream is.
fn core_media_codec_type_of(elementary_stream: VideoCodecElementaryStream) -> CMVideoCodecType {
    match elementary_stream {
        VideoCodecElementaryStream::H264 => kCMVideoCodecType_H264,
        VideoCodecElementaryStream::H265 => kCMVideoCodecType_HEVC,
    }
}

/// A VideoToolbox call's failure, naming what was called and the `OSStatus`
/// it answered.
fn videotoolbox_call_failure(call: &str, os_status: i32) -> Error {
    Error::GpuError(format!("{call} failed with OSStatus {os_status}"))
}

/// A dictionary of `CFString` keys to boolean values, the shape the
/// encoder and decoder specifications take.
fn cf_dictionary_of_booleans(
    entries: &[(&CFString, bool)],
) -> CFRetained<CFDictionary<CFString, CFType>> {
    let keys: Vec<&CFString> = entries.iter().map(|(key, _)| *key).collect();
    let values: Vec<&CFType> = entries
        .iter()
        .map(|(_, value)| -> &CFType { CFBoolean::new(*value) })
        .collect();
    CFDictionary::from_slices(&keys, &values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn knobs(effort_level: Option<u32>, keyframe_interval_seconds: u32) -> VideoEncodeKnobs {
        VideoEncodeKnobs {
            bitrate_bps: Some(4_000_000),
            keyframe_interval_seconds,
            effort_level,
        }
    }

    #[test]
    fn an_effort_level_is_refused_naming_the_knob() {
        let refusal = VideoToolboxVideoCodecBackend
            .refuse_encode_knobs_this_arm_does_not_honour(
                VideoCodecElementaryStream::H264,
                &knobs(Some(2), 1),
            )
            .expect_err("VideoToolbox has no effort index")
            .to_string();
        assert!(refusal.contains("effort_level = 2"), "{refusal}");
    }

    #[test]
    fn a_zero_keyframe_interval_is_refused_naming_the_knob() {
        let refusal = VideoToolboxVideoCodecBackend
            .refuse_encode_knobs_this_arm_does_not_honour(
                VideoCodecElementaryStream::H265,
                &knobs(None, 0),
            )
            .expect_err("the streaming shape needs a cadence")
            .to_string();
        assert!(
            refusal.contains("keyframe_interval_seconds = 0"),
            "{refusal}"
        );
    }

    #[test]
    fn a_bitrate_and_a_keyframe_interval_are_honoured() {
        for elementary_stream in [
            VideoCodecElementaryStream::H264,
            VideoCodecElementaryStream::H265,
        ] {
            VideoToolboxVideoCodecBackend
                .refuse_encode_knobs_this_arm_does_not_honour(elementary_stream, &knobs(None, 2))
                .expect("both knobs map onto VideoToolbox properties");
        }
    }

    #[test]
    fn each_elementary_stream_opens_the_core_media_codec_of_the_same_name() {
        assert_eq!(
            core_media_codec_type_of(VideoCodecElementaryStream::H264),
            u32::from_be_bytes(*b"avc1")
        );
        assert_eq!(
            core_media_codec_type_of(VideoCodecElementaryStream::H265),
            u32::from_be_bytes(*b"hvc1")
        );
    }
}
