// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream's graph inputs: writes, defaults, and the value a kept or reset lifetime
//! carries across a stop and a play.

use super::*;

impl AStreamOfAProjectBringingViews {
    fn add_a_top_level_graph_input(&self, name: &str, config: Value) -> NodeRunId {
        self.stream
            .add_node(
                NodeSpec::new(
                    NodeClassImportPath::new(
                        crate::core::graph::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE,
                    )
                    .unwrap(),
                    config,
                )
                .with_display_name(name.to_string()),
            )
            .expect("a GraphInput is added")
    }

    fn write_the_input(&self, input: &str, value: Value) -> Result<PortFamily> {
        self.async_runtime.block_on(
            self.stream
                .write_a_value_to_a_top_level_graph_input(input.to_string(), value),
        )
    }

    fn with_the_graph_inputs_node<T>(
        &self,
        node_id: &NodeRunId,
        call: impl FnOnce(&mut crate::core::nodes::NodeWhoseOutputsAClientWrites) -> T,
    ) -> T {
        self.stream.compiler.scope(|graph, _tx| {
            let node = graph.traversal().v(node_id).first().unwrap();
            let mut node_instance = node
                .get::<crate::core::graph::NodeInstanceComponent>()
                .unwrap()
                .0
                .lock();
            call(
                node_instance
                    .as_any_mut()
                    .downcast_mut::<crate::core::nodes::NodeWhoseOutputsAClientWrites>()
                    .expect("a GraphInput runs as a node whose outputs a client writes"),
            )
        })
    }
}

#[test]
#[serial]
fn a_value_written_to_a_graph_input_twice_reaches_both_readers_and_the_graph_keeps_none_of_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    let cue_id =
        stream.add_a_top_level_graph_input("cue", json!({ "port": "cue", "family": "records" }));
    let cue_uuid = stream.run(&cue_id);
    let graph_before = stream.what_the_graph_holds_of(&cue_id);
    let (_left_viewer, mut left_reader) = stream.a_reader_of(&cue_uuid, "value");
    let (_right_viewer, mut right_reader) = stream.a_reader_of(&cue_uuid, "value");

    for record in [json!({ "track": "intro" }), json!({ "track": "drop" })] {
        assert_eq!(
            stream.write_the_input("cue", record.clone()).unwrap(),
            PortFamily::Records
        );
        for reader in [&mut left_reader, &mut right_reader] {
            let message = the_next_message_read(reader);
            let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
            assert_eq!(read_back, record);
        }
    }

    assert_eq!(
        stream.with_the_graph_inputs_node(&cue_id, |node| node
            .current_value_of_the_output("value")
            .cloned()),
        Some(json!({ "track": "drop" }))
    );
    assert_eq!(stream.what_the_graph_holds_of(&cue_id), graph_before);
}

#[test]
#[serial]
fn an_array_written_to_a_graph_input_reaches_its_reader_as_its_values_attribute() {
    let stream = AStreamOfAProjectBringingViews::load();
    let fader_id = stream.add_a_top_level_graph_input(
        "fader",
        json!({ "port": "Fader", "family": "array", "range": [0, 1] }),
    );
    let fader_uuid = stream.run(&fader_id);
    let (_viewer, mut reader) = stream.a_reader_of(&fader_uuid, "value");

    assert_eq!(
        stream.write_the_input("fader", json!(0.25)).unwrap(),
        PortFamily::Array
    );

    let message = the_next_message_read(&mut reader);
    assert_eq!(
        message.payload_descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::ArrayFrame)
    );
}

