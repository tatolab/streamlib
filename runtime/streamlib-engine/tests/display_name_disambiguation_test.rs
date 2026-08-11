// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Duplicate display names are disambiguated by the graph, and the assigned
//! name is the one every surface shows.
//!
//! The unit coverage of the counter itself lives beside `add_v`; what this
//! locks is the whole path an author actually meets: the add reports the
//! assigned name a handle must carry, and the
//! graph JSON — the exact payload `streamlib graph` and `GET /api/graph` serve
//! (`to_json_async` in both) — carries the assigned names, not the requested
//! ones.

use serial_test::serial;
use streamlib::sdk::descriptors::{
    Org, Package, PortDescriptor, ProcessorDescriptor, SchemaIdent, SemVer, TypeName,
};
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;

/// Register a descriptor-only processor type — enough for `add_processor`'s
/// port-info lookup, with no instance to construct. Idempotent: a second
/// register under `serial_test` returns an already-registered error we ignore.
fn register_test_type(short: &str) -> SchemaIdent {
    let id = SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("display-name-test").unwrap(),
        TypeName::new(short).unwrap(),
        SemVer::new(1, 0, 0),
    );
    let descriptor = ProcessorDescriptor::new(id.clone(), "display-name disambiguation test")
        .with_input(PortDescriptor::new("_unused_in", "", false))
        .with_output(PortDescriptor::new("_unused_out", "", false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    id
}

/// Every node's display name, in node-iteration order, as the graph JSON
/// renders it.
fn display_names_in_the_graph_json(runtime: &Runner) -> Vec<String> {
    runtime.to_json().expect("graph json")["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|node| {
            node["display_name"]
                .as_str()
                .expect("every node renders a display name")
                .to_string()
        })
        .collect()
}

#[test]
#[serial]
fn the_graph_json_carries_distinct_names_for_two_nodes_of_one_type() {
    let camera = register_test_type("DisambiguatedCamera");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera.clone(), serde_json::json!({})))
        .unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera, serde_json::json!({})))
        .unwrap();

    assert_eq!(
        display_names_in_the_graph_json(&runtime),
        vec!["DisambiguatedCamera", "DisambiguatedCamera 2"],
        "`streamlib graph` must name the two instances apart"
    );
}

#[test]
#[serial]
fn the_read_back_name_is_the_assigned_one_not_the_requested_one() {
    let camera = register_test_type("ReadBackCamera");

    let runtime = Runner::new().unwrap();
    let (_first_id, first_name) = runtime
        .add_processor_reporting_assigned_display_name(
            ProcessorSpec::new(camera.clone(), serde_json::json!({})).with_display_name("Front"),
        )
        .unwrap();
    let (_second_id, second_name) = runtime
        .add_processor_reporting_assigned_display_name(
            ProcessorSpec::new(camera, serde_json::json!({})).with_display_name("Front"),
        )
        .unwrap();

    assert_eq!(first_name, "Front");
    assert_eq!(
        second_name, "Front 2",
        "the second add asked for `Front` and must be told it got `Front 2`"
    );
}

/// The counter reaches the label and nothing else: identity is never derived
/// from the display name, so the two nodes stay one type.
#[test]
#[serial]
fn the_counter_never_reaches_the_processor_type() {
    let camera = register_test_type("TypeUntouchedCamera");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera.clone(), serde_json::json!({})))
        .unwrap();
    runtime
        .add_processor(ProcessorSpec::new(camera, serde_json::json!({})))
        .unwrap();

    let graph = runtime.to_json().expect("graph json");
    for node in graph["nodes"].as_array().expect("nodes array") {
        assert_eq!(node["type"]["type"], "TypeUntouchedCamera");
    }
}

/// Save → load → save stays byte-equivalent with duplicates in the graph: the
/// disambiguated name serializes (it is no longer the type's short name), and
/// reloading it collides with nothing, so the counter does not climb on every
/// round trip.
#[test]
#[serial]
fn a_graph_with_duplicates_round_trips_without_the_counter_climbing() {
    let camera = register_test_type("RoundTripCamera");

    let first_runtime = Runner::new().unwrap();
    for _ in 0..3 {
        first_runtime
            .add_processor(ProcessorSpec::new(camera.clone(), serde_json::json!({})))
            .unwrap();
    }
    let first_snapshot = first_runtime.save_graph_snapshot().unwrap();

    let second_runtime = Runner::new().unwrap();
    second_runtime
        .load_graph_snapshot(
            &GraphSnapshot::from_json_str(&first_snapshot.to_json_string().unwrap()).unwrap(),
        )
        .unwrap();

    assert_eq!(
        display_names_in_the_graph_json(&second_runtime),
        vec!["RoundTripCamera", "RoundTripCamera 2", "RoundTripCamera 3"],
        "a reloaded graph must carry the same names, not re-decorated ones"
    );
    assert_eq!(
        second_runtime.save_graph_snapshot().unwrap(),
        first_snapshot,
        "save → load → save must be byte-equivalent"
    );
}
