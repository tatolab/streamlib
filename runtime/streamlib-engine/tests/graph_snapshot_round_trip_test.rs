// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A `graph` document is a loadable graph: what a running engine renders loads
//! into a fresh one as the same graph, by name.

use serial_test::serial;
use streamlib::sdk::descriptors::{
    PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::{InputLinkPortRef, MeshPortAddress, OutputLinkPortRef};
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;

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

fn the_graph_document_of(runtime: &Runner) -> serde_json::Value {
    runtime.to_json().expect("the graph renders")
}

fn the_spec_in(graph_document: serde_json::Value) -> GraphSnapshot {
    GraphSnapshot::from_graph_document(graph_document).expect("a graph document is a graph")
}

#[test]
#[serial]
fn a_graph_document_saved_from_a_running_engine_loads_back_as_the_same_graph() {
    let camera = register_test_type("RoundTripCamera", "_unused_in", "video");
    let display = register_test_type("RoundTripDisplay", "video_in", "_unused_out");

    let first = Runner::new().unwrap();
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

    let second = Runner::new().unwrap();
    second
        .load_graph_snapshot(&the_spec_in(rendered_first.clone()))
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
    assert!(
        rendered_second.get("stream").is_none(),
        "a graph built rather than loaded as a stream names none"
    );
}

#[test]
#[serial]
fn a_loaded_stream_renders_its_name_and_its_exposures_and_round_trips_them() {
    let camera = register_test_type("ExposedCamera", "_unused_in", "video");

    let runtime = Runner::new().unwrap();
    runtime
        .load_graph_snapshot(&the_spec_in(serde_json::json!({
            "stream": "main",
            "nodes": [{"name": "Front Camera", "type": camera.as_str(), "config": {}}],
            "exposed": [{"node": "front-camera", "port": "Video"}]
        })))
        .expect("the graph loads");
    let rendered = the_graph_document_of(&runtime);

    assert_eq!(rendered["stream"], "main");
    assert_eq!(
        rendered["exposed"],
        serde_json::json!([{"node": "front-camera", "port": "video"}])
    );

    let reloaded = Runner::new().unwrap();
    reloaded
        .load_graph_snapshot(&the_spec_in(rendered.clone()))
        .expect("the render loads");
    assert_eq!(
        the_spec_in(the_graph_document_of(&reloaded)),
        the_spec_in(rendered)
    );
}

#[test]
#[serial]
fn a_loaded_name_already_in_the_graph_is_refused_rather_than_suffixed() {
    let camera = register_test_type("LoadedTwiceCamera", "_unused_in", "video");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(
            ProcessorSpec::new(camera.clone(), serde_json::json!({})).with_display_name("camera"),
        )
        .unwrap();

    let refusal = runtime.load_graph_snapshot(&the_spec_in(serde_json::json!({
        "nodes": [{"name": "Camera", "type": camera.as_str()}]
    })));

    match refusal {
        Err(Error::NodeNameTaken { name, cast }) => {
            assert_eq!(name, "Camera");
            assert_eq!(cast, "camera");
        }
        other => panic!("expected NodeNameTaken, got {other:?}"),
    }
}

#[test]
#[serial]
fn an_exposure_naming_an_input_port_is_refused_naming_the_outputs() {
    let camera = register_test_type("InputExposingCamera", "frames_in", "video");

    let runtime = Runner::new().unwrap();
    let refusal = runtime
        .load_graph_snapshot(&the_spec_in(serde_json::json!({
            "nodes": [{"name": "camera", "type": camera.as_str()}],
            "exposed": [{"node": "camera", "port": "frames_in"}]
        })))
        .expect_err("only an output port is exposed")
        .to_string();

    assert!(refusal.contains("no output port `frames_in`"), "{refusal}");
    assert!(refusal.contains("video"), "{refusal}");
    assert_eq!(
        the_graph_document_of(&runtime)["nodes"],
        serde_json::json!([]),
        "a refused graph adds nothing"
    );
}

/// Every name is checked against the live graph before the first node is
/// added, so a refused load leaves the graph as it found it.
#[test]
#[serial]
fn a_load_refused_for_a_taken_name_adds_none_of_its_nodes() {
    let camera = register_test_type("PartlyTakenCamera", "_unused_in", "video");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(
            ProcessorSpec::new(camera.clone(), serde_json::json!({})).with_display_name("camera"),
        )
        .unwrap();

    let refusal = runtime.load_graph_snapshot(&the_spec_in(serde_json::json!({
        "nodes": [
            {"name": "other", "type": camera.as_str()},
            {"name": "Camera", "type": camera.as_str()}
        ]
    })));

    assert!(
        matches!(refusal, Err(Error::NodeNameTaken { .. })),
        "{refusal:?}"
    );
    assert_eq!(
        the_graph_document_of(&runtime)["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["camera"]
    );
}

/// A link naming a port its node does not have, in that direction, is
/// refused before any node is added.
#[test]
#[serial]
fn a_load_refused_for_a_missing_link_port_adds_none_of_its_nodes() {
    let camera = register_test_type("MissingPortCamera", "frames_in", "video");

    let runtime = Runner::new().unwrap();
    let refusal = runtime
        .load_graph_snapshot(&the_spec_in(serde_json::json!({
            "nodes": [
                {"name": "front", "type": camera.as_str()},
                {"name": "back", "type": camera.as_str()}
            ],
            "links": [{"source": {"node": "front", "port": "video"},
                       "target": {"node": "back", "port": "no_such_input"}}]
        })))
        .expect_err("the link names an input `back` does not have")
        .to_string();

    assert!(
        refusal.contains("no input port `no_such_input`"),
        "{refusal}"
    );
    assert_eq!(
        the_graph_document_of(&runtime)["nodes"],
        serde_json::json!([])
    );
}

/// An end addressed under this runtime's own name is a local reference: a
/// target so named wires here, and one naming a node nobody holds is refused
/// before anything is added.
#[test]
#[serial]
fn an_end_naming_this_runtime_is_a_local_reference() {
    let camera = register_test_type("SelfAddressedCamera", "frames_in", "video");

    let runtime = Runner::new().unwrap();
    let this_runtimes_name = the_graph_document_of(&runtime)["mesh"]["runtime_name"]
        .as_str()
        .unwrap()
        .to_string();

    let refusal = runtime
        .load_graph_snapshot(&the_spec_in(serde_json::json!({
            "nodes": [{"name": "front", "type": camera.as_str()}],
            "links": [{"source": {"node": "front", "port": "video"},
                       "target": {"runtime_name": this_runtimes_name, "node": "nobody", "port": "frames_in"}}]
        })))
        .expect_err("no node here is named `nobody`")
        .to_string();
    assert!(refusal.contains("nobody"), "{refusal}");
    assert_eq!(
        the_graph_document_of(&runtime)["nodes"],
        serde_json::json!([])
    );

    runtime
        .load_graph_snapshot(&the_spec_in(serde_json::json!({
            "nodes": [{"name": "front", "type": camera.as_str()},
                      {"name": "back", "type": camera.as_str()}],
            "links": [{"source": {"node": "front", "port": "video"},
                       "target": {"runtime_name": this_runtimes_name, "node": "Back", "port": "frames_in"}}]
        })))
        .expect("a target naming this runtime wires here");
    assert_eq!(
        the_graph_document_of(&runtime)["links"][0]["target"],
        serde_json::json!({"node": "back", "port": "frames_in"})
    );
}

/// A link whose input is on another runtime has nowhere to be applied, so the
/// load is refused naming that runtime before any node is added.
#[test]
#[serial]
fn a_link_whose_target_names_another_runtime_is_refused_by_name() {
    let camera = register_test_type("OtherRuntimeTargetCamera", "frames_in", "video");

    let runtime = Runner::new().unwrap();
    let refusal = runtime
        .load_graph_snapshot(&the_spec_in(serde_json::json!({
            "nodes": [{"name": "front", "type": camera.as_str()}],
            "links": [{"source": {"node": "front", "port": "video"},
                       "target": {"runtime_name": "studio-display-9f3c", "node": "back", "port": "frames_in"}}]
        })))
        .expect_err("a link's input is on the runtime that loads it")
        .to_string();

    assert!(refusal.contains("studio-display-9f3c"), "{refusal}");
    assert_eq!(
        the_graph_document_of(&runtime)["nodes"],
        serde_json::json!([])
    );
}

#[test]
#[serial]
fn an_exposure_named_twice_is_refused() {
    let camera = register_test_type("TwiceExposedCamera", "_unused_in", "video");

    let refusal = Runner::new()
        .unwrap()
        .load_graph_snapshot(&the_spec_in(serde_json::json!({
            "nodes": [{"name": "camera", "type": camera.as_str()}],
            "exposed": [{"node": "camera", "port": "video"}, {"node": "Camera", "port": "Video"}]
        })))
        .expect_err("one port is exposed once")
        .to_string();

    assert!(refusal.contains("twice"), "{refusal}");
}

/// A link from a port on another runtime renders `{runtime_name, node, port}`
/// and loads back as that same remote end, waiting on its runtime.
#[test]
#[serial]
fn a_link_from_another_runtime_loads_back_as_the_same_remote_end() {
    let display = register_test_type("RemoteFedDisplay", "video_in", "_unused_out");

    let first = Runner::new().unwrap();
    let display_id = first
        .add_processor(ProcessorSpec::new(display, serde_json::json!({})))
        .unwrap();
    first
        .connect(
            OutputLinkPortRef::on_another_runtime(
                MeshPortAddress::new("bench-cam-a1b2", "Camera Source", "Video").unwrap(),
            ),
            InputLinkPortRef::new(&display_id, "video_in"),
        )
        .unwrap();
    let rendered_first = the_graph_document_of(&first);
    assert_eq!(
        rendered_first["links"][0]["source"],
        serde_json::json!({"runtime_name": "bench-cam-a1b2", "node": "camera-source", "port": "video"})
    );

    let second = Runner::new().unwrap();
    second
        .load_graph_snapshot(&the_spec_in(rendered_first.clone()))
        .expect("a graph with a remote end loads");

    assert_eq!(
        the_spec_in(the_graph_document_of(&second)),
        the_spec_in(rendered_first)
    );
}

/// A node whose type never resolved renders under the requested import path
/// verbatim, and its render cannot be loaded — `validate` resolves every type.
#[test]
#[serial]
fn an_unresolved_node_renders_under_the_requested_path_and_refuses_to_load() {
    const UNRESOLVED: &str = "my_app.filters:NeverRegistered";

    let runtime = Runner::new().unwrap();
    let _ = runtime.add_processor(ProcessorSpec::new(
        ProcessorClassImportPath::new(UNRESOLVED).unwrap(),
        serde_json::json!({}),
    ));
    let rendered = the_graph_document_of(&runtime);
    assert_eq!(rendered["nodes"][0]["type"], UNRESOLVED);

    match Runner::new()
        .unwrap()
        .load_graph_snapshot(&the_spec_in(rendered))
    {
        Err(Error::UnknownProcessorType { ident }) => {
            assert_eq!(ident.as_str(), UNRESOLVED);
        }
        other => panic!("expected the load to refuse the unresolved type, got {other:?}"),
    }
}
