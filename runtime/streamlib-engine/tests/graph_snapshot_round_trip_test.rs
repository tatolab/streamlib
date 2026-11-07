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
    stream_id: &str,
) -> Arc<LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(in_the_project(project_directory).with_stream_id(stream_id))
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
        Ok(stream) => panic!("the stream `{}` loaded, and {why}", stream.stream_id()),
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
fn a_loaded_stream_renders_its_name_and_its_exposures_at_their_levels_and_round_trips_them() {
    let camera = register_test_type("ExposedCamera", "_unused_in", "video");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "main",
            "nodes": [
                {"name": "Front Camera", "type": camera.as_str(), "config": {}},
                {"name": "Back Camera", "type": camera.as_str(), "config": {}}
            ],
            "exposed": [
                {"node": "front-camera", "port": "Video"},
                {"node": "back-camera", "port": "video", "level": "public"}
            ]
        }),
    )
    .expect("the graph loads");
    let rendered = the_graph_document_of(&stream);

    assert_eq!(rendered["stream"], "main");
    assert_eq!(
        rendered["exposed"],
        serde_json::json!([
            {"node": "front-camera", "port": "video", "level": "private"},
            {"node": "back-camera", "port": "video", "level": "public"}
        ]),
        "an exposure renders its level, and one written with no level renders as private"
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
fn a_loaded_stream_is_keyed_by_the_cast_of_the_name_its_load_gives_and_one_casting_to_nothing_is_refused()
 {
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
    assert_eq!(runner.ids_of_the_loaded_streams(), ["my-stream"]);
    assert!(
        Arc::ptr_eq(
            &runner
                .loaded_stream_of_the_id("my-stream")
                .expect("a lookup takes the stream id"),
            &stream
        ),
        "a lookup by the id the name cast to finds the stream"
    );
    assert!(
        runner.loaded_stream_of_the_id("My Stream").is_err(),
        "a lookup matches the id exactly and never casts what it is given"
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
    assert_eq!(runner.ids_of_the_loaded_streams(), ["my-stream"]);
}

/// A stream name already loaded is refused naming where the first stream came
/// from and the way out; the first stream is left as it was.
#[test]
#[serial]
fn a_stream_id_already_loaded_is_refused_naming_where_the_first_came_from() {
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
    assert!(refusal.contains("with_stream_id"), "{refusal}");
    assert_eq!(runner.ids_of_the_loaded_streams(), ["main"]);
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
            in_the_project(second_project_directory.path()).with_stream_id("second"),
        )
        .expect("the same graph loads under another name");
    assert_eq!(renamed.stream_id(), "second");
    assert_eq!(runner.ids_of_the_loaded_streams(), ["main", "second"]);
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
            in_the_project(project_directory.path()).with_stream_id("named-by-the-load"),
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
        unnamed_refusal.contains("names no stream") && unnamed_refusal.contains("with_stream_id"),
        "{unnamed_refusal}"
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());
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
        runner.ids_of_the_loaded_streams().is_empty(),
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
    assert!(runner.ids_of_the_loaded_streams().is_empty());
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
        runner.ids_of_the_loaded_streams().is_empty(),
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

#[test]
#[serial]
fn a_streams_clock_loads_renders_and_loads_back() {
    let source = register_test_type("ClockedSource", "_unused_in", "video");
    let project_directory = tempfile::tempdir().expect("a project directory");
    let graph = serde_json::json!({
        "stream": "slow-motion",
        "clock": {"mode": "scaled", "scale": 0.5, "step_ns": 1000},
        "nodes": [{"name": "source", "type": source.as_str()}],
    });

    let first_runner = Runner::new().unwrap();
    let first = load(&first_runner, project_directory.path(), graph).expect("the stream loads");
    let rendered = the_graph_document_of(&first);
    assert_eq!(
        rendered["clock"],
        serde_json::json!({"mode": "scaled", "scale": 0.5, "step_ns": 1000})
    );

    let second_runner = Runner::new().unwrap();
    let second = load(&second_runner, project_directory.path(), rendered.clone())
        .expect("the rendered graph loads back");
    assert_eq!(the_graph_document_of(&second)["clock"], rendered["clock"]);
}

#[test]
#[serial]
fn a_stream_on_real_time_renders_no_clock_key() {
    let source = register_test_type("UnclockedSource", "_unused_in", "video");
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "live",
            "nodes": [{"name": "source", "type": source.as_str()}],
        }),
    )
    .expect("the stream loads");
    assert!(the_graph_document_of(&stream).get("clock").is_none());
}

