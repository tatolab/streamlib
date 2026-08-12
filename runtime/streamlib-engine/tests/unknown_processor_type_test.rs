// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tests for `Error::UnknownProcessorType` — the typed error surfaced when
//! a caller asks `add_processor` for a class import path nothing is
//! registered under.
//!
//! Two behaviors locked here:
//! 1. The error variant is `UnknownProcessorType` (not the old generic
//!    `GraphError("Could not create node")`), and carries the requested path
//!    verbatim — the caller asked for a class by that name, and matching
//!    their spelling is what makes a typo findable.
//! 2. The failed node is left in the graph in `ProcessorState::Error`, so
//!    API consumers (`GET /api/graph`) can see what failed and why. This
//!    is the runtime-dynamic-system shape: the runtime tells you something
//!    didn't resolve, and leaves a placeholder for observability.

use serial_test::serial;
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::error::Error;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;

const UNKNOWN_PATH: &str = "ghost_package:DefinitelyNotARegisteredProcessor";

fn unknown_ident() -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(UNKNOWN_PATH).unwrap()
}

#[test]
#[serial]
fn add_processor_with_unknown_type_returns_typed_error() {
    let runtime = Runner::new().unwrap();
    let ident = unknown_ident();

    let result = runtime.add_processor(ProcessorSpec::new(ident.clone(), serde_json::json!({})));

    match result {
        Err(Error::UnknownProcessorType { ident: returned }) => {
            // Spelled as a literal rather than compared against `ident` — that
            // would echo the input on both sides and pass no matter what the
            // engine returned. The path must come back byte for byte: the
            // engine stores it opaque, so anything else means it parsed it.
            assert_eq!(returned.as_str(), UNKNOWN_PATH);
        }
        other => panic!("expected Err(UnknownProcessorType), got {:?}", other),
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

    // The unresolved node carries the requested path verbatim — nothing
    // resolved it, so there is nothing else it could carry, and the caller can
    // find their own typo in the graph.
    let failed_node = nodes
        .iter()
        .find(|node| node.get("type").and_then(|t| t.as_str()) == Some(UNKNOWN_PATH))
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
fn graph_snapshot_validate_rejects_unknown_processor_type() {
    use streamlib_engine::core::graph_snapshot::GraphSnapshot;

    let json = r#"{
        "processors": [
            {
                "alias": "ghost",
                "type": "ghost_package:DefinitelyNotARegisteredProcessor",
                "config": {}
            }
        ]
    }"#;

    let snapshot = GraphSnapshot::from_json_str(json).unwrap();
    match snapshot.validate() {
        Err(Error::UnknownProcessorType { ident }) => {
            assert_eq!(ident.as_str(), UNKNOWN_PATH);
        }
        other => panic!("expected Err(UnknownProcessorType), got {:?}", other),
    }
}
