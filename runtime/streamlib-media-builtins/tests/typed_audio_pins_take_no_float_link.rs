// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `AudioToTensor` takes Audio and `TensorToAudio` gives Audio, so `connect` judges a link
//! between either and a Float by their pin types and refuses it, though both travel as Channels
//! blocks. A saved graph that already holds such a link, under the conversion's old name, still
//! loads with the link kept, since both its ends travel as one kind of message.

use std::sync::Arc;

use streamlib::sdk::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration, PinType,
    PortDescriptor,
};
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner};
use streamlib_media_builtins::{AudioToTensor, TensorToAudio, register_media_builtin_node_types};

fn a_float_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(
        name,
        "a Float",
        false,
        PinDeclaration::exactly(PinType::Float { exact_size: None }),
    )
}

fn a_fixture_node_type(class_name: &str) -> NodeDescriptor {
    NodeDescriptor::new(
        NodeClassShortName::new(class_name).unwrap(),
        NodeClassImportPath::new(format!("{}::{class_name}", module_path!())).unwrap(),
        "a fixture with one Float port",
    )
}

/// A node type with one Float output `value`, and one with one Float input `value`.
fn the_float_source_and_sink_registered() -> (NodeClassImportPath, NodeClassImportPath) {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    let source = a_fixture_node_type("FloatSource").with_output(a_float_port("value"));
    let sink = a_fixture_node_type("FloatSink").with_input(a_float_port("value"));
    let types = (
        source.node_class_import_path.clone(),
        sink.node_class_import_path.clone(),
    );
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register_media_builtin_node_types();
        for descriptor in [source, sink] {
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("the fixture registers");
        }
    });
    types
}

fn an_empty_stream(
    runner: &Runner,
    project_directory: &std::path::Path,
) -> Arc<LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads")
}

fn the_refusal_of(refused: Result<impl std::fmt::Debug, impl std::fmt::Display>) -> String {
    match refused {
        Err(refusal) => refusal.to_string(),
        Ok(link) => panic!("the link was made: {link:?}"),
    }
}

#[test]
fn connect_refuses_a_float_into_audio_to_tensor() {
    let (float_source, _) = the_float_source_and_sink_registered();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("a runtime constructs");
    let stream = an_empty_stream(&runner, project_directory.path());
    let source = stream
        .add_node(NodeSpec::new(float_source, serde_json::json!({})))
        .unwrap();
    let to_tensor = stream
        .add_node(NodeSpec::new(
            AudioToTensor::node_class_import_path(),
            serde_json::json!({}),
        ))
        .unwrap();

    let refusal = the_refusal_of(stream.connect(
        OutputLinkPortRef::new(&source, "value"),
        InputLinkPortRef::new(&to_tensor, "channels"),
    ));

    assert!(
        refusal.contains("(Float)") && refusal.contains("(Audio)"),
        "{refusal}"
    );
}

#[test]
fn connect_refuses_tensor_to_audio_into_a_float() {
    let (_, float_sink) = the_float_source_and_sink_registered();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("a runtime constructs");
    let stream = an_empty_stream(&runner, project_directory.path());
    let to_audio = stream
        .add_node(NodeSpec::new(
            TensorToAudio::node_class_import_path(),
            serde_json::json!({}),
        ))
        .unwrap();
    let sink = stream
        .add_node(NodeSpec::new(float_sink, serde_json::json!({})))
        .unwrap();

    let refusal = the_refusal_of(stream.connect(
        OutputLinkPortRef::new(&to_audio, "channels"),
        InputLinkPortRef::new(&sink, "value"),
    ));

    assert!(
        refusal.contains("(Audio)") && refusal.contains("(Float)"),
        "{refusal}"
    );
}

/// A saved graph whose one link runs from `output_port` of a `source_type` node to
/// `input_port` of a `sink_type` node, loaded on a fresh runtime; the links the stream renders.
fn the_links_a_saved_graph_loads_with(
    source_type: &str,
    output_port: &str,
    sink_type: &str,
    input_port: &str,
) -> Vec<serde_json::Value> {
    let saved_graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [
            {"id": "00000000-0000-4000-8000-000000000001", "name": "source", "type": source_type},
            {"id": "00000000-0000-4000-8000-000000000002", "name": "sink", "type": sink_type}
        ],
        "links": [{
            "source": {"node": "00000000-0000-4000-8000-000000000001", "port": output_port},
            "target": {"node": "00000000-0000-4000-8000-000000000002", "port": input_port}
        }]
    }))
    .expect("the saved graph reads");
    let project_directory = tempfile::tempdir().expect("a project directory");
    let stream = Runner::new()
        .expect("a runtime constructs")
        .load_stream_from_graph_snapshot(
            &saved_graph,
            options_loading_a_stream_of_the_test_project_at(project_directory.path(), "main"),
        )
        .unwrap_or_else(|refusal| panic!("the saved graph does not load: {refusal}"));
    stream.to_json().expect("the stream renders")["links"]
        .as_array()
        .expect("a render holds links")
        .clone()
}

#[test]
fn a_saved_float_link_into_the_old_channels_to_array_loads_and_is_kept() {
    let (float_source, _) = the_float_source_and_sink_registered();

    let links = the_links_a_saved_graph_loads_with(
        float_source.as_str(),
        "value",
        "tatolab.stream:ChannelsToArray",
        "channels",
    );

    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["target"]["port"], "channels", "{links:?}");
}

#[test]
fn a_saved_link_from_the_old_array_to_channels_into_a_float_loads_and_is_kept() {
    let (_, float_sink) = the_float_source_and_sink_registered();

    let links = the_links_a_saved_graph_loads_with(
        "tatolab.stream:ArrayToChannels",
        "channels",
        float_sink.as_str(),
        "value",
    );

    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["source"]["port"], "channels", "{links:?}");
}
