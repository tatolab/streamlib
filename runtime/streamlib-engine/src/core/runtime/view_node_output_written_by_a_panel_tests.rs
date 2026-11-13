// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! View nodes in a loaded stream read as running without the GPU context a real start takes:
//! each running node holds the processor instance a start leaves it with, and nothing compiles.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_ipc_types::{
    DataChannelPayloadDescriptorKind, MessageReadFromAReservedSlot, OutputPortViewerReader,
};
use streamlib_project_repository::{
    PROJECT_FILE_NAME, PROJECT_VIEWS_DIRECTORY, TATOLAB_VIEW_MANIFEST_FILE_NAME, TatolabProjectFile,
};

use super::*;
use crate::core::descriptors::{PortFamily, ProcessorClassImportPath};
use crate::core::graph::ProcessorInstanceWithItsOutOfProcessLinkWiring;
use crate::core::processors::ProcessorSpec;
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::{Runner, RuntimeOperations};
use crate::core::test_support::{
    MockOutputOnlyProcessor, a_temporary_directory_at_owner_only_mode, ensure_test_mocks_registered,
};
use crate::core::{InputLinkPortRef, OutputLinkPortRef, ProcessorUniqueId};
use crate::iceoryx2::{MessageGroupBoundary, OutputWriterInner};

const ARRIVES_WITHIN: Duration = Duration::from_secs(5);

const DECK_VIEW_TYPE: &str = "acme-dj:deck";

fn the_deck_manifest() -> Value {
    json!({
        "contract": "1.0", "title": "Deck", "description": "Cues a track at the drop.",
        "entry": "index.html", "placement": "node",
        "inputs": [{ "name": "cue_in", "family": "records", "description": "Cues to show." }],
        "outputs": [{ "name": "cue", "family": "records", "description": "The cue played." },
                    { "name": "fader", "family": "array", "range": [0, 1] }]
    })
}

const LEVEL_VIEW_TYPE: &str = "acme-dj:level";

fn the_level_manifest() -> Value {
    json!({
        "contract": "1.0", "title": "Level", "entry": "index.html", "placement": "node",
        "outputs": [{ "name": "level", "family": "array" }],
        "settings": { "type": "object",
                      "properties": { "gain": { "type": "number", "default": 1 } } }
    })
}

