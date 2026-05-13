// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::VideoFrame;
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::Result;

#[streamlib::sdk::processor("SimplePassthrough")]
pub struct SimplePassthroughProcessor;

impl streamlib::sdk::processors::ManualProcessor for SimplePassthroughProcessor::Processor {
    // Uses default setup() and teardown() implementations from Processor trait

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Read from iceoryx2 input mailbox and write to output
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
