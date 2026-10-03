// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A node's name is cast to lowercase URL-safe, a defaulted duplicate takes
//! the next free `-2`, and a typed duplicate is refused — and the name the add
//! reports is the one `graph` renders.
//!
//! The unit coverage of the rule lives beside `add_v`; what this locks is the
//! path an author meets: the add reports the assigned name a handle carries,
//! and the graph JSON — the payload `streamlib graph` and `GET /api/graph`
//! serve — carries the assigned names, not the requested ones.

use serial_test::serial;
use streamlib::sdk::descriptors::{
    PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use streamlib::sdk::error::Error;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;

/// Register a descriptor-only processor type — enough for `add_processor`'s
/// port-info lookup, with no instance to construct. Idempotent: a second
/// register under `serial_test` returns an already-registered error we ignore.
fn register_test_type(short: &str) -> ProcessorClassImportPath {
    register_test_type_named(short, short)
}

/// Register under an import path whose trailing segment is `path_tail` while
/// the descriptor's short name is `short_name`.
fn register_test_type_named(path_tail: &str, short_name: &str) -> ProcessorClassImportPath {
    let import_path =
        ProcessorClassImportPath::new(format!("{}::{path_tail}", module_path!())).unwrap();
    let descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new(short_name).unwrap(),
        import_path.clone(),
        "node name test",
    )
    .with_input(PortDescriptor::new("_unused_in", "", false))
    .with_output(PortDescriptor::new("_unused_out", "", false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

/// Every node's name, in node-iteration order, as the graph JSON renders it.
fn node_names_in_the_graph_json(runtime: &Runner) -> Vec<String> {
    runtime.to_json().expect("graph json")["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|node| {
            node["name"]
                .as_str()
                .expect("every node renders a name")
                .to_string()
        })
        .collect()
}

#[test]
#[serial]
fn the_graph_json_carries_distinct_names_for_two_defaulted_nodes_of_one_type() {
    let camera = register_test_type("SuffixedCamera");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera.clone(), serde_json::json!({})))
        .unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera, serde_json::json!({})))
        .unwrap();

    assert_eq!(
        node_names_in_the_graph_json(&runtime),
        vec!["suffixedcamera", "suffixedcamera-2"],
        "`streamlib graph` must name the two instances apart"
    );
}

#[test]
#[serial]
fn the_read_back_name_is_the_cast_of_the_typed_one() {
    let camera = register_test_type("ReadBackCamera");

    let runtime = Runner::new().unwrap();
    let (_id, name) = runtime
        .add_processor_reporting_assigned_display_name(
            ProcessorSpec::new(camera, serde_json::json!({})).with_display_name("Front Camera"),
        )
        .unwrap();

    assert_eq!(name, "front-camera");
    assert_eq!(node_names_in_the_graph_json(&runtime), vec!["front-camera"]);
}

#[test]
#[serial]
fn a_typed_duplicate_is_refused_by_name_and_adds_nothing() {
    let camera = register_test_type("TypedTwiceCamera");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(
            ProcessorSpec::new(camera.clone(), serde_json::json!({})).with_display_name("FrontCam"),
        )
        .unwrap();
    let refusal = runtime.add_processor(
        ProcessorSpec::new(camera, serde_json::json!({})).with_display_name("frontcam"),
    );

    match refusal {
        Err(Error::NodeNameTaken { name, cast }) => {
            assert_eq!(name, "frontcam");
            assert_eq!(cast, "frontcam");
        }
        other => panic!("expected NodeNameTaken, got {other:?}"),
    }
    assert_eq!(node_names_in_the_graph_json(&runtime), vec!["frontcam"]);
}

/// The suffix reaches the name and nothing else: identity is never derived
/// from the name, so the two nodes stay one type.
#[test]
#[serial]
fn the_suffix_never_reaches_the_processor_type() {
    let camera = register_test_type("TypeUntouchedCamera");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera.clone(), serde_json::json!({})))
        .unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera.clone(), serde_json::json!({})))
        .unwrap();

    let graph = runtime.to_json().expect("graph json");
    for node in graph["nodes"].as_array().expect("nodes array") {
        assert_eq!(node["type"], serde_json::json!(camera.as_str()));
    }
}

/// The default name is read off the registered descriptor, never recovered
/// from the import path.
///
/// The two are deliberately different here: the path ends `WidgetronImpl`, the
/// descriptor says `Widgetron`. Any implementation that splits the path on `::`
/// — the grammar re-invention #1840 forbids — yields `WidgetronImpl` and fails.
/// A fixture where the two coincided would pass under either mechanism and
/// prove nothing.
#[test]
#[serial]
fn the_default_name_comes_from_the_descriptor_not_the_import_path() {
    let widgetron = register_test_type_named("WidgetronImpl", "Widgetron");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(widgetron.clone(), serde_json::json!({})))
        .expect("the fixture type is registered");

    assert_eq!(
        node_names_in_the_graph_json(&runtime),
        vec!["widgetron".to_string()],
        "the name must be the descriptor's short name cast, not the path's tail"
    );
    assert!(
        widgetron.as_str().ends_with("::WidgetronImpl"),
        "the fixture only proves anything while the two genuinely differ"
    );
}
