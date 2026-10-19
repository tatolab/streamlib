// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A loaded stream's live exposure map: a port's level set while the stream
//! runs shows in `graph` at once, and a node or port the stream does not hold
//! is refused by name.

use std::path::Path;
use std::sync::Arc;

use serial_test::serial;
use streamlib::sdk::descriptors::{
    PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use streamlib::sdk::graph::OutputPortExposureLevel;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner};

/// Register a descriptor-only camera type with a `video` and a `preview`
/// output and a `frames_in` input. Idempotent under `serial_test`.
fn a_registered_camera_type() -> ProcessorClassImportPath {
    let import_path =
        ProcessorClassImportPath::new(format!("{}::ExposureCamera", module_path!())).unwrap();
    let descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new("ExposureCamera").unwrap(),
        import_path.clone(),
        "output port exposure test",
    )
    .with_input(PortDescriptor::new("frames_in", "", false))
    .with_output(PortDescriptor::new("video", "", false))
    .with_output(PortDescriptor::new("preview", "", false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

/// A stream named `main` holding one camera node, `camera`, loaded with
/// `exposed` as written.
fn a_camera_stream_exposing(
    runner: &Runner,
    project_directory: &Path,
    exposed: serde_json::Value,
) -> Arc<LoadedStreamInThisRuntime> {
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "stream": "main",
        "nodes": [{"name": "camera", "type": a_registered_camera_type().as_str()}],
        "exposed": exposed
    }))
    .expect("a graph document is a graph");
    runner
        .load_stream_from_graph_snapshot(
            &graph,
            OptionsForLoadingOneStream::in_project_directory(project_directory),
        )
        .expect("the graph loads")
}

fn the_exposures_graph_renders_for(stream: &LoadedStreamInThisRuntime) -> serde_json::Value {
    stream.to_json().expect("the graph renders")["exposed"].clone()
}

#[test]
#[serial]
fn a_level_changed_live_shows_in_graph_at_once() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(
        &runner,
        project_directory.path(),
        serde_json::json!([{"node": "camera", "port": "video", "level": "public"}]),
    );
    assert_eq!(
        the_exposures_graph_renders_for(&stream),
        serde_json::json!([{"node": "camera", "port": "video", "level": "public"}])
    );

    stream
        .set_output_port_exposure_level("camera", "Preview", OutputPortExposureLevel::Private)
        .expect("a port is exposed live");
    stream
        .set_output_port_exposure_level("Camera", "video", OutputPortExposureLevel::Internal)
        .expect("a port is made internal live");

    assert_eq!(
        the_exposures_graph_renders_for(&stream),
        serde_json::json!([{"node": "camera", "port": "preview", "level": "private"}])
    );
}

#[test]
#[serial]
fn a_node_or_an_output_port_the_stream_does_not_hold_is_refused_by_name() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(&runner, project_directory.path(), serde_json::json!([]));

    for (node, port, named) in [
        (
            "display",
            "video",
            [
                "no node in the stream `main` is named \"display\"",
                "The stream `main` holds: camera",
            ],
        ),
        (
            "camera",
            "frames_in",
            ["no output port `frames_in`", "video, preview"],
        ),
    ] {
        let refusal = stream
            .set_output_port_exposure_level(node, port, OutputPortExposureLevel::Private)
            .unwrap_err()
            .to_string();
        for expected in named {
            assert!(refusal.contains(expected), "{expected:?} not in: {refusal}");
        }
    }
}
