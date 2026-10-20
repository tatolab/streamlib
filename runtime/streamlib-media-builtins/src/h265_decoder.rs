// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in H.265 decoder: media frames in, published video surfaces out.
//!
//! The state machine is [`EncodedFrameToPublishedSurfaceDecoder`], shared
//! with the H.264 decoder. The one thing H.265 brings that H.264 does not is
//! the CTU pad — a 1920x1080 stream is coded at 1920x1088 — and that is
//! handled where the SPS is parsed, so the frames arriving here are already
//! the picture the stream meant to carry.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess, VideoCodecElementaryStream,
};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ReactiveProcessor;

use crate::encoded_frame_to_published_surface_decoder::EncodedFrameToPublishedSurfaceDecoder;
use crate::hardware_video_codec_processor_identity::HardwareVideoCodecProcessorIdentity;

/// What makes the shared decode body this built-in.
pub struct H265DecoderCodecIdentity;

impl HardwareVideoCodecProcessorIdentity for H265DecoderCodecIdentity {
    const VIDEO_CODEC_ELEMENTARY_STREAM: VideoCodecElementaryStream =
        VideoCodecElementaryStream::H265;
    const PROCESSOR_NAME: &'static str = "H265Decoder";
}

#[streamlib::sdk::processor(
    built_in_node,
    description = "Decodes H.265 media frames to published video surfaces via hardware \
                  decode — Vulkan Video on Linux, VideoToolbox on macOS.\n\n\
                  `encoded_video` takes media frames in the layout `H265Encoder` publishes and \
                  the moq-dev players read — a QUIC varint timestamp, then one Annex-B access \
                  unit, a keyframe opening a group — from any producer; a message that is not \
                  one is refused by name. The decoder enters the stream at a group start and, \
                  after a read reports a loss, discards to the next one, so it never publishes \
                  a picture built from references it did not see. A keyframe whose parameter \
                  sets differ from the ones before it reconfigures the session. \
                  `video` publishes an ordinary `VideoFrame` on a pooled RGBA pixel-buffer \
                  surface at the conformance-windowed extent — never the coded picture — \
                  carrying the message's timestamp and the `color_info` the picture was converted \
                  with: the SPS VUI's, its matrix and range filled when the stream leaves \
                  them out (BT.709 for HD, BT.601 for SD, limited range); a matrix no \
                  converter represents is refused. `fps`, `texture_layout` and the HDR \
                  sidecars are absent, so an encoder and a \
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
        description = "H.265 media frames to decode"
    ),
    output(
        "video",
        description = "Decoded video frames",
        catalog = BinaryEntry { mode: Snapshot, mime: "application/msgpack" }
    ),
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
