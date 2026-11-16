// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A graph document saved from a running stream loading back, the names and
//! clocks a load keys and renders, and the loads it refuses.

use super::*;

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
    assert_eq!(
        rendered_second["nodes"][0]["id"], rendered_first["nodes"][0]["id"],
        "a node keeps its UUID through a load"
    );
    assert_eq!(
        rendered_second["stream"], "built-in-code",
        "a stream built in code renders its own name, and a load of it keeps that name"
    );
}

#[test]
fn a_graph_carrying_exposed_is_refused_naming_the_keys_a_graph_has() {
    let refusal = GraphSnapshot::from_graph_document(serde_json::json!({
        "stream": "main",
        "nodes": [{"name": "camera", "type": "tatolab.testpattern:TestPatternSource", "config": {}}],
        "exposed": [{"node": "camera", "port": "video"}]
    }))
    .expect_err("a graph carries no exposure")
    .to_string();

    assert!(refusal.contains("the key `exposed`"), "{refusal}");
    assert!(
        refusal.contains(
            "`asset`, `display_name`, `stream`, `clock`, `nodes`, `links`, `groups` and `studio`"
        ),
        "{refusal}"
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
            in_the_project(first_project_directory.path(), "main"),
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
            in_the_project(second_project_directory.path(), "Main"),
        ),
        "a name already loaded is refused",
    )
    .to_string();

    assert!(refusal.contains("`main`"), "{refusal}");
    assert!(
        refusal.contains(&first_project_directory.path().display().to_string()),
        "the refusal must name where the first stream came from: {refusal}"
    );
    assert!(refusal.contains("at_stream_location"), "{refusal}");
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
            in_the_project(second_project_directory.path(), "second"),
        )
        .expect("the same graph loads under another name");
    assert_eq!(renamed.stream_id(), "second");
    assert_eq!(runner.ids_of_the_loaded_streams(), ["main", "second"]);
}

/// A graph holding no node loads as an empty stream, so a stream just created
/// can be targeted by the graph tools that fill it.
#[test]
#[serial]
fn a_graph_holding_no_node_loads_as_an_empty_stream() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();

    let empty_stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({"stream": "main", "nodes": []}),
    )
    .expect("an empty graph loads");
    assert_eq!(empty_stream.node_count(), 0);

    runner
        .load_stream_from_graph_snapshot(
            &the_spec_in(serde_json::json!({"nodes": []})),
            in_the_project(project_directory.path(), "named-by-the-load"),
        )
        .expect("an empty graph the load names loads");

    let mut loaded_stream_ids = runner.ids_of_the_loaded_streams();
    loaded_stream_ids.sort_unstable();
    assert_eq!(loaded_stream_ids, ["main", "named-by-the-load"]);
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
