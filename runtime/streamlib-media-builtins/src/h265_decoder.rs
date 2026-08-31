// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in H.265 decoder: encoded-frame bags in, published video surfaces
//! out.
//!
//! The state machine is [`EncodedFrameToPublishedSurfaceDecoder`], shared
//! with the H.264 decoder. The one thing H.265 brings that H.264 does not is
//! the CTU pad — a 1920x1080 stream is coded at 1920x1088 — and that is
//! handled where the SPS is parsed, so the frames arriving here are already
//! the picture the stream meant to carry.

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::engine::video::Codec;
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ReactiveProcessor;

use crate::encoded_frame_to_published_surface_decoder::EncodedFrameToPublishedSurfaceDecoder;
use crate::encoded_video_frame::EncodedVideoCodec;
use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;

/// What makes the shared decode body this built-in.
pub struct H265DecoderCodecIdentity;

impl HardwareVideoCodecProcessorIdentity for H265DecoderCodecIdentity {
    const ENCODED_VIDEO_CODEC: EncodedVideoCodec = EncodedVideoCodec::H265;
    const VIDEO_SESSION_CODEC: Codec = Codec::H265;
    const PROCESSOR_NAME: &'static str = "H265Decoder";
}

#[streamlib::sdk::processor(
    description = "Decodes H.265 Annex-B encoded-frame bags to published video surfaces via Vulkan Video hardware decode",
    execution = reactive,
    scheduling = high,
    config = crate::encoded_frame_to_published_surface_decoder::HardwareVideoDecoderConfig,
    input(
        "encoded_video",
        delivery_profile = "ordered",
        description = "H.265 encoded-frame bags to decode"
    ),
    output("video", description = "Decoded video frames"),
)]
pub struct H265Decoder {
    decode_body: EncodedFrameToPublishedSurfaceDecoder<H265DecoderCodecIdentity>,
}

impl ReactiveProcessor for H265Decoder::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.decode_body.setup(ctx, &self.config)
    }

    fn teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.decode_body.teardown(ctx)
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("encoded_video") {
            return Ok(());
        }
        let gpu_context = self.decode_body.gpu_context_for_this_tick()?;

        // One allocation for the whole tick, drained after every bag so the
        // pooled pixel buffers are released as soon as their frame is written
        // rather than held for the batch.
        let mut staged = Vec::new();
        while let Some((bag_bytes, frame_header_timestamp_ns)) =
            self.inputs.read_raw("encoded_video")?
        {
            let decode_outcome = self.decode_body.decode_one_arriving_bag(
                &gpu_context,
                &bag_bytes,
                frame_header_timestamp_ns,
                &mut staged,
            );
            for decoded_frame in staged.drain(..) {
                // The pooled pixel buffer rides inside `decoded_frame` and is
                // released when it drops at the end of this iteration — after
                // the write, so the pool cannot rotate the slot out mid-flight.
                self.outputs.write_with_timestamp(
                    "video",
                    &decoded_frame.frame,
                    frame_header_timestamp_ns,
                )?;
            }
            decode_outcome?;
        }
        Ok(())
    }
}