#[test]
#[serial]
fn a_graph_inputs_default_reaches_a_reader_its_last_send_missed() {
    let stream = AStreamOfAProjectBringingViews::load();
    let cue_id = stream.add_a_top_level_graph_input(
        "cue",
        json!({ "port": "cue", "family": "records", "default": { "track": "intro" } }),
    );
    let cue_uuid = stream.run(&cue_id);
    let (_viewer, mut reader) = stream.a_reader_of(&cue_uuid, "value");

    stream
        .with_the_graph_inputs_node(&cue_id, |node| {
            node.hand_each_current_value_to_a_reader_it_has_not_reached()
        })
        .expect("the default is published");

    let message = the_next_message_read(&mut reader);
    let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
    assert_eq!(read_back, json!({ "track": "intro" }));
}

#[test]
#[serial]
fn a_write_to_an_input_no_graph_input_declares_or_of_the_wrong_shape_is_refused_naming_why() {
    let stream = AStreamOfAProjectBringingViews::load();
    let cue_id =
        stream.add_a_top_level_graph_input("cue", json!({ "port": "cue", "family": "records" }));
    stream.run(&cue_id);

    let refusal = stream
        .write_the_input("fader", json!(1))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("`fader`") && refusal.contains("`cue`"),
        "{refusal}"
    );
    let refusal = stream
        .write_the_input("cue", json!("drop"))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("carries Records") && refusal.contains("JSON object"),
        "{refusal}"
    );
}

impl AStreamOfAProjectBringingViews {
    fn play_the_graph_input(
        &self,
        node_id: &NodeRunId,
        project_stores: &crate::core::context::ProjectStoresOpenInThisRuntime,
    ) -> String {
        let node_uuid = self.run(node_id);
        let project_store = project_stores
            .the_store_of_the_project_at(self._project_directory.path())
            .expect("the project's store opens");
        let store = crate::core::context::StoreScopedToThisNode::of_the_node(
            project_store,
            node_uuid.parse().expect("a node UUID"),
        );
        self.with_the_graph_inputs_node(node_id, |node| node.keep_the_graph_input_value_in(store));
        node_uuid
    }

    fn stop_the_graph_input(&self, node_id: &NodeRunId) {
        self.with_the_graph_inputs_node(node_id, |node| {
            node.commit_the_kept_value_as_the_node_stops()
        });
    }

    fn the_graph_inputs_current_value(&self, node_id: &NodeRunId) -> Option<Value> {
        self.with_the_graph_inputs_node(node_id, |node| {
            node.current_value_of_the_output("value").cloned()
        })
    }
}

#[test]
#[serial]
fn a_kept_graph_inputs_value_survives_a_stop_and_a_play_and_a_reopened_store() {
    let stream = AStreamOfAProjectBringingViews::load();
    let lights_id = stream.add_a_top_level_graph_input(
        "lights",
        json!({ "port": "lights", "family": "records", "default": { "level": 0.0 } }),
    );
    {
        let project_stores = crate::core::context::ProjectStoresOpenInThisRuntime::default();
        stream.play_the_graph_input(&lights_id, &project_stores);
        stream
            .write_the_input("lights", json!({ "level": 0.8, "scene": "warm" }))
            .unwrap();
        stream.stop_the_graph_input(&lights_id);

        stream.play_the_graph_input(&lights_id, &project_stores);
        assert_eq!(
            stream.the_graph_inputs_current_value(&lights_id),
            Some(json!({ "level": 0.8, "scene": "warm" }))
        );
        stream.stop_the_graph_input(&lights_id);
    }

    let project_stores_after_a_restart =
        crate::core::context::ProjectStoresOpenInThisRuntime::default();
    let lights_uuid = stream.play_the_graph_input(&lights_id, &project_stores_after_a_restart);
    let (_viewer, mut reader) = stream.a_reader_of(&lights_uuid, "value");
    stream
        .with_the_graph_inputs_node(&lights_id, |node| {
            node.hand_each_current_value_to_a_reader_it_has_not_reached()
        })
        .expect("the kept value is published");
    let read_back: Value =
        rmp_serde::from_slice(&the_next_message_read(&mut reader).payload).unwrap();
    assert_eq!(read_back, json!({ "level": 0.8, "scene": "warm" }));
}

