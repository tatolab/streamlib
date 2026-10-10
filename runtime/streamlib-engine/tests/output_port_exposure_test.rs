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
use streamlib::sdk::graph::{
    CutOffAReaderOfAnExposedOutputPort, OutputPortExposureLevel, OutputPortReaderOutsideItsStream,
};
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::runtime::{
    KeptStreamRecord, LoadedStreamInThisRuntime, OptionsForLoadingOneStream, OwnerExposureRuling,
    Runner, StreamEnvironment,
};

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
fn a_cut_counting_into(cuts: &Arc<AtomicUsize>) -> CutOffAReaderOfAnExposedOutputPort {
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
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            a_cut_counting_into(&cuts),
        )
        .expect_err("an internal port refuses a reader from another stream");

    match &refusal {
        Error::OutputPortNotExposedToTheReader(refused) => {
            assert_eq!(
                (
                    refused.stream.as_str(),
                    refused.node.as_str(),
                    refused.port.as_str(),
                    refused.level.as_str()
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
            OutputPortReaderOutsideItsStream::OnAnotherMachine,
            a_cut_counting_into(&cuts),
        )
        .expect_err("a private port refuses a reader on another machine")
        .to_string();
    assert!(
        refusal.contains("private") && refusal.contains("only a port that is public"),
        "{refusal}"
    );

    let _registration = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
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
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            a_cut_counting_into(&on_this_machine),
        )
        .unwrap();
    let _reader_on_another_machine = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderOutsideItsStream::OnAnotherMachine,
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
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            a_cut_counting_into(&on_this_machine),
        )
        .expect_err("a reader arriving after the level fell is checked against the new level");
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
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
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
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
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
        (
            "display",
            "video",
            [
                "no node in this stream is named \"display\"",
                "This stream holds: camera",
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

#[test]
#[serial]
fn a_cut_that_owns_its_own_registration_is_cut_and_lets_it_go() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(
        &runner,
        project_directory.path(),
        serde_json::json!([{"node": "camera", "port": "video", "level": "private"}]),
    );
    let its_own_registration = Arc::new(std::sync::Mutex::new(None));
    let held_by_the_cut = Arc::clone(&its_own_registration);
    let cuts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&cuts);
    let registration = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                drop(held_by_the_cut.lock().unwrap().take());
            }),
        )
        .unwrap();
    *its_own_registration.lock().unwrap() = Some(registration);

    stream
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Internal)
        .unwrap();

    assert_eq!(cuts.load(Ordering::SeqCst), 1);
    assert!(its_own_registration.lock().unwrap().is_none());
}

#[test]
#[serial]
fn a_cut_that_panics_leaves_every_other_reader_cut_off() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(
        &runner,
        project_directory.path(),
        serde_json::json!([{"node": "camera", "port": "video", "level": "private"}]),
    );
    let _panicking_reader = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            Box::new(|| panic!("a reader's cut that panics")),
        )
        .unwrap();
    let cuts = Arc::new(AtomicUsize::new(0));
    let _reader_after_it = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            a_cut_counting_into(&cuts),
        )
        .unwrap();

    stream
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Internal)
        .expect("a panicking cut does not fail the level change");

    assert_eq!(cuts.load(Ordering::SeqCst), 1);
}

/// Records, when dropped, whether its stream's graph could be read: a drop
/// under the graph's write lock leaves the read waiting past the bound.
struct RecordsWhetherTheStreamsGraphWasFreeWhenDropped {
    stream: Weak<LoadedStreamInThisRuntime>,
    graph_was_free: Arc<std::sync::Mutex<Option<bool>>>,
}

impl Drop for RecordsWhetherTheStreamsGraphWasFreeWhenDropped {
    fn drop(&mut self) {
        let stream = self.stream.clone();
        let (rendered, graph_rendered) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(stream) = stream.upgrade() {
                let _ = stream.to_json();
            }
            let _ = rendered.send(());
        });
        *self.graph_was_free.lock().unwrap() = Some(
            graph_rendered
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_ok(),
        );
    }
}