#[test]
#[serial]
fn a_clock_this_runtime_does_not_run_is_refused_at_load_naming_the_stream() {
    let source = register_test_type("SteppedSource", "_unused_in", "video");
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    for clock in [
        serde_json::json!({"mode": "stepped", "step_ns": 0}),
        serde_json::json!({"mode": "scaled", "scale": 4.0, "step_ns": 0}),
        serde_json::json!({"mode": "scaled", "scale": 0.0}),
    ] {
        let refusal = the_refusal_of(
            load(
                &runner,
                project_directory.path(),
                serde_json::json!({
                    "stream": "warped",
                    "clock": clock,
                    "nodes": [{"name": "source", "type": source.as_str()}],
                }),
            ),
            "its clock is one this runtime does not run",
        );
        assert!(
            refusal.to_string().contains("`warped`"),
            "{refusal} names the stream"
        );
    }
}

#[test]
#[serial]
fn a_reference_edge_in_a_config_renders_beside_the_links_and_loads_back() {
    let gateway = register_test_type("ReferenceEdgeGateway", "_unused_in", "_unused_out");
    let track_import_path =
        ProcessorClassImportPath::new(format!("{}::ReferenceEdgeTrack", module_path!())).unwrap();
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(
        ProcessorDescriptor::new(
            ProcessorClassShortName::new("ReferenceEdgeTrack").unwrap(),
            track_import_path.clone(),
            "graph round-trip test",
        )
        .with_config_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "gateway": {
                    streamlib::sdk::descriptors::NODE_REFERENCE_SCHEMA_KEYWORD:
                        {"node_type": gateway.as_str()}
                }
            }
        })),
    );
    let graph = serde_json::json!({
        "stream": "reference-edges",
        "nodes": [
            {"name": "cloudflare", "type": gateway.as_str()},
            {
                "name": "camera-track",
                "type": track_import_path.as_str(),
                "config": {"gateway": {"node": "cloudflare"}}
            }
        ]
    });

    let project_directory = tempfile::tempdir().expect("a project directory");
    let first_runner = Runner::new().unwrap();
    let first = load(&first_runner, project_directory.path(), graph).expect("the stream loads");
    let rendered = the_graph_document_of(&first);
    assert_eq!(
        rendered["references"],
        serde_json::json!([
            {"node": "camera-track", "config_key": "gateway", "referenced_node": "cloudflare"}
        ])
    );
    assert!(
        rendered["links"].as_array().unwrap().is_empty(),
        "a reference edge is never a link"
    );

    let second_runner = Runner::new().unwrap();
    let second = load(&second_runner, project_directory.path(), rendered.clone())
        .expect("a rendered reference edge loads back");
    assert_eq!(
        the_graph_document_of(&second)["references"],
        rendered["references"]
    );
}

/// Register a descriptor-only type whose ports declare `family`.
fn register_a_family_type(
    short: &str,
    inputs: &[(&str, streamlib::sdk::descriptors::PortFamily)],
    outputs: &[(&str, streamlib::sdk::descriptors::PortFamily)],
) -> ProcessorClassImportPath {
    use streamlib::sdk::descriptors::PortFamilyDeclaration;
    let import_path =
        ProcessorClassImportPath::new(format!("{}::{short}", module_path!())).unwrap();
    let mut descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new(short).unwrap(),
        import_path.clone(),
        "graph variable test",
    );
    for (name, family) in inputs {
        descriptor = descriptor.with_input(
            PortDescriptor::new(*name, "", false)
                .with_family(PortFamilyDeclaration::of_family(*family)),
        );
    }
    for (name, family) in outputs {
        descriptor = descriptor.with_output(
            PortDescriptor::new(*name, "", false)
                .with_family(PortFamilyDeclaration::of_family(*family)),
        );
    }
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