#[test]
#[serial]
fn a_reset_graph_input_starts_from_its_default_after_a_stop() {
    let stream = AStreamOfAProjectBringingViews::load();
    let trigger_id = stream.add_a_top_level_graph_input(
        "trigger",
        json!({
            "port": "trigger", "family": "array", "default": 0, "lifetime": "reset"
        }),
    );
    let project_stores = crate::core::context::ProjectStoresOpenInThisRuntime::default();
    stream.play_the_graph_input(&trigger_id, &project_stores);
    stream.write_the_input("trigger", json!(1)).unwrap();
    stream.stop_the_graph_input(&trigger_id);

    stream.play_the_graph_input(&trigger_id, &project_stores);
    assert_eq!(
        stream.the_graph_inputs_current_value(&trigger_id),
        Some(json!(0))
    );
}

#[test]
#[serial]
fn a_kept_value_its_family_no_longer_takes_leaves_the_graph_input_at_its_default() {
    let stream = AStreamOfAProjectBringingViews::load();
    let fader_id = stream.add_a_top_level_graph_input(
        "fader",
        json!({ "port": "fader", "family": "array", "default": 0.5 }),
    );
    let project_stores = crate::core::context::ProjectStoresOpenInThisRuntime::default();
    let fader_uuid = stream.play_the_graph_input(&fader_id, &project_stores);
    project_stores
        .the_store_of_the_project_at(stream._project_directory.path())
        .unwrap()
        .write(
            streamlib_project_repository::ProjectStoreTableName::named("node_store"),
            &format!("{fader_uuid}/value"),
            &json!("not a number"),
        )
        .unwrap();
    stream.stop_the_graph_input(&fader_id);

    stream.play_the_graph_input(&fader_id, &project_stores);
    assert_eq!(
        stream.the_graph_inputs_current_value(&fader_id),
        Some(json!(0.5))
    );
    assert_eq!(
        project_stores
            .the_store_of_the_project_at(stream._project_directory.path())
            .unwrap()
            .read(
                streamlib_project_repository::ProjectStoreTableName::named("node_store"),
                &format!("{fader_uuid}/value"),
            )
            .unwrap(),
        None
    );
}

#[test]
#[serial]
fn a_running_graph_input_takes_a_new_lifetime_from_a_live_config_change() {
    use crate::core::nodes::DynGeneratedNode;

    let stream = AStreamOfAProjectBringingViews::load();
    let fader_id = stream.add_a_top_level_graph_input(
        "fader",
        json!({ "port": "fader", "family": "array", "default": 0.0, "lifetime": "reset" }),
    );
    let project_stores = crate::core::context::ProjectStoresOpenInThisRuntime::default();
    let fader_uuid = stream.play_the_graph_input(&fader_id, &project_stores);
    let the_value_the_store_keeps = || {
        project_stores
            .the_store_of_the_project_at(stream._project_directory.path())
            .unwrap()
            .read(
                streamlib_project_repository::ProjectStoreTableName::named("node_store"),
                &format!("{fader_uuid}/value"),
            )
            .unwrap()
    };
    stream.write_the_input("fader", json!(0.3)).unwrap();
    assert_eq!(the_value_the_store_keeps(), None);

    stream
        .with_the_graph_inputs_node(&fader_id, |node| {
            node.apply_config_json(
                &json!({ "port": "fader", "family": "array", "default": 0.0, "lifetime": "kept" }),
            )
        })
        .expect("the lifetime changes to kept");
    assert_eq!(the_value_the_store_keeps(), Some(json!(0.3)));
    stream.write_the_input("fader", json!(0.6)).unwrap();
    assert_eq!(the_value_the_store_keeps(), Some(json!(0.6)));

    stream
        .with_the_graph_inputs_node(&fader_id, |node| {
            node.apply_config_json(
                &json!({ "port": "fader", "family": "array", "default": 0.0, "lifetime": "reset" }),
            )
        })
        .expect("the lifetime changes to reset");
    assert_eq!(the_value_the_store_keeps(), None);
    stream.write_the_input("fader", json!(0.9)).unwrap();
    assert_eq!(the_value_the_store_keeps(), None);
}

