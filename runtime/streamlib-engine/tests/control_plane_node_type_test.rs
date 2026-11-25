// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serial_test::serial;
use streamlib::sdk::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration, PinType,
    PortDescriptor,
};
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

/// An empty stream loaded into `runner`, its project in `project_directory`.
fn an_empty_stream_loaded_into(
    runner: &Runner,
    project_directory: &std::path::Path,
) -> std::sync::Arc<streamlib::sdk::runtime::LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads")
}

/// The keys the identity grammar used to put on this wire. None may appear in
/// a node's rendering at any depth.
const RETIRED_IDENTITY_KEYS: [&str; 3] = ["org", "package", "version"];

fn register_test_type(short: &str) -> NodeClassImportPath {
    let import_path = NodeClassImportPath::new(format!("my_app.nodes:{short}")).unwrap();
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short).unwrap(),
        import_path.clone(),
        "control-plane rendering test",
    )
    .with_input(PortDescriptor::new(
        "_unused_in",
        "",
        false,
        PinDeclaration::exactly(PinType::Texture {
            pixel_formats: Vec::new(),
        }),
    ))
    .with_output(PortDescriptor::new(
        "_unused_out",
        "",
        false,
        PinDeclaration::exactly(PinType::Texture {
            pixel_formats: Vec::new(),
        }),
    ));
    let _ = NODE_REGISTRY.register_descriptor_only(descriptor);
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
fn a_graph_node_renders_its_class_as_a_string_and_carries_no_structured_identity() {
    let camera = register_test_type("ControlPlaneCamera");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();

    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());
    stream
        .add_node(NodeSpec::new(camera.clone(), serde_json::json!({})))
        .unwrap();

    let graph = stream.to_json().expect("graph json");
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

#[test]
#[serial]
fn two_classes_render_two_distinct_type_strings() {
    let source = register_test_type("ControlPlaneSource");
    let sink = register_test_type("ControlPlaneSink");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();

    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());
    stream
        .add_node(NodeSpec::new(source.clone(), serde_json::json!({})))
        .unwrap();
    stream
        .add_node(NodeSpec::new(sink.clone(), serde_json::json!({})))
        .unwrap();

    let graph = stream.to_json().expect("graph json");
    let rendered: Vec<&str> = graph["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|node| node["type"].as_str().expect("`type` renders as a string"))
        .collect();

    assert_eq!(rendered, vec![source.as_str(), sink.as_str()]);
}
