// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A `graph` document is a loadable graph: what a running stream renders loads
//! into a fresh one as the same graph, by name.

use std::path::Path;
use std::sync::Arc;

use serial_test::serial;
use streamlib::sdk::descriptors::{
    PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner};

/// Register a descriptor-only type with one input and one output port.
/// Idempotent under `serial_test`.
fn register_test_type(short: &str, input: &str, output: &str) -> ProcessorClassImportPath {
    let import_path =
        ProcessorClassImportPath::new(format!("{}::{short}", module_path!())).unwrap();
    let descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new(short).unwrap(),
        import_path.clone(),
        "graph round-trip test",
    )
    .with_input(PortDescriptor::new(input, "", false))
    .with_output(PortDescriptor::new(output, "", false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

fn the_graph_document_of(stream: &LoadedStreamInThisRuntime) -> serde_json::Value {
    stream.to_json().expect("the graph renders")
}

fn the_spec_in(graph_document: serde_json::Value) -> GraphSnapshot {
    GraphSnapshot::from_graph_document(graph_document).expect("a graph document is a graph")
}

/// Load options for a stream whose project lives in `project_directory`,
/// named by the graph it loads.
fn in_the_project(project_directory: &Path) -> OptionsForLoadingOneStream {
    OptionsForLoadingOneStream::in_project_directory(project_directory)
}

fn an_empty_stream_loaded_into(
    runner: &Runner,
    project_directory: &Path,
    stream_name: &str,
) -> Arc<LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(in_the_project(project_directory).named(stream_name))
        .expect("an empty stream loads")
}

/// `graph` loaded into `runner` as a stream of the project in
/// `project_directory`.
fn load(
    runner: &Runner,
    project_directory: &Path,
    graph: serde_json::Value,
) -> streamlib::sdk::error::Result<Arc<LoadedStreamInThisRuntime>> {
    runner.load_stream_from_graph_snapshot(&the_spec_in(graph), in_the_project(project_directory))
}

/// The refusal a load met; a load that succeeded fails the test naming `why`.
fn the_refusal_of(
    load: streamlib::sdk::error::Result<Arc<LoadedStreamInThisRuntime>>,
    why: &str,
) -> Error {
    match load {
        Ok(stream) => panic!("the stream `{}` loaded, and {why}", stream.stream_name()),
        Err(refusal) => refusal,
    }
}

#[test]
#[serial]
fn a_graph_document_saved_from_a_running_stream_loads_back_as_the_same_graph() {
    let camera = register_test_type("RoundTripCamera", "_unused_in", "video");
    let display = register_test_type("RoundTripDisplay", "video_in", "_unused_out");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let first_runner = Runner::new().unwrap();
    let first =
        an_empty_stream_loaded_into(&first_runner, project_directory.path(), "built-in-code");
    let first_camera = first
        .add_processor(ProcessorSpec::new(camera.clone(), serde_json::json!({})))
        .unwrap();
    let second_camera = first
        .add_processor(ProcessorSpec::new(camera, serde_json::json!({"fps": 30})))
        .unwrap();
    let front_display = first
        .add_processor(
            ProcessorSpec::new(display.clone(), serde_json::json!({"width": 1920}))
                .with_display_name("Front Display"),
        )
        .unwrap();
    let back_display = first
        .add_processor(ProcessorSpec::new(display, serde_json::json!({})))
        .unwrap();
    first
        .connect(
            OutputLinkPortRef::new(&first_camera, "video"),
            InputLinkPortRef::new(&front_display, "video_in"),
        )
        .unwrap();
    first
        .connect(
            OutputLinkPortRef::new(&second_camera, "video"),
            InputLinkPortRef::new(&back_display, "video_in"),
        )
        .unwrap();
    let rendered_first = the_graph_document_of(&first);

    let second_runner = Runner::new().unwrap();
    let second = load(
        &second_runner,
        project_directory.path(),
        rendered_first.clone(),
    )
    .expect("a graph document loads");
    let rendered_second = the_graph_document_of(&second);

    assert_eq!(
        the_spec_in(rendered_second.clone()),
        the_spec_in(rendered_first.clone()),
        "the loaded graph must render the spec it was loaded from"
    );
    let node_names: Vec<&str> = rendered_second["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        node_names,
        [
            "roundtripcamera",
            "roundtripcamera-2",
            "front-display",
            "roundtripdisplay"
        ]
    );
    assert_eq!(
        rendered_second["links"][0]["source"],
        serde_json::json!({"node": "roundtripcamera", "port": "video"})
    );
    assert_eq!(
        rendered_second["links"][0]["target"],
        serde_json::json!({"node": "front-display", "port": "video_in"})
    );
    assert_ne!(
        rendered_second["nodes"][0]["id"], rendered_first["nodes"][0]["id"],
        "ids are live keys a load mints anew"
    );
    assert_eq!(rendered_second["exposed"], serde_json::json!([]));
    assert_eq!(
        rendered_second["stream"], "built-in-code",
        "a stream built in code renders its own name, and a load of it keeps that name"
    );
}

#[test]
#[serial]
fn a_loaded_stream_renders_its_name_and_its_exposures_and_round_trips_them() {
    let camera = register_test_type("ExposedCamera", "_unused_in", "video");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "main",
            "nodes": [{"name": "Front Camera", "type": camera.as_str(), "config": {}}],
            "exposed": [{"node": "front-camera", "port": "Video"}]
        }),
    )
    .expect("the graph loads");
    let rendered = the_graph_document_of(&stream);

    assert_eq!(rendered["stream"], "main");
    assert_eq!(
        rendered["exposed"],
        serde_json::json!([{"node": "front-camera", "port": "video"}])
    );

    let reloading_runner = Runner::new().unwrap();
    let reloaded = load(
        &reloading_runner,
        project_directory.path(),
        rendered.clone(),
    )
    .expect("the render loads");
    assert_eq!(
        the_spec_in(the_graph_document_of(&reloaded)),
        the_spec_in(rendered)
    );
}