fn an_image_variable_read_by_two_readers(
    camera: &ProcessorClassImportPath,
    second_reader: &ProcessorClassImportPath,
) -> serde_json::Value {
    serde_json::json!({
        "stream": "image-variable",
        "nodes": [
            {"name": "camera", "type": camera.as_str()},
            {"name": "set-camera", "type": "tatolab.stream:SetVariable", "config": {"variable": "camera"}},
            {"name": "get-camera", "type": "tatolab.stream:GetVariable", "config": {"variable": "camera"}},
            {"name": "get-camera-2", "type": "tatolab.stream:GetVariable", "config": {"variable": "camera"}},
            {"name": "front", "type": camera.as_str().replace("VariableCamera", "VariableDisplay")},
            {"name": "back", "type": second_reader.as_str()}
        ],
        "links": [
            {"source": {"node": "camera", "port": "video"}, "target": {"node": "set-camera", "port": "value"}},
            {"source": {"node": "get-camera", "port": "value"}, "target": {"node": "front", "port": "video_in"}},
            {"source": {"node": "get-camera-2", "port": "value"}, "target": {"node": "back", "port": "video_in"}}
        ]
    })
}

#[test]
#[serial]
fn an_image_variable_read_twice_runs_as_two_links_and_renders_as_drawn() {
    use streamlib::sdk::descriptors::PortFamily;
    let camera = register_a_family_type("VariableCamera", &[], &[("video", PortFamily::Image)]);
    let display =
        register_a_family_type("VariableDisplay", &[("video_in", PortFamily::Image)], &[]);
    let graph = an_image_variable_read_by_two_readers(&camera, &display);

    let project_directory = tempfile::tempdir().expect("a project directory");
    let first_runner = Runner::new().unwrap();
    let first = load(&first_runner, project_directory.path(), graph.clone())
        .expect("an Image variable read twice loads");
    let rendered = the_graph_document_of(&first);
    let link_ids_by_drawn_source: Vec<(&str, &str)> = rendered["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| {
            (
                link["source"]["node"].as_str().unwrap(),
                link["id"].as_str().unwrap(),
            )
        })
        .collect();
    let ids_of_links_out_of_a_get: Vec<&str> = link_ids_by_drawn_source
        .iter()
        .filter(|(source, _)| source.starts_with("get-camera"))
        .map(|(_, id)| *id)
        .collect();
    assert_eq!(ids_of_links_out_of_a_get.len(), 2);
    assert!(
        ids_of_links_out_of_a_get.iter().all(|id| !id.is_empty())
            && ids_of_links_out_of_a_get[0] != ids_of_links_out_of_a_get[1],
        "each get's drawn link carries the id of the link the runtime runs: {link_ids_by_drawn_source:?}"
    );
    for node in rendered["nodes"].as_array().unwrap() {
        let is_a_variable_node = node["type"]
            .as_str()
            .unwrap()
            .starts_with("tatolab.stream:");
        assert_eq!(
            node["id"].as_str().unwrap().is_empty(),
            is_a_variable_node,
            "only a variable node renders with no runtime id: {node}"
        );
    }

    assert_eq!(
        the_spec_in(rendered.clone()),
        the_spec_in(graph),
        "graph renders the variables as drawn"
    );
    let second_runner = Runner::new().unwrap();
    let second = load(&second_runner, project_directory.path(), rendered.clone())
        .expect("a rendered graph holding variables loads back");
    assert_eq!(
        the_spec_in(the_graph_document_of(&second)),
        the_spec_in(rendered)
    );
}