#[test]
#[serial]
fn a_graph_input_declaring_a_lifetime_that_is_neither_kept_nor_reset_is_refused() {
    let refusal =
        crate::core::graph::graph_interface_resolution::the_value_lifetime_a_graph_input_config_declares(
            Some(&json!({ "lifetime": "forever" })),
        )
        .unwrap_err();
    assert!(
        refusal.contains("\"forever\"") && refusal.contains("\"kept\""),
        "{refusal}"
    );
}

/// A project `acme-show` on disk, its stream `a` loaded in a runner of its own.
fn a_runner_holding_the_stream_of_the_project_at(
    project_directory: &Path,
) -> (Arc<Runner>, Arc<LoadedStreamInThisRuntime>) {
    ensure_test_mocks_registered();
    let runner = Runner::new().expect("the engine builds");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "a",
        ))
        .expect("the stream loads");
    (runner, stream)
}

fn add_the_lights_input(stream: &LoadedStreamInThisRuntime, node_uuid: uuid::Uuid) {
    stream
        .add_node(
            NodeSpec::new(
                NodeClassImportPath::new(
                    crate::core::graph::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE,
                )
                .unwrap(),
                json!({ "port": "lights", "family": "records", "default": { "level": 0.0 } }),
            )
            .with_display_name("lights".to_string())
            .with_node_uuid(node_uuid),
        )
        .expect("a GraphInput is added");
}

fn play(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream plays");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the GraphInput runs");
}

fn the_value_a_new_reader_of_the_input_is_handed(
    stream: &LoadedStreamInThisRuntime,
    async_runtime: &tokio::runtime::Runtime,
    node_uuid: uuid::Uuid,
) -> Value {
    let viewer = async_runtime
        .block_on(stream.open_a_viewer_of_an_output_port(
            node_uuid.to_string(),
            "value".to_string(),
            None,
        ))
        .expect("a running GraphInput's value opens a viewer");
    let hand_off = viewer.hand_off().clone();
    let mut reader = OutputPortViewerReader::subscribe(
        &hand_off.iceoryx2_domain_root,
        &hand_off.channel_service_name,
        &hand_off.wake_service_name,
        "a-reader",
        hand_off.viewer_reads,
    )
    .expect("the reader subscribes to the port's channel");
    let read_back = rmp_serde::from_slice(&the_next_message_read(&mut reader).payload).unwrap();
    drop(viewer);
    read_back
}

#[test]
#[serial]
fn a_kept_graph_input_reaches_a_new_reader_with_its_value_after_a_real_stop_play_and_restart() {
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    std::fs::write(
        project_directory.path().join(PROJECT_FILE_NAME),
        TatolabProjectFile::of_a_new_project("acme-show")
            .canonical_bytes()
            .unwrap(),
    )
    .unwrap();
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let lights_uuid = uuid::Uuid::new_v4();
    let written = json!({ "level": 0.8, "scene": "warm" });
    {
        let (_runner, stream) =
            a_runner_holding_the_stream_of_the_project_at(project_directory.path());
        add_the_lights_input(&stream, lights_uuid);
        play(&stream);
        async_runtime
            .block_on(
                stream.write_a_value_to_a_top_level_graph_input(
                    "lights".to_string(),
                    written.clone(),
                ),
            )
            .expect("the Input is written");
        stream
            .stop_playback_keeping_the_graph()
            .expect("the stream stops");
        play(&stream);
        assert_eq!(
            the_value_a_new_reader_of_the_input_is_handed(&stream, &async_runtime, lights_uuid),
            written
        );
        stream.stop().expect("the stream ends");
    }
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    add_the_lights_input(&stream, lights_uuid);
    play(&stream);
    assert_eq!(
        the_value_a_new_reader_of_the_input_is_handed(&stream, &async_runtime, lights_uuid),
        written
    );
    stream.stop().expect("the stream ends");
}

