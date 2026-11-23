// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A graph generator an import defines runs as a fragment, seeds a new stream, and reruns when its
//! module changes; an import under the name of a node that is no fragment is refused; and a link to
//! a fragment's port feeds the nodes its input feeds.

use super::*;

/// A venv interpreter that answers the runtime's probe and, run as the graph generator run
/// entry, prints the project's `generator-run.json` as the run's document.
const STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_GENERATOR_RUN: &str = "#!/bin/sh\ncase \"$*\" in\n  *tatolab.stream._graph_generator_run_entry*) cat generator-run.json ;;\nesac\nexit 0\n";

/// The import path of the graph generator the stub interpreter's runs answer for.
const LOOK_GENERATOR: &str = "generators.look:look";

impl ProjectWithGraphDocuments {
    /// Give this project the interpreter of [`STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_GENERATOR_RUN`].
    fn with_a_generator_run_printed_from_generator_run_json(self) -> Self {
        write_an_executable_script_from_a_child_process(
            &self.interpreter(),
            STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_GENERATOR_RUN,
        );
        self
    }

    /// Make the next run of [`LOOK_GENERATOR`] place `fragment` as `look`.
    fn next_look_run_places(&self, fragment: Value) {
        std::fs::write(
            self.path().join("generator-run.json"),
            json!({
                "generator": LOOK_GENERATOR,
                "placed": { "name": "look", "type": LOOK_GENERATOR, "fragment": fragment },
            })
            .to_string(),
        )
        .expect("the generator run document");
    }

    /// Make the next run of [`LOOK_GENERATOR`] fail with `traceback`.
    fn next_look_run_fails_with(&self, traceback: &str) {
        std::fs::write(
            self.path().join("generator-run.json"),
            json!({ "generator": LOOK_GENERATOR, "failed_because": traceback }).to_string(),
        )
        .expect("the generator run document");
    }
}

/// A fragment of a source linked into a sink, and a spare sink when `with_a_spare` says so.
fn a_source_into_a_sink_fragment(with_a_spare: bool) -> Value {
    let source_type = StreamToolsTestSource::node_class_import_path();
    let sink_type = StreamToolsTestSink::node_class_import_path();
    let mut nodes = vec![
        json!({ "name": "pass-source", "type": source_type.as_str() }),
        json!({ "name": "pass-sink", "type": sink_type.as_str() }),
    ];
    if with_a_spare {
        nodes.push(json!({ "name": "pass-spare", "type": sink_type.as_str() }));
    }
    json!({
        "nodes": nodes,
        "links": [{
            "source": { "node": "pass-source", "port": "video" },
            "target": { "node": "pass-sink", "port": "video" },
        }],
    })
}

/// The UUID of each node of the fragment node `look` in `graph_document`, by name.
fn the_uuid_of_each_node_of_look_in(
    graph_document: &Value,
) -> std::collections::BTreeMap<String, String> {
    graph_document["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["name"] == "look")
        .unwrap_or_else(|| panic!("the graph holds `look`: {graph_document}"))["fragment"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|node| {
            (
                node["name"].as_str().unwrap_or_default().to_owned(),
                node["id"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// `import_graph_generator` places a generator's run as a fragment the running stream runs;
/// run again it refreshes that fragment, each node keeping its UUID by name, and one undo
/// brings the fragment back as it was.
#[tokio::test(flavor = "multi_thread")]
async fn an_imported_graph_generator_runs_as_a_fragment_and_a_rerun_keeps_each_nodes_uuid() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let import_the_look = || {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        let arguments = json!({
            "stream": stream_asset, "generator": LOOK_GENERATOR, "author": an_agent_named("alice"),
        });
        async move {
            tool_answer(
                &tool_call_result(
                    operations_on_the_loaded_streams,
                    "import_graph_generator",
                    arguments,
                )
                .await,
            )
        }
    };

    project.next_look_run_places(a_source_into_a_sink_fragment(false));
    let placed = import_the_look().await.expect("the import applies");
    assert_eq!(placed["added_nodes"][0]["name"], "look", "{placed}");
    let first_run = project.the_graph_document_of("main");
    let first_uuids = the_uuid_of_each_node_of_look_in(&first_run);
    assert_eq!(
        first_uuids.keys().collect::<Vec<_>>(),
        ["pass-sink", "pass-source"],
        "{first_run}"
    );
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["look", "source"],
        "the stream runs the fragment drawn as one node"
    );

    project.next_look_run_places(a_source_into_a_sink_fragment(true));
    let refreshed = import_the_look().await.expect("the rerun applies");
    assert_eq!(
        refreshed["operations"][0]["operation"], "refresh_fragment",
        "{refreshed}"
    );
    let second_run = project.the_graph_document_of("main");
    let second_uuids = the_uuid_of_each_node_of_look_in(&second_run);
    assert_eq!(second_uuids["pass-source"], first_uuids["pass-source"]);
    assert_eq!(second_uuids["pass-sink"], first_uuids["pass-sink"]);
    assert!(second_uuids.contains_key("pass-spare"), "{second_run}");
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&live),
        second_uuids,
        "the running stream runs the refreshed fragment: {live}"
    );

    project.next_look_run_fails_with("Traceback\nRuntimeError: the rig list is empty");
    let refusal = import_the_look()
        .await
        .expect_err("a failed run is refused");
    assert!(
        refusal.contains("RuntimeError: the rig list is empty"),
        "{refusal}"
    );
    assert_eq!(project.the_graph_document_of("main"), second_run);

    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the undo applies");
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&project.the_graph_document_of("main")),
        first_uuids
    );
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&engine.the_live_graph_of(&stream_id)),
        first_uuids,
        "the undo brings the running fragment back as it was"
    );
}

