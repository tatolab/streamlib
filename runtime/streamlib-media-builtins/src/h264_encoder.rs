// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in H.264 encoder: published video surfaces in, media frames out.
//!
//! The state machine is [`PublishedSurfaceToEncodedFrameEncoder`], shared
//! with the H.265 encoder because the two differ in nothing but which
//! elementary stream they mint a session for. What lives here is the port
//! surface, the registration name, and the codec identity.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess, VideoCodecElementaryStream,
};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ReactiveProcessor;

use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;
use crate::published_surface_to_encoded_frame_encoder::PublishedSurfaceToEncodedFrameEncoder;

/// What makes the shared encode body this built-in.
pub struct H264EncoderCodecIdentity;

impl HardwareVideoCodecProcessorIdentity for H264EncoderCodecIdentity {
    const VIDEO_CODEC_ELEMENTARY_STREAM: VideoCodecElementaryStream =
        VideoCodecElementaryStream::H264;
    const PROCESSOR_NAME: &'static str = "H264Encoder";
}

#[streamlib::sdk::processor(
    built_in_node,
    description = "Encodes published video surfaces to H.264 via hardware encode — Vulkan Video \
                  on Linux, VideoToolbox on macOS.\n\n\
                  `video` takes any published `VideoFrame` — buffer-backed (camera, test \
                  pattern) or texture-backed (a kernel output). `encoded_video` publishes one \
                  media frame per access unit, the layout the moq-dev players read: a QUIC \
                  varint timestamp in microseconds, then the Annex-B access unit, parameter sets \
                  inline before each keyframe. A keyframe opens a group. After its first frame \
                  the port's catalog entry is a `VideoEntry` naming the codec string the \
                  stream's SPS spells and the coded extent, with no `description`. A frame the \
                  encoder cannot consume is logged and dropped while the node keeps running; a \
                  frame its producer recycled before the encoder claimed it is counted as a \
                  drop on the `video` link, never an error; a \
                  write the channel ceiling refuses makes the next frame a keyframe. The \
                  session mints from the first frame's dimensions and re-mints when the \
                  upstream extent changes.\n\n\
                  On a device without a hardware encoder for the codec the session fails to \
                  mint: the failure latches and every later frame is discarded with one error \
                  line; no exception reaches Python.",
    execution = reactive,
    scheduling = high,
    config = crate::published_surface_to_encoded_frame_encoder::HardwareVideoEncoderConfig,
    input("video", delivery_profile = "ordered", description = "Video frames to encode"),
    output("encoded_video", description = "H.264 media frames, one access unit each"),
)]
pub struct H264Encoder {
    encode_body: PublishedSurfaceToEncodedFrameEncoder<H264EncoderCodecIdentity>,
}

impl ReactiveProcessor for H264Encoder::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.encode_body.setup(ctx, &self.config)
    }

    fn teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.encode_body.teardown(ctx)
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.encode_body
            .encode_the_next_published_surface_on_input_port(
                &self.config,
                &self.inputs,
                "video",
                &self.outputs,
                "encoded_video",
            )
    }
}
