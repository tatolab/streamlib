// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in H.265 encoder: published video surfaces in, encoded-frame bags
//! out.
//!
//! The state machine is [`PublishedSurfaceToEncodedFrameEncoder`], shared
//! with the H.264 encoder because the two differ in nothing but which
//! elementary stream they mint a session for. What lives here is the port
//! surface, the registration name, and the codec identity.

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::engine::video::Codec;
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ReactiveProcessor;

use crate::encoded_video_frame::EncodedVideoCodec;
use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;
use crate::published_surface_to_encoded_frame_encoder::PublishedSurfaceToEncodedFrameEncoder;
use crate::video_frame::VideoFrame;

/// What makes the shared encode body this built-in.
pub struct H265EncoderCodecIdentity;

impl HardwareVideoCodecProcessorIdentity for H265EncoderCodecIdentity {
    const ENCODED_VIDEO_CODEC: EncodedVideoCodec = EncodedVideoCodec::H265;
    const VIDEO_SESSION_CODEC: Codec = Codec::H265;
    const PROCESSOR_NAME: &'static str = "H265Encoder";
}

#[streamlib::sdk::processor(
    description = "Encodes published video surfaces to H.265 Annex-B encoded-frame bags via Vulkan Video hardware encode",
    execution = reactive,
    scheduling = high,
    config = crate::published_surface_to_encoded_frame_encoder::HardwareVideoEncoderConfig,
    input("video", delivery_profile = "ordered", description = "Video frames to encode"),
    output("encoded_video", description = "H.265 encoded-frame bags"),
)]
pub struct H265Encoder {
    encode_body: PublishedSurfaceToEncodedFrameEncoder<H265EncoderCodecIdentity>,
}

impl ReactiveProcessor for H265Encoder::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.encode_body.setup(ctx)
    }

    fn teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.encode_body.teardown(ctx)
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video")?;

        // Every bag the encode staged is written even when the call errored,
        // so a failure part way through a frame's packets never discards the
        // ones already produced.
        let mut staged = Vec::new();
        let encode_outcome =
            self.encode_body
                .encode_one_published_surface(&self.config, &frame, &mut staged);
        for encoded_frame in staged {
            self.outputs.write_with_timestamp(
                "encoded_video",
                &encoded_frame.frame,
                encoded_frame.timestamp_ns,
            )?;
        }
        encode_outcome
    }
}
