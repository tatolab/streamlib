// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A loaded stream's live exposure map: a port's level set while the stream
//! runs shows in `graph` at once, a reader from outside the stream registers
//! only where the level allows it, and a lowered level cuts off the readers it
//! no longer allows.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use serial_test::serial;
use streamlib::sdk::descriptors::{
    PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::{OutputPortExposureLevel, OutputPortReaderLocation};
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

/// A cut that counts how often it ran.
fn a_cut_counting_into(cuts: &Arc<AtomicUsize>) -> Box<dyn FnOnce() + Send + Sync> {
    let cuts = Arc::clone(cuts);
    Box::new(move || {
        cuts.fetch_add(1, Ordering::SeqCst);
    })
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
fn a_reader_from_outside_the_stream_of_an_internal_port_is_refused_naming_the_port_and_its_level() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(&runner, project_directory.path(), serde_json::json!([]));
    let cuts = Arc::new(AtomicUsize::new(0));

    let refusal = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderLocation::ElsewhereOnThisMachine,
            a_cut_counting_into(&cuts),
        )
        .err()
        .expect("an internal port refuses a reader from another stream");

    match &refusal {
        Error::OutputPortNotExposedToTheReader {
            stream,
            node,
            port,
            level,
            ..
        } => {
            assert_eq!(
                (
                    stream.as_str(),
                    node.as_str(),
                    port.as_str(),
                    level.as_str()
                ),
                ("main", "camera", "video", "internal")
            );
        }
        other => panic!("expected OutputPortNotExposedToTheReader, got {other:?}"),
    }
    let message = refusal.to_string();
    for named in [
        "`video`",
        "`camera`",
        "`main`",
        "internal",
        "private or public",
    ] {
        assert!(message.contains(named), "{named:?} not in: {message}");
    }
    assert_eq!(
        Arc::strong_count(&cuts),
        1,
        "a refused reader's cut is not kept"
    );
}

#[test]
#[serial]
fn a_private_port_refuses_a_reader_on_another_machine_and_takes_one_on_this_machine() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(
        &runner,
        project_directory.path(),
        serde_json::json!([{"node": "camera", "port": "video", "level": "private"}]),
    );
    let cuts = Arc::new(AtomicUsize::new(0));

    let refusal = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderLocation::OnAnotherMachine,
            a_cut_counting_into(&cuts),
        )
        .err()
        .expect("a private port refuses a reader on another machine")
        .to_string();
    assert!(
        refusal.contains("private") && refusal.contains("only a port that is public"),
        "{refusal}"
    );

    let _registration = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderLocation::ElsewhereOnThisMachine,
            a_cut_counting_into(&cuts),
        )
        .expect("a private port takes a reader on this machine");
}

#[test]
#[serial]
fn lowering_a_level_cuts_off_at_once_every_reader_it_no_longer_allows_and_no_other() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(
        &runner,
        project_directory.path(),
        serde_json::json!([{"node": "camera", "port": "video", "level": "public"}]),
    );
    let on_this_machine = Arc::new(AtomicUsize::new(0));
    let on_another_machine = Arc::new(AtomicUsize::new(0));
    let _reader_on_this_machine = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderLocation::ElsewhereOnThisMachine,
            a_cut_counting_into(&on_this_machine),
        )
        .unwrap();
    let _reader_on_another_machine = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderLocation::OnAnotherMachine,
            a_cut_counting_into(&on_another_machine),
        )
        .unwrap();

    stream
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
        .unwrap();
    assert_eq!(on_another_machine.load(Ordering::SeqCst), 1);
    assert_eq!(on_this_machine.load(Ordering::SeqCst), 0);

    stream
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Internal)
        .unwrap();
    assert_eq!(on_this_machine.load(Ordering::SeqCst), 1);
    assert_eq!(
        on_another_machine.load(Ordering::SeqCst),
        1,
        "a reader is cut off once"
    );

    let refusal = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderLocation::ElsewhereOnThisMachine,
            a_cut_counting_into(&on_this_machine),
        )
        .err()
        .expect("a reader arriving after the level fell is checked against the new level");
    assert!(refusal.to_string().contains("internal"), "{refusal}");
}

#[test]
#[serial]
fn a_cut_runs_after_the_stream_lets_go_of_its_graph() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(
        &runner,
        project_directory.path(),
        serde_json::json!([{"node": "camera", "port": "video", "level": "private"}]),
    );
    let graph_read_inside_the_cut = Arc::new(std::sync::Mutex::new(None));
    let stream_held_weakly: Weak<LoadedStreamInThisRuntime> = Arc::downgrade(&stream);
    let into = Arc::clone(&graph_read_inside_the_cut);
    let _registration = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderLocation::ElsewhereOnThisMachine,
            Box::new(move || {
                let stream = stream_held_weakly.upgrade().expect("the stream is loaded");
                *into.lock().unwrap() =
                    Some(stream.to_json().expect("the graph renders")["exposed"].clone());
            }),
        )
        .unwrap();

    stream
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Internal)
        .unwrap();

    assert_eq!(
        graph_read_inside_the_cut.lock().unwrap().take(),
        Some(serde_json::json!([])),
        "the cut read the graph, already at the new level"
    );
}

#[test]
#[serial]
fn a_dropped_registration_takes_its_reader_off_the_port() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(
        &runner,
        project_directory.path(),
        serde_json::json!([{"node": "camera", "port": "video", "level": "private"}]),
    );
    let cuts = Arc::new(AtomicUsize::new(0));
    let registration = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderLocation::ElsewhereOnThisMachine,
            a_cut_counting_into(&cuts),
        )
        .unwrap();

    drop(registration);
    stream
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Internal)
        .unwrap();

    assert_eq!(cuts.load(Ordering::SeqCst), 0);
    assert_eq!(Arc::strong_count(&cuts), 1, "the port holds no cut for it");
}

#[test]
#[serial]
fn a_node_or_an_output_port_the_stream_does_not_hold_is_refused_by_name() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(&runner, project_directory.path(), serde_json::json!([]));

    for (node, port, named) in [
        ("display", "video", ["holds no node `display`", "camera"]),
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