/// A stream `create_stream` just made loads holding no node, and `import_graph_generator`
/// fills it as its first content.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_generator_seeds_a_stream_create_stream_just_made() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };

    let created = call(
        "create_stream",
        json!({
            "project_directory": project.path(),
            "path": "streams/looks.graph.json",
            "display_name": "Looks",
        }),
    )
    .await
    .expect("the stream is created");
    let load_of_the_created_stream = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        created["action"]
            .as_str()
            .expect("the creation queues a load"),
    )
    .await;
    assert_eq!(
        load_of_the_created_stream["state"], "done",
        "the empty stream loads: {load_of_the_created_stream}"
    );

    project.next_look_run_places(a_source_into_a_sink_fragment(false));
    let placed = call(
        "import_graph_generator",
        json!({ "stream": created["asset"]["asset"], "generator": LOOK_GENERATOR }),
    )
    .await
    .expect("the generator seeds the new stream");
    assert_eq!(placed["added_nodes"][0]["name"], "look", "{placed}");
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&project.the_graph_document_of("looks"))
            .keys()
            .collect::<Vec<_>>(),
        ["pass-sink", "pass-source"]
    );
}

/// An import under the name of a node that is no fragment is refused naming it, nothing changed.
#[tokio::test(flavor = "multi_thread")]
async fn an_import_under_the_name_of_a_node_that_is_no_fragment_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    std::fs::write(
        project.path().join("generator-run.json"),
        json!({
            "generator": LOOK_GENERATOR,
            "placed": { "name": "source", "type": LOOK_GENERATOR,
                        "fragment": a_source_into_a_sink_fragment(false) },
        })
        .to_string(),
    )
    .expect("the generator run document");
    let before = project.the_graph_document_of("main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "import_graph_generator",
            json!({ "stream": stream_asset, "generator": LOOK_GENERATOR, "name": "source" }),
        )
        .await,
    )
    .expect_err("refused");

    assert!(
        refusal.contains(&format!("node {SOURCE_NODE_UUID} `source`"))
            && refusal.contains("no fragment"),
        "{refusal}"
    );
    assert_eq!(project.the_graph_document_of("main"), before);
}

/// `refresh_fragments` re-runs each fragment whose generator a changed module defines, keeping
/// each node's UUID by name, and answers no operation for a change no fragment's generator is in.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_fragments_reruns_the_fragments_a_changed_module_defines() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    project.next_look_run_places(a_source_into_a_sink_fragment(false));
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "import_graph_generator",
            json!({ "stream": stream_asset, "generator": LOOK_GENERATOR }),
        )
        .await,
    )
    .expect("the import applies");
    let first_uuids = the_uuid_of_each_node_of_look_in(&project.the_graph_document_of("main"));
    let refresh_after_a_change_to = |changed_file: &'static str| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        let arguments = json!({
            "project_directory": project.path(), "changed_files": [changed_file],
        });
        async move {
            tool_answer(
                &tool_call_result(
                    operations_on_the_loaded_streams,
                    "refresh_fragments",
                    arguments,
                )
                .await,
            )
            .expect("the refresh applies")
        }
    };

    let untouched = refresh_after_a_change_to("nodes/blur.py").await;
    assert_eq!(untouched["operations"], json!([]), "{untouched}");
    assert_eq!(untouched["transaction"], json!(null), "{untouched}");

    project.next_look_run_places(a_source_into_a_sink_fragment(true));
    let refreshed = refresh_after_a_change_to("generators/look.py").await;

    assert_eq!(
        refreshed["operations"][0]["operation"], "refresh_fragment",
        "{refreshed}"
    );
    let refreshed_uuids = the_uuid_of_each_node_of_look_in(&project.the_graph_document_of("main"));
    assert_eq!(refreshed_uuids["pass-source"], first_uuids["pass-source"]);
    assert!(refreshed_uuids.contains_key("pass-spare"));
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&engine.the_live_graph_of(&stream_id)),
        refreshed_uuids
    );
}

/// A `connect` to a port a fragment's `GraphInput` declares links the running stream to each
/// node that input feeds, and `graph` draws the link to the fragment.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_connected_to_a_fragments_port_feeds_the_nodes_its_input_feeds() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::node_class_import_path();
    project.next_look_run_places(json!({
        "nodes": [
            { "name": "graphinput", "type": "tatolab.stream:GraphInput",
              "input_values": { "port": "video", "family": "image" } },
            { "name": "pass-sink", "type": sink_type.as_str() },
        ],
        "links": [{
            "source": { "node": "graphinput", "port": "value" },
            "target": { "node": "pass-sink", "port": "video" },
        }],
    }));
    let placed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "import_graph_generator",
            json!({ "stream": stream_asset, "generator": LOOK_GENERATOR }),
        )
        .await,
    )
    .expect("the import applies");
    let look = placed["added_nodes"][0]["node"].clone();

    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "connect",
            json!({
                "stream": stream_asset,
                "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                "target": { "node": look, "port": "video" },
            }),
        )
        .await,
    )
    .expect("the connect applies");

    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        links_by_node_name_in(&live),
        ["source/video -> look/video"],
        "{live}"
    );
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("main")),
        ["source/video -> look/video"]
    );
}