fn a_view_folder_in(project_directory: &Path, view_folder_name: &str, manifest: &Value) {
    let folder = project_directory
        .join(PROJECT_VIEWS_DIRECTORY)
        .join(view_folder_name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join(TATOLAB_VIEW_MANIFEST_FILE_NAME),
        serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
}

/// A stream `a` of the project `acme-dj`, whose `views/` holds `deck` and a `broken` view that
/// does not open.
struct AStreamOfAProjectBringingViews {
    _runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
    async_runtime: tokio::runtime::Runtime,
    _project_directory: tempfile::TempDir,
}

impl AStreamOfAProjectBringingViews {
    fn load() -> Self {
        ensure_test_mocks_registered();
        let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
        std::fs::write(
            project_directory.path().join(PROJECT_FILE_NAME),
            TatolabProjectFile::of_a_new_project("acme-dj")
                .canonical_bytes()
                .unwrap(),
        )
        .unwrap();
        a_view_folder_in(project_directory.path(), "deck", &the_deck_manifest());
        a_view_folder_in(project_directory.path(), "level", &the_level_manifest());
        a_view_folder_in(
            project_directory.path(),
            "broken",
            &json!({ "contract": "1.0", "entry": "index.html", "placement": "node" }),
        );
        let runner = Runner::new().expect("the engine builds");
        let stream = runner
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                "a",
            ))
            .expect("the stream loads");
        Self {
            _runner: runner,
            stream,
            async_runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            _project_directory: project_directory,
        }
    }

    fn add_a_node(&self, node_type: &str, name: &str) -> Result<ProcessorUniqueId> {
        self.stream.add_processor(
            ProcessorSpec::new(ProcessorClassImportPath::new(node_type).unwrap(), json!({}))
                .with_display_name(name.to_string()),
        )
    }

    /// Run `node_id` as a start would: its processor built and attached, answering its UUID.
    fn run(&self, node_id: &ProcessorUniqueId) -> String {
        let node_types = Arc::clone(self.stream.node_types_this_stream_resolves());
        self.stream.compiler.scope(|graph, _tx| {
            let node = graph
                .traversal_mut()
                .v(node_id)
                .first_mut()
                .expect("the node is in the stream");
            let instance = node_types
                .create(node)
                .expect("the node's processor builds");
            ProcessorInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(node);
            node.node_uuid.to_string()
        })
    }

    fn the_output_writer_of(&self, node_id: &ProcessorUniqueId) -> Arc<OutputWriterInner> {
        self.stream.compiler.scope(|graph, _tx| {
            let node = graph.traversal().v(node_id).first().unwrap();
            node.get::<crate::core::graph::ProcessorInstanceComponent>()
                .unwrap()
                .0
                .lock()
                .iceoryx2_output_writer_inner()
                .unwrap()
        })
    }

    fn a_reader_of(&self, node_uuid: &str, port: &str) -> OutputPortViewerHeldOpenAndRead {
        let viewer = self
            .async_runtime
            .block_on(self.stream.open_a_viewer_of_an_output_port(
                node_uuid.to_string(),
                port.to_string(),
                None,
            ))
            .expect("a running node's output opens a viewer");
        let hand_off = viewer.hand_off().clone();
        let reader = OutputPortViewerReader::subscribe(
            &hand_off.iceoryx2_domain_root,
            &hand_off.channel_service_name,
            &hand_off.wake_service_name,
            "a-reader",
            hand_off.viewer_reads,
        )
        .expect("the reader subscribes to the port's channel");
        (viewer, reader)
    }

    fn write(&self, node_uuid: &str, port: &str, value: Value) -> Result<()> {
        self.async_runtime
            .block_on(self.stream.write_a_value_onto_an_output_of_a_view_node(
                node_uuid.to_string(),
                port.to_string(),
                value,
            ))
    }

    /// What the stream's graph holds of `node_id`: its config, pins and `studio` fields, and
    /// every link.
    fn what_the_graph_holds_of(&self, node_id: &ProcessorUniqueId) -> Value {
        let node = self.stream.compiler.scope(|graph, _tx| {
            let node = graph.traversal().v(node_id).first().unwrap();
            json!({ "config": node.config, "pins": node.pins, "studio": node.studio })
        });
        json!({ "node": node, "links": self.stream.to_json().unwrap()["links"] })
    }
}

type OutputPortViewerHeldOpenAndRead = (
    crate::core::runtime::OutputPortViewerHeldOpen,
    OutputPortViewerReader,
);

fn the_next_message_read(reader: &mut OutputPortViewerReader) -> MessageReadFromAReservedSlot {
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    loop {
        if let Some(message) = reader.read_the_latest_message().expect("the reader reads") {
            return message;
        }
        assert!(Instant::now() < gave_up_at, "no message reached the reader");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[serial]
fn the_node_catalog_reports_a_view_type_with_its_manifests_description_and_ports() {
    let stream = AStreamOfAProjectBringingViews::load();

    let catalog = stream
        .stream
        .node_types_this_stream_resolves()
        .node_catalog();
    let deck = catalog
        .iter()
        .find(|descriptor| descriptor.processor_class_import_path.as_str() == DECK_VIEW_TYPE)
        .expect("the catalog lists the project's view");
    assert_eq!(deck.description, "Cues a track at the drop.");
    assert_eq!(
        deck.inputs
            .iter()
            .map(|port| (port.name.as_str(), port.description.as_str()))
            .collect::<Vec<_>>(),
        [("cue_in", "Cues to show.")]
    );
    assert_eq!(
        deck.outputs
            .iter()
            .map(|port| (port.name.as_str(), port.family.families_an_input_takes()))
            .collect::<Vec<_>>(),
        [
            ("cue", vec![PortFamily::Records]),
            ("fader", vec![PortFamily::Array])
        ]
    );
    assert!(
        !catalog
            .iter()
            .any(|descriptor| descriptor.processor_class_import_path.as_str() == "acme-dj:broken"),
        "a view that does not open is no type"
    );
}

#[test]
#[serial]
fn a_value_written_onto_a_view_nodes_output_reaches_its_reader_and_the_graph_keeps_none_of_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream
        .add_a_node(DECK_VIEW_TYPE, "deck")
        .expect("a node of the view's type is added");
    let deck_uuid = stream.run(&deck_id);
    let graph_before = stream.what_the_graph_holds_of(&deck_id);
    let (_viewer, mut reader) = stream.a_reader_of(&deck_uuid, "cue");

    stream
        .write(&deck_uuid, "cue", json!({ "track": "intro", "beat": 32 }))
        .expect("the view node takes the record");
    let message = the_next_message_read(&mut reader);
    assert_eq!(
        message.payload_descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
    assert_eq!(read_back, json!({ "track": "intro", "beat": 32 }));

    stream
        .write(&deck_uuid, "cue", json!({ "track": "drop", "beat": 64 }))
        .expect("a later write is taken too");
    let message = the_next_message_read(&mut reader);
    let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
    assert_eq!(read_back["track"], "drop", "the last write is what is read");

    assert_eq!(stream.what_the_graph_holds_of(&deck_id), graph_before);
}

#[test]
#[serial]
fn a_write_the_port_or_node_does_not_take_is_refused_naming_why() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);
    let camera_id = stream
        .add_a_node(
            MockOutputOnlyProcessor::processor_class_import_path().as_str(),
            "camera",
        )
        .unwrap();
    let camera_uuid = stream.run(&camera_id);

    let refusal = stream
        .write(&deck_uuid, "fader", json!({ "values": "loud" }))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("`fader`") && refusal.contains("carries Array"),
        "{refusal}"
    );
    let refusal = stream
        .write(&deck_uuid, "volume", json!(1))
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("no output `volume`"), "{refusal}");
    let refusal = stream
        .write(&camera_uuid, "out1", json!({}))
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("no view its project brings"), "{refusal}");
}