#[test]
#[serial]
fn a_loaded_stream_name_is_recorded_in_its_cast_and_one_casting_to_nothing_is_refused() {
    let camera = register_test_type("StreamNameCastCamera", "_unused_in", "video");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "My Stream",
            "nodes": [{"name": "camera", "type": camera.as_str()}]
        }),
    )
    .expect("the graph loads");
    assert_eq!(the_graph_document_of(&stream)["stream"], "my-stream");
    assert_eq!(runner.names_of_the_loaded_streams(), ["my-stream"]);
    assert!(
        Arc::ptr_eq(
            &runner
                .loaded_stream_named("My Stream")
                .expect("a lookup casts the name it is given"),
            &stream
        ),
        "a lookup by the uncast name finds the stream"
    );

    let refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            serde_json::json!({
                "stream": "!!!",
                "nodes": [{"name": "camera", "type": camera.as_str()}]
            }),
        ),
        "a stream name casting to nothing is refused",
    )
    .to_string();
    assert!(
        refusal.contains("cannot load the stream `!!!`"),
        "{refusal}"
    );
    assert_eq!(runner.names_of_the_loaded_streams(), ["my-stream"]);
}

/// A stream name already loaded is refused naming where the first stream came
/// from and the way out; the first stream is left as it was.
#[test]
#[serial]
fn a_stream_name_already_loaded_is_refused_naming_where_the_first_came_from() {
    let camera = register_test_type("LoadedTwiceCamera", "_unused_in", "video");

    let first_project_directory = tempfile::tempdir().expect("a project directory");
    let second_project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let first = runner
        .load_stream_from_graph_snapshot(
            &the_spec_in(serde_json::json!({
                "stream": "main",
                "nodes": [{"name": "camera", "type": camera.as_str()}]
            })),
            in_the_project(first_project_directory.path()),
        )
        .expect("the first stream loads");

    let refusal = the_refusal_of(
        runner.load_stream_from_graph_snapshot(
            &the_spec_in(serde_json::json!({
                "stream": "Main",
                "nodes": [
                    {"name": "camera", "type": camera.as_str()},
                    {"name": "other", "type": camera.as_str()}
                ]
            })),
            in_the_project(second_project_directory.path()),
        ),
        "a name already loaded is refused",
    )
    .to_string();

    assert!(refusal.contains("`main`"), "{refusal}");
    assert!(
        refusal.contains(&first_project_directory.path().display().to_string()),
        "the refusal must name where the first stream came from: {refusal}"
    );
    assert!(refusal.contains("--name"), "{refusal}");
    assert_eq!(runner.names_of_the_loaded_streams(), ["main"]);
    assert_eq!(
        the_graph_document_of(&first)["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["camera"],
        "the refused load added a node to the stream already loaded under its name"
    );

    let renamed = runner
        .load_stream_from_graph_snapshot(
            &the_spec_in(serde_json::json!({
                "stream": "Main",
                "nodes": [{"name": "camera", "type": camera.as_str()}]
            })),
            in_the_project(second_project_directory.path()).named("second"),
        )
        .expect("the same graph loads under another name");
    assert_eq!(renamed.stream_name(), "second");
    assert_eq!(runner.names_of_the_loaded_streams(), ["main", "second"]);
}

/// A stream whose function adds nothing compiles to an empty graph, and the
/// runtime refuses it by name rather than running nothing.
#[test]
#[serial]
fn a_graph_holding_no_node_is_refused_naming_the_stream_and_the_fix() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();

    let refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            serde_json::json!({"stream": "main", "nodes": []}),
        ),
        "an empty graph is refused",
    )
    .to_string();
    assert!(
        refusal.contains("the stream `main` holds no node"),
        "{refusal}"
    );
    assert!(refusal.contains("stream_builder.add("), "{refusal}");

    let named_by_the_load_refusal = the_refusal_of(
        runner.load_stream_from_graph_snapshot(
            &the_spec_in(serde_json::json!({"nodes": []})),
            in_the_project(project_directory.path()).named("named-by-the-load"),
        ),
        "an empty graph is refused",
    )
    .to_string();
    assert!(
        named_by_the_load_refusal.contains("the stream `named-by-the-load` holds no node"),
        "{named_by_the_load_refusal}"
    );

    let unnamed_refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            serde_json::json!({"nodes": []}),
        ),
        "a graph naming no stream is refused",
    )
    .to_string();
    assert!(
        unnamed_refusal.contains("names no stream") && unnamed_refusal.contains("--name"),
        "{unnamed_refusal}"
    );
    assert!(runner.names_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn an_exposure_naming_an_input_port_is_refused_naming_the_outputs() {
    let camera = register_test_type("InputExposingCamera", "frames_in", "video");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            serde_json::json!({
                "stream": "main",
                "nodes": [{"name": "camera", "type": camera.as_str()}],
                "exposed": [{"node": "camera", "port": "frames_in"}]
            }),
        ),
        "only an output port is exposed",
    )
    .to_string();

    assert!(refusal.contains("no output port `frames_in`"), "{refusal}");
    assert!(refusal.contains("video"), "{refusal}");
    assert!(
        runner.names_of_the_loaded_streams().is_empty(),
        "a refused graph adds no stream"
    );
}

