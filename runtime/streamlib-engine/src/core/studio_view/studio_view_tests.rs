// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A StudioView node in a stream loaded as running without the GPU context a real start
//! takes: each node holds the processor instance a start leaves it with, and a viewer reads
//! what a port publishes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_ipc_types::{MessageReadFromAReservedSlot, OutputPortViewerReader};
use streamlib_project_repository::{PROJECT_FILE_NAME, TatolabProjectFile};

use super::*;
use crate::core::error::Error;
use crate::core::graph::{
    InputLinkPortRef, OutputLinkPortRef, ProcessorInstanceWithItsOutOfProcessLinkWiring,
};
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::processors::{NodeTypesOneStreamResolves, ProcessorInstance, ProcessorSpec};
use crate::core::runtime::{
    LoadedStreamInThisRuntime, OptionsForLoadingOneStream, OutputPortViewerHeldOpen, Runner,
    RuntimeOperations,
};
use crate::core::test_support::{
    MockOutputOnlyProcessor, a_temporary_directory_at_owner_only_mode, ensure_test_mocks_registered,
};
use crate::core::{GeneratedProcessor, ProcessorUniqueId};
use crate::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody,
    DataChannelPayloadDescriptorKind, MessageGroupBoundary, OutputWriterInner,
    array_frame_attribute_table_of,
};

/// The name the test project's `tatolab-project.json` gives it.
const PROJECT_NAME: &str = "rig";

/// How long a message has to reach a viewer.
const ARRIVES_WITHIN: Duration = Duration::from_secs(5);

fn the_deck_manifest() -> Value {
    json!({
        "contract": "1.0", "title": "DJ Deck", "entry": "index.html", "placement": "node",
        "inputs": [{ "name": "cue_in", "family": "records" }],
        "outputs": [
            { "name": "fader", "family": "array", "range": [0, 1] },
            { "name": "cue", "family": "records" },
            { "name": "tone", "family": "channels" },
            { "name": "webcam", "family": "image" },
        ],
        "settings": {
            "type": "object",
            "properties": { "skin": { "type": "string", "default": "dark" } },
        },
    })
}

/// A project named [`PROJECT_NAME`] bringing the node view `deck`, the area view `meter` and
/// the folder `broken`, whose manifest is not JSON.
fn a_project_bringing_views() -> tempfile::TempDir {
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    std::fs::write(
        project_directory.path().join(PROJECT_FILE_NAME),
        TatolabProjectFile::of_a_new_project(PROJECT_NAME)
            .canonical_bytes()
            .unwrap(),
    )
    .unwrap();
    for (view_folder, manifest) in [
        ("deck", the_deck_manifest().to_string()),
        (
            "meter",
            json!({ "contract": "1.0", "title": "Meter", "entry": "index.html", "placement": "area" })
                .to_string(),
        ),
        ("broken", "{".to_string()),
    ] {
        let folder = project_directory.path().join("views").join(view_folder);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("tatolab_view.json"), manifest).unwrap();
    }
    project_directory
}

fn a_studio_view_node(view_folder: &str) -> ProcessorSpec {
    ProcessorSpec::new(
        the_studio_view_node_type(),
        json!({ "view_type": format!("{PROJECT_NAME}:{view_folder}"), "skin": "light" }),
    )
    .with_display_name("deck".to_string())
}

/// A stream `a` of a project bringing views, holding a running StudioView node `deck` showing
/// the view `deck`.
struct AStreamHoldingARunningStudioView {
    _runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
    deck_id: ProcessorUniqueId,
    deck_node_uuid: String,
    async_runtime: tokio::runtime::Runtime,
    _project_directory: tempfile::TempDir,
}

