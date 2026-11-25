// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `graph` document is a loadable graph: what a running stream renders loads
//! into a fresh one as the same graph, by name.

use streamlib::sdk::descriptors::{PinDeclaration, PinType};
use std::path::Path;
use std::sync::Arc;
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

use serial_test::serial;
use streamlib::sdk::descriptors::{NodeClassImportPath, NodeClassShortName, NodeDescriptor, PortDescriptor};
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::{
    LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner, RuntimeOperations,
};

mod graph_document_load_and_refusal_tests;
mod node_reference_tests;
mod variable_and_interface_tests;

/// Register a descriptor-only type with one input and one output port.
/// Idempotent under `serial_test`.
fn register_test_type(short: &str, input: &str, output: &str) -> NodeClassImportPath {
    let import_path = NodeClassImportPath::new(format!("{}::{short}", module_path!())).unwrap();
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short).unwrap(),
        import_path.clone(),
        "graph round-trip test",
    )
    .with_input(PortDescriptor::new(input, "", false, PinDeclaration::exactly(PinType::Texture { pixel_formats: Vec::new() })))
    .with_output(PortDescriptor::new(output, "", false, PinDeclaration::exactly(PinType::Texture { pixel_formats: Vec::new() })));
    let _ = NODE_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

fn the_graph_document_of(stream: &LoadedStreamInThisRuntime) -> serde_json::Value {
    stream.to_json().expect("the graph renders")
}

fn the_spec_in(graph_document: serde_json::Value) -> GraphSnapshot {
    GraphSnapshot::from_graph_document(graph_document).expect("a graph document is a graph")
}

/// The spec in `graph_document` with each node's UUID dropped, refusing a node that renders none.
fn the_spec_without_its_node_uuids_in(graph_document: serde_json::Value) -> GraphSnapshot {
    let mut graph = the_spec_in(graph_document);
    for node in &mut graph.nodes {
        assert!(
            node.id.take().is_some(),
            "node `{}` renders no UUID",
            node.name
        );
    }
    graph
}

/// Load options for the stream `stream_id` of the project in `project_directory`.
fn in_the_project(project_directory: &Path, stream_id: &str) -> OptionsForLoadingOneStream {
    options_loading_a_stream_of_the_test_project_at(project_directory, stream_id)
}

/// The stream id a test graph gives the stream it loads: its `stream`, else `main`.
fn the_stream_id_a_test_graph_gives(graph: &serde_json::Value) -> String {
    graph["stream"].as_str().unwrap_or("main").to_owned()
}

fn an_empty_stream_loaded_into(
    runner: &Runner,
    project_directory: &Path,
    stream_id: &str,
) -> Arc<LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(in_the_project(project_directory, stream_id))
        .expect("an empty stream loads")
}

/// `graph` loaded into `runner` as a stream of the project in
/// `project_directory`.
fn load(
    runner: &Runner,
    project_directory: &Path,
    graph: serde_json::Value,
) -> streamlib::sdk::error::Result<Arc<LoadedStreamInThisRuntime>> {
    let stream_id = the_stream_id_a_test_graph_gives(&graph);
    runner.load_stream_from_graph_snapshot(
        &the_spec_in(graph),
        in_the_project(project_directory, &stream_id),
    )
}

/// The refusal a load met; a load that succeeded fails the test naming `why`.
fn the_refusal_of(
    load: streamlib::sdk::error::Result<Arc<LoadedStreamInThisRuntime>>,
    why: &str,
) -> Error {
    match load {
        Ok(stream) => panic!("the stream `{}` loaded, and {why}", stream.stream_id()),
        Err(refusal) => refusal,
    }
}

/// An input of `name` holding a reference to a node of `referenced_node_type`.
fn an_input_holding_a_reference_to(
    name: &str,
    referenced_node_type: &NodeClassImportPath,
) -> PortDescriptor {
    use streamlib::sdk::descriptors::{NodeInputMode, PinDeclaration, PinType, SettingReadTime};
    PortDescriptor::new(name, "", false, PinDeclaration::exactly(PinType::NodeReference {
        referenced_node_type: Some(referenced_node_type.clone()),
        one_referrer_per_referenced_node: false,
    }))
    .with_input_mode(NodeInputMode::SettingOnly {
        read: SettingReadTime::AtSetup,
    })
}

/// Register a gateway type and a track type whose input `gateway` takes a
/// reference to a gateway.
fn register_a_gateway_and_a_track_naming_one() -> (NodeClassImportPath, NodeClassImportPath) {
    let gateway = register_test_type("ReferenceEdgeGateway", "_unused_in", "_unused_out");
    let track_import_path =
        NodeClassImportPath::new(format!("{}::ReferenceEdgeTrack", module_path!())).unwrap();
    let _ = NODE_REGISTRY.register_descriptor_only(
        NodeDescriptor::new(
            NodeClassShortName::new("ReferenceEdgeTrack").unwrap(),
            track_import_path.clone(),
            "graph round-trip test",
        )
        .with_input(an_input_holding_a_reference_to("gateway", &gateway)),
    );
    (gateway, track_import_path)
}