#[test]
#[serial]
fn a_get_read_as_the_wrong_family_is_refused_at_load_naming_the_get_and_its_variable() {
    use streamlib::sdk::descriptors::PortFamily;
    let camera = register_a_family_type("VariableCamera", &[], &[("video", PortFamily::Image)]);
    register_a_family_type("VariableDisplay", &[("video_in", PortFamily::Image)], &[]);
    let records_reader = register_a_family_type(
        "VariableRecordsReader",
        &[("video_in", PortFamily::Records)],
        &[],
    );

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            an_image_variable_read_by_two_readers(&camera, &records_reader),
        ),
        "a get read as Records from an Image variable must be refused",
    )
    .to_string();
    assert!(
        refusal.contains("`GetVariable` node `get-camera-2` reads the variable `camera`"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_variable_holding_a_gateway_reference_renders_a_reference_edge_and_loads_back() {
    let gateway = register_test_type("VariableGateway", "_unused_in", "_unused_out");
    let track_import_path =
        ProcessorClassImportPath::new(format!("{}::VariableTrack", module_path!())).unwrap();
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(
        ProcessorDescriptor::new(
            ProcessorClassShortName::new("VariableTrack").unwrap(),
            track_import_path.clone(),
            "graph variable test",
        )
        .with_config_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "gateway": {
                    streamlib::sdk::descriptors::NODE_REFERENCE_SCHEMA_KEYWORD:
                        {"node_type": gateway.as_str()}
                }
            }
        })),
    );
    let graph = serde_json::json!({
        "stream": "gateway-variable",
        "nodes": [
            {"name": "cloudflare", "type": gateway.as_str()},
            {"name": "set-connection", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "connection", "node_reference": {"node": "cloudflare"}}},
            {"name": "get-connection", "type": "tatolab.stream:GetVariable",
             "config": {"variable": "connection"}},
            {"name": "camera-track", "type": track_import_path.as_str(),
             "config": {"gateway": {"node": "get-connection"}}}
        ]
    });

    let project_directory = tempfile::tempdir().expect("a project directory");
    let first_runner = Runner::new().unwrap();
    let first = load(&first_runner, project_directory.path(), graph.clone())
        .expect("a gateway held in a variable loads");
    let rendered = the_graph_document_of(&first);
    assert_eq!(
        rendered["references"],
        serde_json::json!([
            {"node": "camera-track", "config_key": "gateway", "referenced_node": "cloudflare"}
        ])
    );
    assert_eq!(the_spec_in(rendered.clone()), the_spec_in(graph));

    let second_runner = Runner::new().unwrap();
    load(&second_runner, project_directory.path(), rendered)
        .expect("a rendered gateway variable loads back");

    let wrong_type = serde_json::json!({
        "stream": "gateway-variable-of-the-wrong-type",
        "nodes": [
            {"name": "cloudflare", "type": track_import_path.as_str()},
            {"name": "set-connection", "type": "tatolab.stream:SetVariable",
             "config": {"variable": "connection", "node_reference": {"node": "cloudflare"}}},
            {"name": "get-connection", "type": "tatolab.stream:GetVariable",
             "config": {"variable": "connection"}},
            {"name": "camera-track", "type": track_import_path.as_str(),
             "config": {"gateway": {"node": "get-connection"}}}
        ]
    });
    let refusal = the_refusal_of(
        load(
            &Runner::new().unwrap(),
            project_directory.path(),
            wrong_type,
        ),
        "a variable naming a track where a gateway is taken must be refused",
    )
    .to_string();
    assert!(
        refusal.contains(&format!("takes a `{}`", gateway.as_str())),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_variable_read_by_a_node_named_as_typed_renders_its_link_once() {
    use streamlib::sdk::descriptors::PortFamily;
    let camera = register_a_family_type("VariableCamera", &[], &[("video", PortFamily::Image)]);
    let display =
        register_a_family_type("VariableDisplay", &[("video_in", PortFamily::Image)], &[]);
    let mut graph = an_image_variable_read_by_two_readers(&camera, &display);
    graph["nodes"][5]["name"] = serde_json::json!("Back Display");
    graph["links"][2]["target"]["node"] = serde_json::json!("Back Display");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = load(&runner, project_directory.path(), graph).expect("the stream loads");
    let links_into = |rendered: &serde_json::Value, node: &str| -> Vec<serde_json::Value> {
        rendered["links"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|link| link["target"]["node"] == node)
            .cloned()
            .collect()
    };
    let rendered = the_graph_document_of(&stream);
    let into_the_back_display = links_into(&rendered, "back-display");
    assert_eq!(into_the_back_display.len(), 1, "{into_the_back_display:?}");
    assert_eq!(into_the_back_display[0]["source"]["node"], "get-camera-2");
    assert!(!into_the_back_display[0]["id"].as_str().unwrap().is_empty());
}