#[test]
#[serial]
fn a_write_onto_a_view_node_of_a_stopped_project_says_the_project_is_stopped_and_how_to_play_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);
    stream
        .stream
        .stop_playback_keeping_the_graph()
        .expect("the stream's playback stops");

    let refusal = stream
        .write(&deck_uuid, "fader", json!(0.5))
        .expect_err("a stopped project runs no node to publish")
        .to_string();
    assert!(refusal.contains("is stopped"), "{refusal}");
    assert!(refusal.contains("`start_playback` plays it"), "{refusal}");
    assert!(!refusal.contains("Resource not found"), "{refusal}");
}

#[test]
#[serial]
fn a_view_nodes_config_its_views_settings_do_not_declare_or_type_is_refused_naming_each_key() {
    let stream = AStreamOfAProjectBringingViews::load();
    let level_id = stream.add_a_node(LEVEL_VIEW_TYPE, "level").unwrap();

    stream
        .stream
        .update_processor_config(&level_id, json!({ "gain": 0.5 }))
        .expect("a setting the view declares, of its type, is taken");
    let refusal = stream
        .stream
        .update_processor_config(&level_id, json!({ "gain": "loud", "colour": 3 }))
        .expect_err("a setting of the wrong type and one the view does not declare are refused")
        .to_string();
    assert!(refusal.contains("`gain`"), "{refusal}");
    assert!(refusal.contains("\"loud\""), "{refusal}");
    assert!(refusal.contains("colour"), "{refusal}");
    assert!(refusal.contains(LEVEL_VIEW_TYPE), "{refusal}");

    let refusal = stream
        .stream
        .add_processor(
            ProcessorSpec::new(
                ProcessorClassImportPath::new(DECK_VIEW_TYPE).unwrap(),
                json!({ "bpm": 120 }),
            )
            .with_display_name("deck".to_string()),
        )
        .expect_err("a view that declares no settings takes none")
        .to_string();
    assert!(refusal.contains("bpm"), "{refusal}");
}

