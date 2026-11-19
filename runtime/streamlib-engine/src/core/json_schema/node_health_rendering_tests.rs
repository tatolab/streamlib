// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn a_node_named(display_name: &str) -> crate::core::graph::GraphNode {
    crate::core::graph::GraphNode::new(
        streamlib_node_schema::NodeClassImportPath::new("tests:Blur").expect("a legal import path"),
        Some(crate::core::descriptors::NodeKind::Process),
        display_name,
        None,
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn a_node_renders_its_health_and_each_diagnostic_at_its_own_place() {
    let mut node = a_node_named("blur");
    let state = crate::core::graph::StateComponent::new(crate::core::graph::NodeState::Running);
    state
        .clone_inner()
        .note_why_process_is_raising(Some("ZeroDivisionError".to_owned()));
    node.insert(state);

    let rendered = serde_json::to_value(GraphNodeOutput::from(&node)).unwrap();

    assert_eq!(rendered["components"]["health"], "raising", "{rendered}");
    assert_eq!(
        rendered["components"]["diagnostics"],
        serde_json::json!([{
            "kind": "node_raising",
            "place": { "on": "node", "node_id": node.node_uuid.to_string(), "node_name": "blur" },
            "words": "ZeroDivisionError",
            "recovery": "fix_the_code",
        }]),
        "{rendered}"
    );
}

#[test]
fn a_node_keeping_no_state_renders_no_health_and_no_diagnostic() {
    let rendered = serde_json::to_value(GraphNodeOutput::from(&a_node_named("blur"))).unwrap();

    let components = rendered["components"]
        .as_object()
        .expect("components render");
    assert!(!components.contains_key("health"), "{rendered}");
    assert!(!components.contains_key("diagnostics"), "{rendered}");
}
