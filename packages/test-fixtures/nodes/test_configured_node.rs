// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::nodes::ContinuousNode;

/// The settings [`ConfiguredNode`] reads at setup.
#[streamlib::sdk::node_settings]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredNodeSettings {
    #[description = "A number the node holds from setup on."]
    pub threshold: f32,
}

#[streamlib::sdk::node(
    execution = continuous,
    settings = crate::test_configured_node::ConfiguredNodeSettings,
)]
#[meta(
    description = "Attribute-macro test fixture: one setting, taken from the value its input holds before setup."
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
        let _threshold = self.settings.threshold;
        Ok(())
    }
}
