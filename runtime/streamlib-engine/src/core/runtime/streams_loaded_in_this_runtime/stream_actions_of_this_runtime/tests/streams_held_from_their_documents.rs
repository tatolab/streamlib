// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[serial]
fn a_replace_whose_node_code_does_not_describe_a_type_holds_the_stream_failed_from_its_document() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_naming_a_type_not_described("camera"),
    );
    let camera = project.stream_id_of("camera");
    let previous_record = a_record_of(&project, "camera");
    let previous = a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &previous_record,
    );

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("a type not described is a failed stream, not a refused run");

    let (stream, failed_because) = the_one_failed_stream_of(&run);
    assert!(
        failed_because.contains("AbsentNodeType"),
        "{failed_because}"
    );
    assert_eq!(stream.stream_id, camera);
    assert!(stream.replaced_a_recorded_stream);
    assert!(
        previous.has_ended(),
        "the replace unloaded the previous stream"
    );
    let held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(held.is_held_stopped() && held.runtime_context.lock().is_none());
    assert_eq!(held.node_count(), 2, "every node of its document is held");
    assert!(
        held.node_types_this_stream_resolves()
            .placeholder_of_the_node_type_not_described(
                &NodeClassImportPath::new("tatolab.absent:AbsentNodeType").unwrap()
            )
            .is_some()
    );
    assert_eq!(
        the_record_of(state_directory.path(), &camera)
            .failed_because
            .as_deref(),
        Some(failed_because)
    );
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == camera)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
    assert!(
        listed
            .failed_because
            .as_deref()
            .is_some_and(|why| why.contains("`absent`")),
        "{listed:?}"
    );
}

#[test]
#[serial]
fn a_link_between_two_nodes_of_types_not_described_is_held_with_its_exposed_field() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let mut graph = the_graph_of_a_stream_naming_a_type_not_described("camera");
    let mut second_absent = graph["nodes"][1].clone();
    second_absent["name"] = serde_json::json!("second_absent");
    second_absent["type"] = serde_json::json!("tatolab.absent:SecondAbsentNodeType");
    second_absent["exposed_config_fields"] = serde_json::json!(["strength"]);
    graph["nodes"].as_array_mut().unwrap().push(second_absent);
    graph["links"] = serde_json::json!([
        {
            "source": { "node": "source", "port": "out1" },
            "target": { "node": "absent", "port": "video" },
        },
        {
            "source": { "node": "absent", "port": "result" },
            "target": { "node": "second_absent", "port": "video" },
        },
    ]);
    let project = ProjectWithGraphDocuments::holding(graph);
    let camera = project.stream_id_of("camera");

    a_project_opened_on(&runner, &project);

    let held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(held.is_held_stopped());
    assert_eq!(held.node_count(), 3);
    assert_eq!(
        held.to_json().unwrap()["links"].as_array().unwrap().len(),
        2
    );
    assert!(the_record_of(state_directory.path(), &camera).is_failed());
}

/// A node of a type its node code did not describe renders the value on each input apart from
/// its config as it did before a node held one map of input values: an exposed config field is
/// config, and every other value is on an input the placeholder gives it.
#[test]
#[serial]
fn a_node_of_a_type_not_described_renders_each_value_not_known_as_config_on_an_input() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let mut graph = the_graph_of_a_stream_naming_a_type_not_described("camera");
    graph["nodes"][1]["exposed_config_fields"] = serde_json::json!(["strength"]);
    graph["nodes"][1]["pins"] = serde_json::json!({"gain": 2});
    let project = ProjectWithGraphDocuments::holding(graph);
    let camera = project.stream_id_of("camera");

    a_project_opened_on(&runner, &project);

    let render = runner
        .loaded_stream_of_the_id(&camera)
        .unwrap()
        .to_json()
        .unwrap();
    let absent = render["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "absent")
        .expect("the node of the type not described renders");
    assert_eq!(absent["config"], serde_json::json!({"strength": 3}));
    assert_eq!(absent["pins"], serde_json::json!({"gain": 2}));
    let inputs: Vec<&str> = absent["ports"]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|port| port["name"].as_str())
        .collect();
    assert_eq!(inputs, ["gain", "strength"]);
}

