// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `AudioToTensor` takes Audio and `TensorToAudio` gives Audio, so `connect` judges a link
//! between either and a Float by their pin types and refuses it, where the Channels family both
//! messages belong to would have let it through.

use std::sync::Arc;

use streamlib::sdk::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration, PinType,
    PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner};
use streamlib_media_builtins::{AudioToTensor, TensorToAudio, register_media_builtin_node_types};

fn a_float_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(
        name,
        "a Float",
        false,
        PortFamilyDeclaration::of_family(PortFamily::Channels),
    )
    .with_pin_declaration(PinDeclaration::exactly(PinType::Float { exact_size: None }))
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
