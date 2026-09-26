// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video codec seam's floor on a platform no codec arm serves.

use super::GpuContextFullAccess;
use super::video_codec_backend::{
    VideoCodecBackend, VideoCodecElementaryStream, VideoDecodeSession, VideoDecodeSessionRequest,
    VideoEncodeSession, VideoEncodeSessionRequest,
};
use crate::core::{Error, Result};

/// A backend that refuses every session open by name.
///
/// An encoder that swallowed frames, or a decoder that published nothing,
/// would look healthy while doing no work, so the refusal is the answer.
pub(crate) struct RefusingNullVideoCodecBackend;

impl VideoCodecBackend for RefusingNullVideoCodecBackend {
    fn backend_name(&self) -> &'static str {
        "refusing-null"
    }

    fn open_encode_session(
        &self,
        _gpu_context: &GpuContextFullAccess,
        request: &VideoEncodeSessionRequest,
    ) -> Result<Box<dyn VideoEncodeSession>> {
        Err(refusal_for_a_platform_no_codec_arm_serves(
            RefusedVideoCodecSessionDirection::Encode,
            request.elementary_stream,
        ))
    }

    fn open_decode_session(
        &self,
        _gpu_context: &GpuContextFullAccess,
        request: &VideoDecodeSessionRequest,
    ) -> Result<Box<dyn VideoDecodeSession>> {
        Err(refusal_for_a_platform_no_codec_arm_serves(
            RefusedVideoCodecSessionDirection::Decode,
            request.elementary_stream,
        ))
    }
}

/// Which kind of session a refusal turned down.
#[derive(Debug, Clone, Copy)]
enum RefusedVideoCodecSessionDirection {
    Encode,
    Decode,
}

impl std::fmt::Display for RefusedVideoCodecSessionDirection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Encode => "encode",
            Self::Decode => "decode",
        })
    }
}

fn refusal_for_a_platform_no_codec_arm_serves(
    direction: RefusedVideoCodecSessionDirection,
    elementary_stream: VideoCodecElementaryStream,
) -> Error {
    Error::Configuration(format!(
        "No hardware video codec backend serves {}: {elementary_stream:?} {direction} runs on \
         Linux (Vulkan Video).",
        crate::platform::name()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_the_platform_the_direction_and_the_stream() {
        let refusal = refusal_for_a_platform_no_codec_arm_serves(
            RefusedVideoCodecSessionDirection::Decode,
            VideoCodecElementaryStream::H265,
        )
        .to_string();
        assert!(refusal.contains(crate::platform::name()), "{refusal}");
        assert!(refusal.contains("H265 decode"), "{refusal}");
    }
}