#[test]
#[serial]
fn a_second_node_of_one_view_in_a_stream_is_refused_naming_the_node_holding_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();

    let refusal = stream
        .add_a_node(DECK_VIEW_TYPE, "deck-2")
        .expect_err("a stream holds one node of each view")
        .to_string();
    assert!(
        refusal.contains("node `deck`") && refusal.contains(DECK_VIEW_TYPE),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_node_naming_a_view_that_does_not_open_is_refused_with_why() {
    let stream = AStreamOfAProjectBringingViews::load();

    let refusal = stream
        .add_a_node("acme-dj:broken", "broken")
        .expect_err("a view that does not open is no type")
        .to_string();
    assert!(
        refusal.contains("`acme-dj:broken` does not open") && refusal.contains("title"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_panel_reads_a_view_nodes_input_through_a_viewer_of_the_port_linked_into_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    let camera_id = stream
        .add_a_node(
            MockOutputOnlyProcessor::processor_class_import_path().as_str(),
            "camera",
        )
        .unwrap();
    let camera_uuid = stream.run(&camera_id);
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    stream.run(&deck_id);
    stream
        .stream
        .connect(
            OutputLinkPortRef::new(camera_id.clone(), "out1"),
            InputLinkPortRef::new(deck_id, "cue_in"),
        )
        .expect("the camera's output links into the view node's input");
    let (_viewer, mut reader) = stream.a_reader_of(&camera_uuid, "out1");

    let record = rmp_serde::to_vec_named(&json!({ "track": "outro" })).unwrap();
    stream
        .the_output_writer_of(&camera_id)
        .write_bytes_described_by(
            "out1",
            &record,
            10,
            MessageGroupBoundary::StartsGroup,
            streamlib_ipc_types::DataChannelPayloadDescriptor::records_message(),
        )
        .expect("the camera writes");
    let message = the_next_message_read(&mut reader);
    assert_eq!(message.payload, record);
}

impl AStreamOfAProjectBringingViews {
    fn add_a_top_level_graph_input(&self, name: &str, config: Value) -> ProcessorUniqueId {
        self.stream
            .add_processor(
                ProcessorSpec::new(
                    ProcessorClassImportPath::new(
                        crate::core::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE,
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

    fn with_the_graph_inputs_processor<T>(
        &self,
        node_id: &ProcessorUniqueId,
        call: impl FnOnce(&mut crate::core::processors::NodeWhoseOutputsAClientWritesProcessor) -> T,
    ) -> T {
        self.stream.compiler.scope(|graph, _tx| {
            let node = graph.traversal().v(node_id).first().unwrap();
            let mut processor = node
                .get::<crate::core::graph::ProcessorInstanceComponent>()
                .unwrap()
                .0
                .lock();
            call(
                processor
                    .as_any_mut()
                    .downcast_mut::<crate::core::processors::NodeWhoseOutputsAClientWritesProcessor>()
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
        stream.with_the_graph_inputs_processor(&cue_id, |processor| processor
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
        .with_the_graph_inputs_processor(&cue_id, |processor| {
            processor.hand_each_current_value_to_a_reader_it_has_not_reached()
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
    /// Run the `GraphInput` `node_id` as a Play does: a fresh processor, handed its store.
    fn play_the_graph_input(
        &self,
        node_id: &ProcessorUniqueId,
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
        self.with_the_graph_inputs_processor(node_id, |processor| {
            processor.keep_the_graph_input_value_in(store)
        });
        node_uuid
    }

    /// Stop the `GraphInput` `node_id` as a Stop does: its processor torn down and dropped.
    fn stop_the_graph_input(&self, node_id: &ProcessorUniqueId) {
        self.with_the_graph_inputs_processor(node_id, |processor| {
            processor.commit_the_kept_value_as_the_node_stops()
        });
    }

    fn the_graph_inputs_current_value(&self, node_id: &ProcessorUniqueId) -> Option<Value> {
        self.with_the_graph_inputs_processor(node_id, |processor| {
            processor.current_value_of_the_output("value").cloned()
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
        .with_the_graph_inputs_processor(&lights_id, |processor| {
            processor.hand_each_current_value_to_a_reader_it_has_not_reached()
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
    use crate::core::processors::DynGeneratedProcessor;

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
        .with_the_graph_inputs_processor(&fader_id, |processor| {
            processor.apply_config_json(
                &json!({ "port": "fader", "family": "array", "default": 0.0, "lifetime": "kept" }),
            )
        })
        .expect("the lifetime changes to kept");
    assert_eq!(the_value_the_store_keeps(), Some(json!(0.3)));
    stream.write_the_input("fader", json!(0.6)).unwrap();
    assert_eq!(the_value_the_store_keeps(), Some(json!(0.6)));

    stream
        .with_the_graph_inputs_processor(&fader_id, |processor| {
            processor.apply_config_json(
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
        crate::core::graph_interface_resolution::the_value_lifetime_a_graph_input_config_declares(
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
        .add_processor(
            ProcessorSpec::new(
                ProcessorClassImportPath::new(
                    crate::core::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE,
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
        .wait_until_every_processor_is_running(Duration::from_secs(30))
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
        super::super::loaded_stream::the_current_value_of_each_graph_input(graph, &|uuid| {
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
        .add_processor(
            ProcessorSpec::new(
                ProcessorClassImportPath::new(
                    crate::core::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE,
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