#[test]
#[serial]
fn a_play_of_a_stream_held_from_its_document_is_refused_and_keeps_it_held() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_naming_a_type_not_described("camera"),
    );
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);
    let held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(held.is_held_stopped());

    let queued = runner.start_playback(project.path()).unwrap();
    let played = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(
        played
            .streams
            .iter()
            .map(|stream| (stream.stream_id.as_str(), stream.state))
            .collect::<Vec<_>>(),
        [(camera.as_str(), Some(StreamListingState::Failed))],
        "{played:?}"
    );
    let still_held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(Arc::ptr_eq(&held, &still_held), "the stream stays held");
    assert!(still_held.is_held_stopped() && still_held.runtime_context.lock().is_none());
    assert!(the_record_of(state_directory.path(), &camera).is_failed());
}

#[test]
#[serial]
fn a_play_queued_behind_the_open_that_loads_the_stream_names_the_stream_and_how_it_ended() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_naming_a_type_not_described("camera"),
    );
    let camera = project.stream_id_of("camera");
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let open = runner
        .open_project(project_open_or_reload_request_of(project.path()))
        .unwrap();
    let play = runner.start_playback(project.path()).unwrap();
    assert!(runner.loaded_stream_of_the_id(&camera).is_err());
    std::fs::write(&gate, "").unwrap();

    let opened = the_stream_action_once_finished(&runner, &open.action_id);
    let played = the_stream_action_once_finished(&runner, &play.action_id);

    assert_eq!(opened.state, StreamActionState::Done, "{opened:?}");
    assert_eq!(played.state, StreamActionState::Done, "{played:?}");
    assert_eq!(
        played
            .streams
            .iter()
            .map(|stream| (stream.stream_id.as_str(), stream.state))
            .collect::<Vec<_>>(),
        [(camera.as_str(), Some(StreamListingState::Failed))],
        "{played:?}"
    );
}

/// The graph of a stream named `stream_name`: a source linked into a sink, and with
/// `holding_a_link_into_a_port_the_sink_has_not` a second link into an input the sink does
/// not have, each node by a UUID of its own.
fn the_graph_of_a_source_linked_into_a_sink(
    stream_name: &str,
    holding_a_link_into_a_port_the_sink_has_not: bool,
) -> serde_json::Value {
    let mut links = vec![serde_json::json!({
        "source": {"node": "source", "port": "out1"},
        "target": {"node": "sink", "port": "in1"},
    })];
    if holding_a_link_into_a_port_the_sink_has_not {
        links.push(serde_json::json!({
            "source": {"node": "source", "port": "out2"},
            "target": {"node": "sink", "port": "no_such_input"},
        }));
    }
    serde_json::json!({
        "stream": stream_name,
        "nodes": [{
            "id": "7d3f1c1e-0a8b-4f3e-9a51-6c2b8f0e4a11",
            "name": "source",
            "type": MockOutputOnlyNode::Node::node_class_import_path().as_str(),
            "config": {},
        }, {
            "id": "1b9e6a52-3c4d-4e8f-8b07-2d5a9c1f7e33",
            "name": "sink",
            "type": crate::core::test_support::MockInputOnlyNode::Node::node_class_import_path().as_str(),
            "config": {},
        }],
        "links": links,
    })
}

#[test]
#[serial]
fn a_stream_whose_graph_holds_a_link_its_validation_refuses_is_held_with_that_link_shown_failed() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_source_linked_into_a_sink(
        "camera", true,
    ));
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);

    let held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(held.is_held_stopped());
    assert_eq!(held.node_count(), 2);
    let graph = held.to_json().unwrap();
    let links = graph["links"].as_array().unwrap();
    assert_eq!(links.len(), 2, "{graph}");
    let set_aside = links
        .iter()
        .find(|link| link["target"]["port"] == "no_such_input")
        .unwrap_or_else(|| panic!("the refused link is shown: {graph}"));
    assert_eq!(set_aside["state"], "error", "{graph}");
    assert!(
        set_aside["error_reason"]
            .as_str()
            .is_some_and(|why| why.contains("no_such_input")),
        "{graph}"
    );
    let why_it_cannot_play = held
        .why_its_graph_as_held_keeps_this_stream_from_playing()
        .expect("a link set aside keeps it from playing");
    assert!(
        why_it_cannot_play.contains("`source/out2` -> `sink/no_such_input` is refused"),
        "{why_it_cannot_play}"
    );
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == camera)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
    assert_eq!(
        listed.failed_because.as_deref(),
        Some(why_it_cannot_play.as_str())
    );

    let queued = runner.start_playback(project.path()).unwrap();
    let played = the_stream_action_once_finished(&runner, &queued.action_id);
    assert_eq!(
        played
            .streams
            .iter()
            .map(|stream| (stream.stream_id.as_str(), stream.state))
            .collect::<Vec<_>>(),
        [(camera.as_str(), Some(StreamListingState::Failed))],
        "{played:?}"
    );
    assert!(
        runner
            .loaded_stream_of_the_id(&camera)
            .unwrap()
            .is_held_stopped()
    );
}

