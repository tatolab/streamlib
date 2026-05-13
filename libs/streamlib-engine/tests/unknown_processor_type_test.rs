// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tests for `Error::UnknownProcessorType` — the typed error surfaced when
//! a caller asks `add_processor` for a structurally-valid `SchemaIdent`
//! whose type isn't registered.
//!
//! Two behaviors locked here:
//! 1. The error variant is `UnknownProcessorType` (not the old generic
//!    `GraphError("Could not create node")`), and carries the offending
//!    ident verbatim.
//! 2. The failed node is left in the graph in `ProcessorState::Error`, so
//!    API consumers (`GET /api/graph`) can see what failed and why. This
//!    is the runtime-dynamic-system shape: the runtime tells you something
//!    didn't resolve, and leaves a placeholder for observability.

use serial_test::serial;
use streamlib::sdk::error::Error;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
// Force-link a processor crate so `Runner::new()` finds at least one
// entry in PROCESSOR_REGISTRY.
#[allow(unused_imports)]
use streamlib_test_fixtures::SimplePassthroughProcessor;

fn unknown_ident() -> streamlib::sdk::descriptors::SchemaIdent {
    streamlib::sdk::schema_ident!(
        "tatolab",
        "ghost-package",
        "DefinitelyNotARegisteredProcessor",
        "9.9.9"
    )
}

#[test]
#[serial]
fn add_processor_with_unknown_type_returns_typed_error() {
    let runtime = Runner::new().unwrap();
    let ident = unknown_ident();

    let result = runtime.add_processor(ProcessorSpec::new(ident.clone(), serde_json::json!({})));

    match result {
        Err(Error::UnknownProcessorType { ident: returned }) => {
            assert_eq!(returned, ident);
        }
        other => panic!(
            "expected Err(UnknownProcessorType), got {:?}",
            other
        ),
    }
}

#[test]
#[serial]
fn unknown_processor_type_leaves_failed_node_in_graph_with_error_state() {
    let runtime = Runner::new().unwrap();
    let ident = unknown_ident();

    // Add — expect typed error, but the node IS added as a side effect for
    // observability. Mentally revert the `add_v_op.rs` change (return empty
    // traversal on miss) and this test fails — the graph stays empty.
    let _ = runtime
        .add_processor(ProcessorSpec::new(ident.clone(), serde_json::json!({})))
        .err()
        .expect("registry miss should error");

    // Inspect the graph via the public `to_json` API — the failed node must
    // be visible with components.state == "Error".
    let graph_json = runtime.to_json().expect("to_json should succeed");
    let nodes = graph_json
        .get("nodes")
        .and_then(|v| v.as_array())
        .expect("graph JSON should carry a nodes array");

    let failed_node = nodes
        .iter()
        .find(|node| {
            node.get("type")
                .and_then(|t| t.get("type"))
                .and_then(|s| s.as_str())
                == Some("DefinitelyNotARegisteredProcessor")
        })
        .expect("failed node should be present in the graph for observability");

    let state = failed_node
        .get("components")
        .and_then(|c| c.get("state"))
        .and_then(|s| s.as_str())
        .expect("failed node should carry a components.state field");
    assert_eq!(
        state, "Error",
        "failed node should be in Error state, was {}",
        state
    );
}

#[test]
#[serial]
fn graph_file_validate_rejects_unknown_processor_type() {
    use streamlib_engine::core::graph_file::GraphFileDefinition;

    let json = r#"{
        "processors": [
            {
                "alias": "ghost",
                "type": {
                    "org": "tatolab",
                    "package": "ghost-package",
                    "type": "DefinitelyNotARegisteredProcessor",
                    "version": "1.0.0"
                },
                "config": {}
            }
        ]
    }"#;

    let def = GraphFileDefinition::from_json_str(json).unwrap();
    match def.validate() {
        Err(Error::UnknownProcessorType { ident }) => {
            assert_eq!(ident.r#type.as_str(), "DefinitelyNotARegisteredProcessor");
            assert_eq!(ident.package.as_str(), "ghost-package");
        }
        other => panic!(
            "expected Err(UnknownProcessorType), got {:?}",
            other
        ),
    }
}
