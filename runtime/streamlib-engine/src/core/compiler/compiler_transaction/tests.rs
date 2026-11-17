// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::compiler::Compiler;
use crate::core::nodes::NodeSpec;
use crate::core::test_support::{MockSourceTakingAFrameSize, ensure_test_mocks_registered};

fn an_update_of(node: &NodeRunId, config: serde_json::Value) -> PendingOperation {
    PendingOperation::UpdateNodeConfig {
        node_run_id: node.clone(),
        config_to_apply: config,
    }
}

/// A compiler whose graph holds one node holding `config`, and that node.
fn a_compiler_holding_a_node_configured(config: serde_json::Value) -> (Compiler, NodeRunId) {
    ensure_test_mocks_registered();
    let compiler = Compiler::new();
    let node = compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(NodeSpec::new(
                MockSourceTakingAFrameSize::node_class_import_path(),
                config,
            ))
            .expect("the node is named")
            .first()
            .expect("the mock is in the registry")
            .id
            .clone()
    });
    (compiler, node)
}

/// Log the config `node` will hold with `field` set to `value`, built in the step that logs it.
fn set_one_field(compiler: &Compiler, node: &NodeRunId, field: &str, value: serde_json::Value) {
    compiler.scope(|graph, tx| {
        let mut config = tx
            .the_config_the_node_will_hold(graph, node)
            .expect("the node holds a config");
        config[field] = value;
        tx.log(an_update_of(node, config));
    });
}

/// The config the last update logged for `node` hands it.
fn the_config_last_logged_for(compiler: &Compiler, node: &NodeRunId) -> serde_json::Value {
    compiler
        .logged_pending_operations()
        .into_iter()
        .rev()
        .find_map(|operation| match operation {
            PendingOperation::UpdateNodeConfig {
                node_run_id,
                config_to_apply,
            } if &node_run_id == node => Some(config_to_apply),
            _ => None,
        })
        .expect("an update is logged for the node")
}

#[test]
fn a_node_will_hold_the_config_its_last_logged_update_hands_it_else_the_one_it_holds() {
    let (compiler, node) =
        a_compiler_holding_a_node_configured(serde_json::json!({"frame_width": 640}));
    let held = |compiler: &Compiler| {
        compiler.scope(|graph, tx| tx.the_config_the_node_will_hold(graph, &node))
    };
    assert_eq!(
        held(&compiler),
        Some(serde_json::json!({"frame_width": 640}))
    );
    set_one_field(&compiler, &node, "frame_height", serde_json::json!(480));
    set_one_field(&compiler, &node, "frame_width", serde_json::json!(1280));
    assert_eq!(
        held(&compiler),
        Some(serde_json::json!({"frame_width": 1280, "frame_height": 480}))
    );
    let taken =
        compiler.scope(|_graph, tx| tx.take_each_config_update_of(&HashSet::from([node.clone()])));
    assert_eq!(taken.len(), 2);
    assert_eq!(
        held(&compiler),
        Some(serde_json::json!({"frame_width": 640}))
    );
}

/// A field set while a commit has taken the update of another field and not yet applied it
/// builds on that update: the node holds both once each applies.
#[test]
fn a_field_set_while_a_commit_applies_another_builds_on_it() {
    let (compiler, node) = a_compiler_holding_a_node_configured(
        serde_json::json!({"frame_width": 640, "frame_height": 480}),
    );
    set_one_field(&compiler, &node, "frame_width", serde_json::json!(1280));
    let taken_by_the_commit = compiler.take_the_logged_operations_to_compile();
    assert_eq!(taken_by_the_commit.len(), 1);

    set_one_field(&compiler, &node, "frame_height", serde_json::json!(720));

    assert_eq!(
        the_config_last_logged_for(&compiler, &node),
        serde_json::json!({"frame_width": 1280, "frame_height": 720})
    );
}