#[test]
#[serial]
fn an_edit_of_a_stream_holding_a_link_set_aside_keeps_it_and_refuses_a_new_link_its_validation_refuses()
 {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_source_linked_into_a_sink(
        "camera", true,
    ));
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);
    let held = runner.loaded_stream_of_the_id(&camera).unwrap();

    let mut renamed_sink = the_graph_of_a_source_linked_into_a_sink("camera", true);
    renamed_sink["nodes"][1]["name"] = serde_json::json!("drain");
    for link in renamed_sink["links"].as_array_mut().unwrap() {
        link["target"]["node"] = serde_json::json!("drain");
    }
    held.reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
        renamed_sink,
        LinksAReconcileMakes::ThoseTheLiveGraphKeptThoughConnectRefusesThem,
    )
    .expect("an edit keeps the link already set aside");
    assert!(
        held.why_its_graph_as_held_keeps_this_stream_from_playing()
            .is_some()
    );

    let mut a_second_refused_link = the_graph_of_a_source_linked_into_a_sink("camera", true);
    a_second_refused_link["links"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "source": {"node": "source", "port": "out1"},
            "target": {"node": "sink", "port": "another_missing_input"},
        }));
    let refusal = held
        .reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
            a_second_refused_link,
            LinksAReconcileMakes::ThoseTheLiveGraphKeptThoughConnectRefusesThem,
        )
        .expect_err("a new link its validation refuses is refused")
        .to_string();
    assert!(refusal.contains("another_missing_input"), "{refusal}");

    held.reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
        the_graph_of_a_source_linked_into_a_sink("camera", false),
        LinksAReconcileMakes::ThoseTheLiveGraphKeptThoughConnectRefusesThem,
    )
    .expect("taking the refused link out is an edit");
    assert_eq!(
        held.why_its_graph_as_held_keeps_this_stream_from_playing(),
        None
    );
    assert_eq!(
        held.to_json().unwrap()["links"].as_array().unwrap().len(),
        1
    );
}

#[test]
#[serial]
fn a_play_whose_start_is_refused_keeps_the_stream_held_stopped_and_records_it_failed() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);
    let held = runner.loaded_stream_of_the_id(&camera).unwrap();
    held.node_types_this_stream_resolves()
        .register_a_type_described_in_this_streams_node_interpreter(
            crate::core::compiler::compiler_ops::function_node_island::tests::a_function_node_type(
                "test_ops:remap",
                &["x"],
                &["result"],
            ),
            crate::core::nodes::HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no node in this test",
                        node.id
                    )))
                }),
            },
        )
        .unwrap();
    held.add_node(crate::core::nodes::NodeSpec::new(
        NodeClassImportPath::new("test_ops:remap").unwrap(),
        serde_json::Value::Null,
    ))
    .unwrap();

    let queued = runner.start_playback(project.path()).unwrap();
    let played = the_stream_action_once_finished(&runner, &queued.action_id);

    assert_eq!(
        played
            .streams
            .iter()
            .map(|stream| (stream.stream_id.as_str(), stream.state))
            .collect::<Vec<_>>(),
        [(camera.as_str(), Some(StreamListingState::Failed))],
        "{played:?}"
    );
    let still_held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(Arc::ptr_eq(&held, &still_held), "the stream stays held");
    assert!(still_held.is_held_stopped() && still_held.runtime_context.lock().is_none());
    assert_eq!(still_held.node_count(), 2);
    let record = the_record_of(state_directory.path(), &camera);
    assert!(record.stopped, "{record:?}");
    assert!(
        record
            .failed_because
            .as_deref()
            .is_some_and(|why| why.contains("no value reaches")),
        "{record:?}"
    );
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == camera)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
}