impl AStreamHoldingARunningStudioView {
    fn load() -> Self {
        ensure_test_mocks_registered();
        let project_directory = a_project_bringing_views();
        let runner = Runner::new().expect("the engine builds");
        let stream = runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .with_stream_id("a"),
            )
            .expect("the stream loads");
        let deck_id = stream
            .add_processor(a_studio_view_node("deck"))
            .expect("a StudioView of a view that opens is added");
        let deck_node_uuid = stream.compiler.scope(|graph, _tx| {
            let node_types = Arc::clone(graph.node_types_this_stream_resolves());
            let deck = graph
                .traversal_mut()
                .v(&deck_id)
                .first_mut()
                .expect("the node is in the stream");
            let instance = node_types
                .create(deck)
                .expect("the StudioView processor is built");
            ProcessorInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(deck);
            deck.node_uuid.to_string()
        });
        Self {
            _runner: runner,
            stream,
            deck_id,
            deck_node_uuid,
            async_runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            _project_directory: project_directory,
        }
    }

    fn write(&self, port: &str, value: Value) -> crate::core::error::Result<()> {
        self.async_runtime
            .block_on(self.stream.write_a_studio_view_output(
                self.deck_node_uuid.clone(),
                port.into(),
                value,
            ))
    }

    fn open_a_viewer_of(&self, node_uuid: &str, port: &str) -> OutputPortViewerHeldOpen {
        self.async_runtime
            .block_on(self.stream.open_a_viewer_of_an_output_port(
                node_uuid.to_string(),
                port.into(),
                None,
            ))
            .expect("a port of a running node opens a viewer")
    }
}

fn a_reader_of(viewer: &OutputPortViewerHeldOpen, reader_name: &str) -> OutputPortViewerReader {
    OutputPortViewerReader::subscribe(
        &viewer.hand_off().iceoryx2_domain_root,
        &viewer.hand_off().channel_service_name,
        reader_name,
    )
    .expect("the viewer subscribes to the port's channel")
}

fn the_next_message_read(reader: &mut OutputPortViewerReader) -> MessageReadFromAReservedSlot {
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    loop {
        if let Some(message) = reader.read_the_latest_message().expect("the viewer reads") {
            return message;
        }
        assert!(Instant::now() < gave_up_at, "no message reached the viewer");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Each `float64` element of attribute `name` of the Array frame `message` carries, and its shape.
fn the_float64_attribute(
    message: &MessageReadFromAReservedSlot,
    name: &str,
) -> (Vec<u64>, Vec<f64>) {
    assert_eq!(
        message.payload_descriptor.kind,
        DataChannelPayloadDescriptorKind::ArrayFrame as u32
    );
    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&message.payload_descriptor.body),
        &message.payload,
    )
    .expect("an Array frame");
    let row = rows
        .into_iter()
        .find(|row| row.name == name)
        .expect("the attribute is in the frame");
    assert_eq!(row.dtype, "float64");
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = row.residency
    else {
        panic!("a CPU attribute");
    };
    let data =
        &message.payload[payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize];
    (
        row.shape,
        data.as_chunks::<8>()
            .0
            .iter()
            .map(|element| f64::from_le_bytes(*element))
            .collect(),
    )
}

/// What a graph document records of the stream: each node's identity, config and ports, and
/// its links.
fn what_a_graph_document_records_of(stream: &LoadedStreamInThisRuntime) -> Value {
    let graph = stream.to_json().unwrap();
    json!({
        "nodes": graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| json!({
                "id": node["id"], "type": node["type"], "name": node["name"],
                "config": node["config"], "ports": node["ports"],
            }))
            .collect::<Vec<_>>(),
        "links": graph["links"],
    })
}

