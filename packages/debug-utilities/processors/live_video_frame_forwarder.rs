// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Live VideoFrame Forwarder Processor
//
// Reactive inline pass-through: on every input VideoFrame it forwards that
// same frame to its output port unchanged, so a graph spliced through it on a
// running runtime KEEPS delivering live frames downstream (camera → forwarder
// → display never freezes). Contrast SimplePassthrough, whose `manual`
// one-shot `start()` pumps a single frame and then goes quiet.
//
// Cdylib-safe by construction: a VideoFrame carries a `surface_id` reference,
// not the GPU texture, so forwarding it is a pure struct copy that never
// touches a device or any engine-only primitive.

use crate::_generated_::VideoFrame;
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib_plugin_sdk::sdk::error::Result;

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/debug-utilities/LiveVideoFrameForwarder",
    description = "Forwards each input VideoFrame to the output port unchanged on every frame, keeping a live-spliced graph delivering frames",
    execution = reactive,
    config = crate::_generated_::LiveVideoFrameForwarderConfig,
    input("input", "@tatolab/core/VideoFrame", delivery_profile = "every_sample", description = "Video frame input"),
    output("output", "@tatolab/core/VideoFrame", description = "Video frame output (unchanged)"),
)]
pub struct LiveVideoFrameForwarderProcessor;

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor
    for LiveVideoFrameForwarderProcessor::Processor
{
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("input") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("input")?;
        self.outputs.write("output", &frame)?;
        Ok(())
    }

    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}
