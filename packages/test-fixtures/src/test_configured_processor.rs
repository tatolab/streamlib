// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Attribute-macro test fixture verifying that
//! `#[streamlib::sdk::processor("...")]` emits the right config-binding
//! code against a `streamlib.yaml` `package:` block.

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ContinuousProcessor;

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/TestConfiguredProcessor",
    execution = continuous,
    config = crate::_generated_::TestConfiguredProcessorConfig,
)]
pub struct ConfiguredProcessor;

impl ContinuousProcessor for ConfiguredProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let _threshold = self.config.threshold;
        Ok(())
    }
}
