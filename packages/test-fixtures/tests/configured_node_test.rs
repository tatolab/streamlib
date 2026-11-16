// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::nodes::GeneratedNode;
use streamlib_test_fixtures::test_configured_node::ConfiguredNode;
use streamlib_test_fixtures::test_fixture_node_configs::TestConfiguredNodeConfig;

#[test]
fn test_config_field_access() {
    let config = TestConfiguredNodeConfig { threshold: 0.5 };
    let node = ConfiguredNode::Node::from_config(config).unwrap();
    assert_eq!(node.config.threshold, 0.5);
}

#[test]
fn test_config_update() {
    let config = TestConfiguredNodeConfig { threshold: 0.5 };
    let mut node = ConfiguredNode::Node::from_config(config).unwrap();

    let new_config = TestConfiguredNodeConfig { threshold: 0.8 };
    node.update_config(new_config).unwrap();

    assert_eq!(node.config.threshold, 0.8);
}