/// A link naming a port its node does not have, in that direction, is
/// refused before any node is added.
#[test]
#[serial]
fn a_load_refused_for_a_missing_link_port_adds_none_of_its_nodes() {
    let camera = register_test_type("MissingPortCamera", "frames_in", "video");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            serde_json::json!({
                "stream": "main",
                "nodes": [
                    {"name": "front", "type": camera.as_str()},
                    {"name": "back", "type": camera.as_str()}
                ],
                "links": [{"source": {"node": "front", "port": "video"},
                           "target": {"node": "back", "port": "no_such_input"}}]
            }),
        ),
        "the link names an input `back` does not have",
    )
    .to_string();

    assert!(
        refusal.contains("no input port `no_such_input`"),
        "{refusal}"
    );
    assert!(runner.names_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn an_exposure_named_twice_is_refused() {
    let camera = register_test_type("TwiceExposedCamera", "_unused_in", "video");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            serde_json::json!({
                "stream": "main",
                "nodes": [{"name": "camera", "type": camera.as_str()}],
                "exposed": [
                    {"node": "camera", "port": "video"},
                    {"node": "Camera", "port": "Video"}
                ]
            }),
        ),
        "one port is exposed once",
    )
    .to_string();

    assert!(refusal.contains("twice"), "{refusal}");
}

/// A node whose type never resolved renders under the requested import path
/// verbatim, and its render cannot be loaded — `validate` resolves every type.
///
/// A Rust path, because a Python one is described before it is added and,
/// with no stream environment to describe it in, is refused without a node.
#[test]
#[serial]
fn an_unresolved_node_renders_under_the_requested_path_and_refuses_to_load() {
    const UNRESOLVED: &str = "my_app::filters::NeverRegistered";

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
    let _ = stream.add_processor(ProcessorSpec::new(
        ProcessorClassImportPath::new(UNRESOLVED).unwrap(),
        serde_json::json!({}),
    ));
    let rendered = the_graph_document_of(&stream);
    assert_eq!(rendered["nodes"][0]["type"], UNRESOLVED);

    let reloading_runner = Runner::new().unwrap();
    match the_refusal_of(
        load(&reloading_runner, project_directory.path(), rendered),
        "a render naming an unresolved type cannot load",
    ) {
        Error::UnknownProcessorType { ident } => {
            assert_eq!(ident.as_str(), UNRESOLVED);
        }
        other => panic!("expected the load to refuse the unresolved type, got {other:?}"),
    }
}

/// A load refused at a link — after every node is in, where the engine refuses
/// a source port whose channel name cannot be built — adds no stream, and its
/// name is free for the next load.
#[test]
#[serial]
fn a_load_refused_at_a_link_adds_no_stream() {
    // A cast port name, so registration and `validate` take it, too long to
    // leave a channel name room for its processor's chunk.
    let port_name_too_long_for_a_channel = "v".repeat(60);
    let camera = register_test_type(
        "LinkRefusedCamera",
        "_unused_in",
        &port_name_too_long_for_a_channel,
    );
    let display = register_test_type("LinkRefusedDisplay", "video_in", "_unused_out");
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();

    let refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            serde_json::json!({
                "stream": "main",
                "nodes": [
                    {"name": "camera", "type": camera.as_str()},
                    {"name": "display", "type": display.as_str()}
                ],
                "links": [{"source": {"node": "camera", "port": port_name_too_long_for_a_channel},
                           "target": {"node": "display", "port": "video_in"}}]
            }),
        ),
        "the engine refuses the link",
    );

    assert!(
        matches!(refusal, Error::InvalidLink(_)),
        "the load must be refused at the link, not before it: {refusal:?}"
    );
    assert!(
        runner.names_of_the_loaded_streams().is_empty(),
        "a load refused at a link added a stream"
    );
    load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "main",
            "nodes": [{"name": "display", "type": display.as_str()}]
        }),
    )
    .expect("the refused stream's name is free for the next load");
}
