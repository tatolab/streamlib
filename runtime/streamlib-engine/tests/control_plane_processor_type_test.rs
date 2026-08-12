// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The control plane names a processor's class with a string.
//!
//! Asserted against the rendered payload rather than the DTO's Rust type: the
//! shape a reader gets is the contract, and a `Serialize` impl is what decides
//! it. `to_json_async` is the exact payload `GET /api/graph` and
//! `streamlib graph` serve.
//!
//! The negative half is what makes this a lock rather than a restatement — no
//! `org`, `package` or `version` key survives anywhere in a node's rendering,
//! at any depth. Assert only that `type` is a string and a structured identity
//! could come back beside it.

use serial_test::serial;
use streamlib::sdk::descriptors::{
    PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;

/// The keys the identity grammar used to put on this wire. None may appear in
/// a node's rendering at any depth.
const RETIRED_IDENTITY_KEYS: [&str; 3] = ["org", "package", "version"];

fn register_test_type(short: &str) -> ProcessorClassImportPath {
    let import_path = ProcessorClassImportPath::new(format!("my_app.processors:{short}")).unwrap();
    let descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new(short).unwrap(),
        import_path.clone(),
        "control-plane rendering test",
    )
    .with_input(PortDescriptor::new("_unused_in", "", false))
    .with_output(PortDescriptor::new("_unused_out", "", false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

/// Every key appearing anywhere under `value`, at any depth.
fn every_key(value: &serde_json::Value, into: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, nested) in map {
                into.push(key.clone());
                every_key(nested, into);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                every_key(item, into);
            }
        }
        _ => {}
    }
}

#[test]
#[serial]
fn a_processor_node_renders_its_class_as_a_string_and_carries_no_structured_identity() {
    let camera = register_test_type("ControlPlaneCamera");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera.clone(), serde_json::json!({})))
        .unwrap();

    let graph = runtime.to_json().expect("graph json");
    let node = &graph["nodes"][0];

    assert_eq!(
        node["type"],
        serde_json::Value::String(camera.as_str().to_string()),
        "`type` must be the class import path as a plain string, got {}",
        node["type"]
    );

    let mut keys = Vec::new();
    every_key(node, &mut keys);
    for retired in RETIRED_IDENTITY_KEYS {
        assert!(
            !keys.contains(&retired.to_string()),
            "`{retired}` must not appear anywhere in a node's rendering; \
             identity is one opaque string now. Keys found: {keys:?}"
        );
    }
}

/// Two processors of different classes render two different strings — the
/// field distinguishes them without an object to look inside.
#[test]
#[serial]
fn two_classes_render_two_distinct_type_strings() {
    let source = register_test_type("ControlPlaneSource");
    let sink = register_test_type("ControlPlaneSink");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(source.clone(), serde_json::json!({})))
        .unwrap();
    runtime
        .add_processor(ProcessorSpec::new(sink.clone(), serde_json::json!({})))
        .unwrap();

    let graph = runtime.to_json().expect("graph json");
    let rendered: Vec<&str> = graph["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|node| node["type"].as_str().expect("`type` renders as a string"))
        .collect();

    assert_eq!(rendered, vec![source.as_str(), sink.as_str()]);
}
