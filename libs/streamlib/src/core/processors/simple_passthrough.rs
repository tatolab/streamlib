// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::Videoframe;
use crate::core::Result;

#[crate::processor("com.tatolab.simple_passthrough")]
pub struct SimplePassthroughProcessor;

impl crate::core::ManualProcessor for SimplePassthroughProcessor::Processor {
    // Uses default setup() and teardown() implementations from Processor trait

    fn start(&mut self) -> Result<()> {
        // Read from iceoryx2 input mailbox and write to output
        if self.inputs.has_data("input") {
            let frame: Videoframe = self.inputs.read("input")?;
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