/// A cut owning a probe that records whether the graph was free when the
/// cut was dropped, and the record it writes.
fn a_cut_probing_the_graph_lock_when_dropped(
    stream: &Arc<LoadedStreamInThisRuntime>,
) -> (
    CutOffAReaderOfAnExposedOutputPort,
    Arc<std::sync::Mutex<Option<bool>>>,
) {
    let graph_was_free = Arc::new(std::sync::Mutex::new(None));
    let probe = RecordsWhetherTheStreamsGraphWasFreeWhenDropped {
        stream: Arc::downgrade(stream),
        graph_was_free: Arc::clone(&graph_was_free),
    };
    (
        Box::new(move || {
            let _owned_by_the_cut = &probe;
        }),
        graph_was_free,
    )
}

#[test]
#[serial]
fn every_reader_a_registration_lets_go_of_is_dropped_once_the_graph_lock_is_released() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = a_camera_stream_exposing(
        &runner,
        project_directory.path(),
        serde_json::json!([{"node": "camera", "port": "video", "level": "private"}]),
    );

    for (node, port, refused_because) in [
        ("camera", "preview", "the port is internal"),
        ("camera", "missing", "the node has no such output"),
        ("display", "video", "the stream holds no such node"),
    ] {
        let (cut, graph_was_free) = a_cut_probing_the_graph_lock_when_dropped(&stream);
        assert!(
            stream
                .register_a_reader_of_an_exposed_output_port(
                    node,
                    port,
                    OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
                    cut,
                )
                .is_err(),
            "{refused_because}"
        );
        assert_eq!(
            *graph_was_free.lock().unwrap(),
            Some(true),
            "a reader refused because {refused_because} was dropped under the graph lock"
        );
    }

    let (cut, graph_was_free) = a_cut_probing_the_graph_lock_when_dropped(&stream);
    let first = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            cut,
        )
        .unwrap();
    drop(first);
    let cuts = Arc::new(AtomicUsize::new(0));
    let _second = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            a_cut_counting_into(&cuts),
        )
        .unwrap();
    assert_eq!(
        *graph_was_free.lock().unwrap(),
        Some(true),
        "a reader whose registration was dropped was let go under the graph lock"
    );
}

/// The owner's restriction of a port its stream function exposes more openly
/// is the level the loaded stream holds before it starts, so no reader from
/// outside the stream is ever admitted at the function's level.
#[test]
#[serial]
fn a_recorded_restriction_is_the_level_a_loaded_stream_holds_before_it_starts() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let mut record = KeptStreamRecord::of_a_running_stream(
        "main",
        &StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter: project_directory.path().join(".venv/bin/python"),
        },
        None,
        serde_json::json!({
            "stream": "main",
            "nodes": [{"name": "camera", "type": a_registered_camera_type().as_str()}],
            "exposed": [
                {"node": "camera", "port": "video", "level": "private"},
                {"node": "camera", "port": "preview", "level": "private"}
            ]
        }),
    );
    record.record_the_owners_exposure_ruling(OwnerExposureRuling {
        node: "camera".to_string(),
        port: "video".to_string(),
        level: OutputPortExposureLevel::Internal,
    });
    record.record_the_owners_exposure_ruling(OwnerExposureRuling {
        node: "camera".to_string(),
        port: "preview".to_string(),
        level: OutputPortExposureLevel::Public,
    });

    let graph =
        GraphSnapshot::from_graph_document(record.graph_with_the_owners_exposure_rulings_applied())
            .expect("the ruled graph is a graph");
    let stream = runner
        .load_stream_from_graph_snapshot(
            &graph,
            OptionsForLoadingOneStream::in_project_directory(project_directory.path()),
        )
        .expect("the ruled graph loads");

    assert_eq!(
        the_exposures_graph_renders_for(&stream),
        serde_json::json!([{"node": "camera", "port": "preview", "level": "public"}])
    );
    let cuts = Arc::new(AtomicUsize::new(0));
    let refusal = stream
        .register_a_reader_of_an_exposed_output_port(
            "camera",
            "video",
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            a_cut_counting_into(&cuts),
        )
        .expect_err("the owner's internal holds over the function's private");
    assert!(
        matches!(refusal, Error::OutputPortNotExposedToTheReader(_)),
        "{refusal}"
    );
}
