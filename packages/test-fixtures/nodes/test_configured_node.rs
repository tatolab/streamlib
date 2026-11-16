// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::nodes::ContinuousNode;

#[streamlib::sdk::node(
    execution = continuous,
    config = crate::test_fixture_node_configs::TestConfiguredNodeConfig,
)]
#[meta(
    description = "Attribute-macro test fixture verifying config-emit against a streamlib.yaml package block"
)]
pub struct ConfiguredNode;

impl ContinuousNode for ConfiguredNode::Node {
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
