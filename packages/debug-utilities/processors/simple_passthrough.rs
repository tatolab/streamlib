// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::VideoFrame;
use streamlib_plugin_sdk::sdk::context::RuntimeContextFullAccess;
use streamlib_plugin_sdk::sdk::error::Result;
use streamlib_plugin_sdk::sdk::processors::ManualProcessor;

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/debug-utilities/SimplePassthrough",
    description = "Passes video frames through unchanged (for testing)",
    execution = manual,
    config = crate::_generated_::SimplePassthroughConfig,
    input("input", "@tatolab/core/VideoFrame", description = "Video frame input"),
    output("output", "@tatolab/core/VideoFrame", description = "Video frame output (unchanged)"),
)]
pub struct SimplePassthroughProcessor;

impl ManualProcessor for SimplePassthroughProcessor::Processor {
    // Uses default setup() and teardown() implementations from Processor trait

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.inputs.has_data("input") {
            let frame: VideoFrame = self.inputs.read("input")?;
            self.outputs.write("output", &frame)?;
        }
        Ok(())
    }
}

impl SimplePassthroughProcessor::Processor {
    pub fn scale(&self) -> f32 {
        self.config.scale
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.config.scale = scale;
    }
}
