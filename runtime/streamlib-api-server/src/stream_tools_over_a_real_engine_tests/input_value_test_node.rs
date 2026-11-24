// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node type the tests of `set_node_input_value` set values on: typed inputs beside a
//! setting it holds in its config.

use super::*;

/// The one setting [`InputValueTestNode`] holds in its config.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    streamlib::sdk::schemars::JsonSchema,
)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub(super) struct InputValueTestNodeConfig {
    #[serde(default)]
    pub frame_width: Option<u32>,
}

/// A native node taking a config, a typed `gain` input, an Image `video` input that takes only
/// a link, a typed `level` output a `gain` can be linked from, and an Integer `count` output a
/// `gain` reads for free.
#[streamlib::sdk::node(
    execution = manual,
    input("gain", delivery_profile = "newest", pin_type = Float),
    input("video", delivery_profile = "newest", pin_type = Texture),
    output("level", pin_type = Float),
    output("count", pin_type = Integer),
)]
pub(super) struct InputValueTestNode;

impl ManualNode for InputValueTestNode::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// Register [`InputValueTestNode`] with the node registry, once per test binary.
pub(super) fn register_the_input_value_test_node_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        NODE_REGISTRY
            .register_with_a_config_schema_for_a_test::<InputValueTestNode::Node, InputValueTestNodeConfig>()
    });
}