fn the_port_names_of(ports: &Value) -> Vec<(String, String)> {
    ports
        .as_array()
        .expect("a list of ports")
        .iter()
        .map(|port| {
            (
                port["name"].as_str().unwrap().to_string(),
                port["family"]["family"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

#[test]
#[serial]
fn a_studio_view_node_takes_one_port_per_input_and_output_its_view_declares() {
    let deck = AStreamHoldingARunningStudioView::load();

    let graph = deck.stream.to_json().unwrap();
    let node = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "deck")
        .expect("the graph lists the StudioView node");

    assert_eq!(node["type"], STUDIO_VIEW_NODE_TYPE, "{node}");
    assert_eq!(
        the_port_names_of(&node["ports"]["inputs"]),
        [("cue_in".to_string(), "records".to_string())],
        "{node}"
    );
    assert_eq!(
        the_port_names_of(&node["ports"]["outputs"]),
        [
            ("fader".to_string(), "array".to_string()),
            ("cue".to_string(), "records".to_string()),
            ("tone".to_string(), "channels".to_string()),
            ("webcam".to_string(), "image".to_string()),
        ],
        "{node}"
    );
}

/// A view the project does not bring, one whose manifest does not read and an `area` view each
/// refuse the node naming it and the view, both on a live add and in a graph document's load.
#[test]
#[serial]
fn a_studio_view_of_a_view_that_is_missing_or_does_not_open_is_refused_by_name() {
    let deck = AStreamHoldingARunningStudioView::load();
    let node_types =
        NodeTypesOneStreamResolves::of_the_project_in(deck._project_directory.path().to_path_buf());

    for (view_folder, why) in [
        ("nowhere", "bring no view `rig:nowhere`"),
        ("broken", "is not JSON"),
        ("meter", "an `area` view"),
    ] {
        let live_add = deck
            .stream
            .add_processor(a_studio_view_node(view_folder).with_display_name("panel"))
            .expect_err("a view that does not open adds no node");
        match &live_add {
            Error::StudioViewDoesNotOpen {
                node_name,
                view_type,
                why: refused_because,
            } => {
                assert_eq!(node_name, "panel");
                assert_eq!(view_type, &format!("{PROJECT_NAME}:{view_folder}"));
                assert!(refused_because.contains(why), "{refused_because}");
            }
            other => panic!("expected StudioViewDoesNotOpen, got {other:?}"),
        }

        let document = GraphSnapshot::from_graph_document(json!({
            "nodes": [{
                "name": "panel",
                "type": STUDIO_VIEW_NODE_TYPE,
                "config": { "view_type": format!("{PROJECT_NAME}:{view_folder}") },
            }],
        }))
        .unwrap();
        let load = document
            .validate(&node_types)
            .map(drop)
            .expect_err("a graph document showing a view that does not open does not load");
        assert!(
            load.to_string().contains("node `panel`")
                && load
                    .to_string()
                    .contains(&format!("`{PROJECT_NAME}:{view_folder}`")),
            "{load}"
        );
    }
    assert!(
        deck.stream
            .add_processor(
                ProcessorSpec::new(the_studio_view_node_type(), json!({ "skin": "dark" }))
                    .with_display_name("unnamed")
            )
            .is_err(),
        "a StudioView names its view"
    );
}

/// What a panel writes onto each output is published there in its family's payload, the last
/// write winning, and the stream's graph holds none of it.
#[test]
#[serial]
fn a_value_written_onto_each_output_reaches_a_viewer_of_it_and_the_graph_holds_none_of_it() {
    let deck = AStreamHoldingARunningStudioView::load();
    let recorded_before = what_a_graph_document_records_of(&deck.stream);
    let fader_viewer = deck.open_a_viewer_of(&deck.deck_node_uuid, "fader");
    let cue_viewer = deck.open_a_viewer_of(&deck.deck_node_uuid, "cue");
    let tone_viewer = deck.open_a_viewer_of(&deck.deck_node_uuid, "tone");
    let mut fader = a_reader_of(&fader_viewer, "a-fader-viewer");
    let mut cue = a_reader_of(&cue_viewer, "a-cue-viewer");
    let mut tone = a_reader_of(&tone_viewer, "a-tone-viewer");

    deck.write("fader", json!(0.1)).unwrap();
    deck.write("fader", json!(0.75)).unwrap();
    let latest_fader = the_next_message_read(&mut fader);
    assert_eq!(
        the_float64_attribute(&latest_fader, "value"),
        (vec![1], vec![0.75]),
        "the last write wins"
    );
    assert_eq!(
        latest_fader.group_boundary,
        MessageGroupBoundary::StartsGroup
    );

    deck.write("fader", json!({ "levels": [[0.5, 1.0], [0.25, 0.0]] }))
        .unwrap();
    assert_eq!(
        the_float64_attribute(&the_next_message_read(&mut fader), "levels"),
        (vec![2, 2], vec![0.5, 1.0, 0.25, 0.0])
    );

    deck.write("cue", json!({ "deck": "A", "bpm": 128, "playing": true }))
        .unwrap();
    let cue_message = the_next_message_read(&mut cue);
    assert_eq!(
        cue_message.payload_descriptor.kind,
        DataChannelPayloadDescriptorKind::RecordsMessage as u32
    );
    let record: Value = rmp_serde::from_slice(&cue_message.payload).unwrap();
    assert_eq!(record, json!({ "deck": "A", "bpm": 128, "playing": true }));

    for expected_start_sample in [0, 3] {
        deck.write(
            "tone",
            json!({ "sample_rate": 48_000, "channels": [[0.5, -0.5, 0.25], [1.0, 0.0, -1.0]] }),
        )
        .unwrap();
        let block = the_next_message_read(&mut tone);
        assert_eq!(
            block.payload_descriptor.kind,
            DataChannelPayloadDescriptorKind::ChannelsBlock as u32
        );
        let body =
            ChannelsBlockDescriptorBody::from_descriptor_body(&block.payload_descriptor.body);
        assert_eq!(
            (
                body.start_sample,
                body.sample_rate,
                body.channel_count,
                body.samples_per_channel
            ),
            (expected_start_sample, 48_000, 2, 3)
        );
        let samples: Vec<f32> = block
            .payload
            .as_chunks::<4>()
            .0
            .iter()
            .map(|sample| f32::from_le_bytes(*sample))
            .collect();
        assert_eq!(samples, [0.5, -0.5, 0.25, 1.0, 0.0, -1.0]);
    }

    drop((fader, cue, tone));
    drop((fader_viewer, cue_viewer, tone_viewer));
    assert_eq!(
        what_a_graph_document_records_of(&deck.stream),
        recorded_before,
        "a write changes nothing the graph holds"
    );
}

#[test]
#[serial]
fn a_write_onto_a_port_the_node_lacks_or_of_a_value_its_family_does_not_take_is_refused() {
    let deck = AStreamHoldingARunningStudioView::load();

    let no_such_port = deck
        .write("crossfader", json!(0.5))
        .unwrap_err()
        .to_string();
    assert!(
        no_such_port.contains("no output `crossfader`")
            && no_such_port.contains("fader, cue, tone, webcam"),
        "{no_such_port}"
    );
    let an_input = deck.write("cue_in", json!({})).unwrap_err().to_string();
    assert!(an_input.contains("no output `cue_in`"), "{an_input}");
    for (port, value, why) in [
        ("webcam", json!(1), "takes no Image value"),
        ("fader", json!("loud"), "a number or a list of numbers"),
        ("fader", json!([[1.0], [2.0, 3.0]]), "rectangular"),
        ("cue", json!([1, 2]), "an object of fields"),
        ("tone", json!({ "channels": [[0.5]] }), "`sample_rate`"),
        (
            "tone",
            json!({ "sample_rate": 48_000, "channels": [[0.5], [0.5, 0.5]] }),
            "as many samples",
        ),
    ] {
        let refusal = deck.write(port, value).unwrap_err().to_string();
        assert!(
            refusal.contains(&format!("output `{port}`")) && refusal.contains(why),
            "{port}: {refusal}"
        );
    }
}

#[test]
#[serial]
fn a_write_onto_a_node_that_is_not_a_running_studio_view_is_refused() {
    let deck = AStreamHoldingARunningStudioView::load();
    let camera_id = deck
        .stream
        .add_processor(
            ProcessorSpec::new(
                MockOutputOnlyProcessor::processor_class_import_path(),
                Value::Null,
            )
            .with_display_name("camera".to_string()),
        )
        .unwrap();
    let camera_uuid = deck.stream.compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(&camera_id)
            .first()
            .unwrap()
            .node_uuid
            .to_string()
    });
    let idle_deck_id = deck
        .stream
        .add_processor(a_studio_view_node("deck").with_display_name("idle"))
        .unwrap();
    let idle_deck_uuid = deck.stream.compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(&idle_deck_id)
            .first()
            .unwrap()
            .node_uuid
            .to_string()
    });
    let write_onto = |node_uuid: &str| {
        deck.async_runtime
            .block_on(deck.stream.write_a_studio_view_output(
                node_uuid.to_string(),
                "fader".into(),
                json!(0.5),
            ))
            .unwrap_err()
            .to_string()
    };

    assert!(write_onto(&camera_uuid).contains("onto a StudioView node's output"));
    assert!(write_onto(&idle_deck_uuid).contains("is not running"));
    let not_held = write_onto("deck");
    assert!(
        not_held.contains("has the id `deck`")
            && not_held.contains(&format!("`{}` (deck)", deck.deck_node_uuid)),
        "{not_held}"
    );
}

