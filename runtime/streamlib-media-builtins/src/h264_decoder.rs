// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in H.264 decoder: media frames in, published video surfaces out.
//!
//! The state machine is [`EncodedFrameToPublishedSurfaceDecoder`], shared
//! with the H.265 decoder. What lives here is the port surface, the
//! registration name, and the codec identity.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess, VideoCodecElementaryStream,
};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ReactiveProcessor;

use crate::encoded_frame_to_published_surface_decoder::EncodedFrameToPublishedSurfaceDecoder;
use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;

/// What makes the shared decode body this built-in.
pub struct H264DecoderCodecIdentity;

impl HardwareVideoCodecProcessorIdentity for H264DecoderCodecIdentity {
    const VIDEO_CODEC_ELEMENTARY_STREAM: VideoCodecElementaryStream =
        VideoCodecElementaryStream::H264;
    const PROCESSOR_NAME: &'static str = "H264Decoder";
}

#[streamlib::sdk::processor(
    built_in_node,
    description = "Decodes H.264 media frames to published video surfaces via hardware \
                  decode — Vulkan Video on Linux, VideoToolbox on macOS.\n\n\
                  `encoded_video` takes media frames in the layout `H264Encoder` publishes and \
                  the moq-dev players read — a QUIC varint timestamp, then one Annex-B access \
                  unit, a keyframe opening a group — from any producer; a message that is not \
                  one is refused by name. The decoder enters the stream at a group start and, \
                  after a read reports a loss, discards to the next one, so it never publishes \
                  a picture built from references it did not see. A keyframe whose parameter \
                  sets differ from the ones before it reconfigures the session. \
                  `video` publishes an ordinary `VideoFrame` on a pooled RGBA pixel-buffer \
                  surface at the conformance-windowed extent — never the coded picture — \
                  carrying the message's timestamp and the `color_info` the SPS VUI signals, with `fps`, \
                  `texture_layout` and the HDR sidecars absent, so an encoder and a \
                  `read(port, into=VideoFrame)` consume it unchanged. A decoded frame is \
                  buffer-backed, so it reaches a Python kernel through a DLPack landing copy, \
                  never by bare surface id.\n\n\
                  The decode session is minted at `setup()`. On a device with no hardware \
                  decoder for the codec — no Vulkan Video decode queue, or no VideoToolbox \
                  hardware decoder — setup refuses by name: the node never reaches Running, \
                  and waiting for every node to run raises rather than the graph running \
                  with an empty channel.",
    execution = reactive,
    scheduling = high,
    config = crate::encoded_frame_to_published_surface_decoder::HardwareVideoDecoderConfig,
    input(
        "encoded_video",
        delivery_profile = "ordered",
        description = "H.264 media frames to decode"
    ),
    output(
        "video",
        description = "Decoded video frames",
        catalog = BinaryEntry { mode: Snapshot, mime: "application/msgpack" }
    ),
)]
pub struct H264Decoder {
    decode_body: EncodedFrameToPublishedSurfaceDecoder<H264DecoderCodecIdentity>,
}

impl ReactiveProcessor for H264Decoder::Processor {
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
        // One allocation for the whole tick, drained after every message so the
        // pooled pixel buffers are released as soon as their frame is written
        // rather than held for the batch.
        let mut staged = Vec::new();
        while let Some(message) = self.inputs.read_raw("encoded_video")? {
            let decode_outcome = self
                .decode_body
                .decode_one_arriving_message(&message, &mut staged);
            for decoded_frame in staged.drain(..) {
                // The pooled pixel buffer rides inside `decoded_frame` and is
                // released when it drops at the end of this iteration — after
                // the write, so the pool cannot rotate the slot out mid-flight.
                self.outputs.write_msgpack_with_timestamp(
                    "video",
                    &decoded_frame.frame,
                    message.timestamp_ns,
                )?;
            }
            decode_outcome?;
        }
        Ok(())
    }
}