#[test]
#[serial]
fn a_reload_once_its_document_names_only_described_types_loads_the_failed_stream_again() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_naming_a_type_not_described("camera"),
    );
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);
    let held = runner.loaded_stream_of_the_id(&camera).unwrap();

    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    the_project_reloaded(&runner, &project);

    let loaded_again = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(!Arc::ptr_eq(&held, &loaded_again));
    assert_eq!(
        loaded_again.why_its_graph_as_held_keeps_this_stream_from_playing(),
        None
    );
    assert!(loaded_again.is_held_stopped());
    assert!(!the_record_of(state_directory.path(), &camera).is_failed());
}

/// A recorded stream whose document links a node into the `value` of its top-level `GraphInput`,
/// a setting, is held with that link set aside, naming the setting, and the rest of its graph
/// loaded.
///
/// Mental-revert: without the setting rule in the document's validation, the link is first
/// refused at its connect, after the load added the graph's nodes, and the stream is not held.
#[test]
#[serial]
fn a_recorded_stream_linking_into_a_graph_inputs_value_is_held_with_that_link_set_aside() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let mut graph = the_graph_of_a_source_linked_into_a_sink("camera", false);
    graph["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "5e8c2b7a-1d4f-4c6e-9a3b-8f2d6c1e7a54",
            "name": "echo",
            "type": crate::core::graph::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE,
            "config": {"port": "echo", "family": "records"},
        }));
    graph["links"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "source": {"node": "source", "port": "out1"},
            "target": {"node": "echo", "port": "value"},
        }));
    let project = ProjectWithGraphDocuments::holding(graph);
    let camera = project.stream_id_of("camera");
    a_project_opened_on(&runner, &project);

    let held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(held.is_held_stopped());
    assert_eq!(held.node_count(), 3);
    let why_it_cannot_play = held
        .why_its_graph_as_held_keeps_this_stream_from_playing()
        .expect("a link set aside keeps it from playing");
    assert!(
        why_it_cannot_play.contains("input `value` of `echo`, a setting, which takes no wire"),
        "{why_it_cannot_play}"
    );
    let rendered = held.to_json().unwrap();
    let links_loaded: Vec<&serde_json::Value> = rendered["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|link| link["state"] != "error")
        .collect();
    assert_eq!(links_loaded.len(), 1, "{rendered}");
    assert_eq!(links_loaded[0]["target"]["node"], "sink", "{rendered}");
}

#[test]
#[serial]
fn a_stream_whose_load_is_refused_is_held_from_its_document_reading_none_of_its_node_code() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let mut graph = the_graph_of_a_stream_named("camera");
    graph["nodes"] = serde_json::json!([{
        "name": "source",
        "type": crate::core::test_support::MockSourceTakingOneSetting::Node::node_class_import_path().as_str(),
        "config": {"frame_width": "wide"},
    }]);
    let project = ProjectWithGraphDocuments::holding(graph);
    let camera = project.stream_id_of("camera");

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("a refused load is a failed stream, not a refused run");

    let (stream, failed_because) = the_one_failed_stream_of(&run);
    assert_eq!(stream.stream_id, camera);
    assert!(failed_because.contains("frame_width"), "{failed_because}");
    let held = runner
        .loaded_stream_of_the_id(&camera)
        .expect("the stream is held from its document, its graph there to read and edit");
    assert!(held.is_held_stopped() && held.runtime_context.lock().is_none());
    assert_eq!(held.node_count(), 1, "every node of its document is held");
    assert!(
        held.node_types_this_stream_resolves()
            .why_every_node_type_is_held_as_a_placeholder()
            .is_some_and(|why| why == failed_because),
        "every node type is held as a placeholder, for why the load was refused"
    );
    assert_eq!(
        the_record_of(state_directory.path(), &camera)
            .failed_because
            .as_deref(),
        Some(failed_because)
    );
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == camera)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
    assert_eq!(listed.node_count, Some(1), "{listed:?}");
}
