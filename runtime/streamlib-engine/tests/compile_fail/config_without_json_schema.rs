// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::Result;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeConfigWithoutTheDerive {
    pub width: u32,
}

#[streamlib::sdk::processor(
    execution = manual,
    config = crate::ProbeConfigWithoutTheDerive,
)]
pub struct ProbeProcessor;

impl streamlib::sdk::processors::ManualProcessor for ProbeProcessor::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn main() {}