/// A panel reads a StudioView node's input through a viewer of the output linked into it.
#[test]
#[serial]
fn a_panel_reads_an_input_through_a_viewer_of_the_output_linked_into_it() {
    let deck = AStreamHoldingARunningStudioView::load();
    let cue_source_id = deck
        .stream
        .add_processor(
            ProcessorSpec::new(
                MockOutputOnlyProcessor::processor_class_import_path(),
                Value::Null,
            )
            .with_display_name("cues".to_string()),
        )
        .unwrap();
    deck.stream
        .connect(
            OutputLinkPortRef::new(cue_source_id.clone(), "out1"),
            InputLinkPortRef::new(deck.deck_id.clone(), "cue_in"),
        )
        .expect("a Records output links into the view's Records input");
    let mut instance = ProcessorInstance::new(Box::new(
        <MockOutputOnlyProcessor::Processor as GeneratedProcessor>::from_config(Default::default())
            .unwrap(),
    ));
    instance.install_iceoryx2_resources().unwrap();
    let cue_source_output: Arc<OutputWriterInner> =
        instance.iceoryx2_output_writer_inner().unwrap();
    let cue_source_uuid = deck.stream.compiler.scope(|graph, _tx| {
        let cue_source = graph.traversal_mut().v(&cue_source_id).first_mut().unwrap();
        ProcessorInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(cue_source);
        cue_source.node_uuid.to_string()
    });

    let panel_viewer = deck.open_a_viewer_of(&cue_source_uuid, "out1");
    let mut panel = a_reader_of(&panel_viewer, "a-panel");
    cue_source_output
        .write_bytes_with_timestamp("out1", b"cue A", 42, MessageGroupBoundary::StartsGroup)
        .unwrap();

    let read = the_next_message_read(&mut panel);
    assert_eq!((&read.payload[..], read.timestamp_ns), (&b"cue A"[..], 42));
}

#[test]
fn a_running_studio_view_takes_new_settings_and_keeps_its_view() {
    let deck = AStreamHoldingARunningStudioView::load();
    let mut processor = deck.stream.compiler.scope(|graph, _tx| {
        super::StudioViewProcessor::of_the_node(graph.traversal().v(&deck.deck_id).first().unwrap())
            .unwrap()
    });

    crate::core::processors::DynGeneratedProcessor::apply_config_json(
        &mut processor,
        &json!({ "view_type": "rig:deck", "skin": "neon" }),
    )
    .expect("a view's settings change live");
    assert_eq!(
        crate::core::processors::DynGeneratedProcessor::config_json(&processor),
        json!({ "view_type": "rig:deck", "skin": "neon" })
    );
    let another_view = crate::core::processors::DynGeneratedProcessor::apply_config_json(
        &mut processor,
        &json!({ "view_type": "rig:mixer" }),
    )
    .unwrap_err()
    .to_string();
    assert!(another_view.contains("is another node"), "{another_view}");
}