fn the_current_value_graph_shows_for(
    stream: &LoadedStreamInThisRuntime,
    node_uuid: uuid::Uuid,
) -> Option<Value> {
    let stream_handles = stream.the_handles_a_runtime_context_of_this_stream_belongs_to();
    stream.compiler.scope(|graph, _tx| {
        crate::core::runtime::streams_loaded_in_this_runtime::loaded_stream::the_current_value_of_each_graph_input(graph, &|uuid| {
            stream_handles.the_store_of_the_node(uuid)
        })
        .remove(&node_uuid.to_string())
    })
}

#[test]
#[serial]
fn a_value_written_to_a_stopped_kept_graph_input_shows_in_graph_and_reaches_its_next_play() {
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    std::fs::write(
        project_directory.path().join(PROJECT_FILE_NAME),
        TatolabProjectFile::of_a_new_project("acme-show")
            .canonical_bytes()
            .unwrap(),
    )
    .unwrap();
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let lights_uuid = uuid::Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    add_the_lights_input(&stream, lights_uuid);
    play(&stream);
    stream
        .stop_playback_keeping_the_graph()
        .expect("the stream stops");
    assert_eq!(
        the_current_value_graph_shows_for(&stream, lights_uuid),
        Some(json!({ "level": 0.0 }))
    );

    let written = json!({ "level": 0.6, "scene": "cool" });
    async_runtime
        .block_on(
            stream.write_a_value_to_a_top_level_graph_input("lights".to_string(), written.clone()),
        )
        .expect("a stopped kept Input takes the value for its next Play");
    assert_eq!(
        the_current_value_graph_shows_for(&stream, lights_uuid),
        Some(written.clone())
    );
    let refusal = async_runtime
        .block_on(stream.write_a_value_to_a_top_level_graph_input("lights".to_string(), json!(3)))
        .expect_err("a value its family does not take is refused while stopped too")
        .to_string();
    assert!(refusal.contains("carries Records"), "{refusal}");

    play(&stream);
    assert_eq!(
        the_value_a_new_reader_of_the_input_is_handed(&stream, &async_runtime, lights_uuid),
        written
    );
    stream.stop().expect("the stream ends");
}

#[test]
#[serial]
fn a_value_written_to_a_stopped_reset_graph_input_is_refused_saying_the_project_is_stopped() {
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    std::fs::write(
        project_directory.path().join(PROJECT_FILE_NAME),
        TatolabProjectFile::of_a_new_project("acme-show")
            .canonical_bytes()
            .unwrap(),
    )
    .unwrap();
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    stream
        .add_node(
            NodeSpec::new(
                NodeClassImportPath::new(
                    crate::core::graph::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE,
                )
                .unwrap(),
                json!({ "port": "trigger", "family": "array", "default": 0, "lifetime": "reset" }),
            )
            .with_display_name("trigger".to_string()),
        )
        .expect("a GraphInput is added");
    play(&stream);
    stream
        .stop_playback_keeping_the_graph()
        .expect("the stream stops");

    let refusal = async_runtime
        .block_on(stream.write_a_value_to_a_top_level_graph_input("trigger".to_string(), json!(1)))
        .expect_err("a reset Input keeps nothing written while stopped");
    assert!(
        matches!(
            refusal,
            Error::NodeOfAStreamIsNotRunning {
                not_running_because: WhyANodeOfAStreamIsNotRunning::PlaybackStopped,
                ..
            }
        ),
        "{refusal}"
    );
    assert!(
        refusal.to_string().contains("`start_playback` plays it"),
        "{refusal}"
    );
    stream.stop().expect("the stream ends");
}
