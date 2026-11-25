// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_ipc_types::{
    DataChannelPayloadDescriptorKind, MessageReadFromAReservedSlot, OutputPortViewerReader,
};
use streamlib_project_repository::{PROJECT_FILE_NAME, TatolabProjectFile};

use super::*;
use crate::core::descriptors::{NodeClassImportPath};
use crate::core::graph::NodeInstanceWithItsOutOfProcessLinkWiring;
use crate::core::graph::input_values::{
    InputValueToSetLive, InputValuesSetLive, LiveInputValueSetRefusal,
};
use crate::core::nodes::NodeSpec;
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::{Runner, RuntimeOperations};
use crate::core::test_support::{
    MockOutputOnlyNode, a_temporary_directory_at_owner_only_mode, a_view_in,
    ensure_test_mocks_registered,
};
use crate::core::{InputLinkPortRef, NodeRunId, OutputLinkPortRef};
use crate::iceoryx2::{MessageGroupBoundary, OutputWriterInner};

mod view_node_tests;

const ARRIVES_WITHIN: Duration = Duration::from_secs(5);

/// The UUID of the project `acme-dj`, which leads the type of each view it brings.
macro_rules! acme_dj {
    () => {
        "3c9e7a52-6b1f-4d84-a0e3-9f2b5c7d1e46"
    };
}
const ACME_DJ: &str = acme_dj!();

const DECK_VIEW_TYPE: &str = concat!(acme_dj!(), ":deck");

fn the_deck_manifest() -> Value {
    json!({
        "contract": "1.1", "title": "Deck", "description": "Cues a track at the drop.",
        "entry": "index.html", "placement": "node",
        "inputs": [{ "name": "cue_in", "pin_type": { "type": "string" }, "description": "Cues to show." }],
        "outputs": [{ "name": "cue", "pin_type": { "type": "string" }, "description": "The cue played." },
                    { "name": "fader", "pin_type": { "type": "tensor" }, "range": [0, 1] }]
    })
}

const PULSE_VIEW_TYPE: &str = concat!(acme_dj!(), ":pulse");

fn the_pulse_manifest() -> Value {
    json!({
        "contract": "1.1", "title": "Pulse", "entry": "index.html", "placement": "node",
        "outputs": [{ "name": "beat", "pin_type": { "type": "audio" } },
                    { "name": "tempo", "pin_type": { "type": "float" } }]
    })
}

const BOARD_VIEW_TYPE: &str = concat!(acme_dj!(), ":board");

fn the_board_manifest() -> Value {
    json!({
        "contract": "1.1", "title": "Board", "entry": "index.html", "placement": "node",
        "outputs": [{ "name": "beat", "pin_type": { "type": "audio" } },
                    { "name": "cue", "pin_type": { "type": "string" } },
                    { "name": "fader", "pin_type": { "type": "tensor" } }]
    })
}

const LEVEL_VIEW_TYPE: &str = concat!(acme_dj!(), ":level");

fn the_level_manifest() -> Value {
    json!({
        "contract": "1.1", "title": "Level", "entry": "index.html", "placement": "node",
        "outputs": [{ "name": "level", "pin_type": { "type": "tensor" } }],
        "settings": { "type": "object",
                      "properties": { "gain": { "type": "number", "default": 1 } } }
    })
}

const MIXER_VIEW_TYPE: &str = concat!(acme_dj!(), ":mixer");

fn the_mixer_manifest() -> Value {
    json!({
        "contract": "1.1", "title": "Mixer", "entry": "index.html", "placement": "node",
        "outputs": [{ "name": "mix", "pin_type": { "type": "tensor" } }],
        "settings": { "type": "object", "required": ["gain"],
                      "properties": { "gain": { "type": "number", "default": 1 },
                                      "trim": { "type": "number", "default": 0 } } }
    })
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
            TatolabProjectFile {
                project: ACME_DJ.parse().unwrap(),
                ..TatolabProjectFile::of_a_new_project("acme-dj")
            }
            .canonical_bytes()
            .unwrap(),
        )
        .unwrap();
        a_view_in(project_directory.path(), "deck", &the_deck_manifest());
        a_view_in(project_directory.path(), "level", &the_level_manifest());
        a_view_in(project_directory.path(), "mixer", &the_mixer_manifest());
        a_view_in(project_directory.path(), "pulse", &the_pulse_manifest());
        a_view_in(project_directory.path(), "board", &the_board_manifest());
        a_view_in(
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

    fn add_a_node(&self, node_type: &str, name: &str) -> Result<NodeRunId> {
        self.stream.add_node(
            NodeSpec::new(NodeClassImportPath::new(node_type).unwrap(), json!({}))
                .with_display_name(name.to_string()),
        )
    }

    fn run(&self, node_id: &NodeRunId) -> String {
        let node_types = Arc::clone(self.stream.node_types_this_stream_resolves());
        self.stream.compiler.scope(|graph, _tx| {
            let node = graph
                .traversal_mut()
                .v(node_id)
                .first_mut()
                .expect("the node is in the stream");
            let instance = node_types.create(node).expect("the node's node builds");
            NodeInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(node);
            node.node_uuid.to_string()
        })
    }

    fn the_output_writer_of(&self, node_id: &NodeRunId) -> Arc<OutputWriterInner> {
        self.stream.compiler.scope(|graph, _tx| {
            let node = graph.traversal().v(node_id).first().unwrap();
            node.get::<crate::core::graph::NodeInstanceComponent>()
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

    /// A panel's write of `value` onto output `port` of the view node `node_uuid`, as
    /// `write_studio_view_output` makes it in the engine: the view node is read, then its input
    /// of the output's name set live, as a `live` transaction of that one set hands it over.
    fn write(&self, node_uuid: &str, port: &str, value: Value) -> Result<()> {
        self.write_each(node_uuid, &[(port, value)])
    }

    /// Each of `writes`, a value onto an output of the view node `node_uuid`, set live as one
    /// `live` transaction of those sets hands them over, the view node read for each first.
    fn write_each(&self, node_uuid: &str, writes: &[(&str, Value)]) -> Result<()> {
        let mut sets = Vec::new();
        for (port, value) in writes {
            let node =
                self.async_runtime
                    .block_on(self.stream.the_running_view_node_a_panel_writes(
                        node_uuid.to_string(),
                        port.to_string(),
                        value.clone(),
                    ))?;
            sets.push(InputValueToSetLive {
                member: None,
                node,
                input: port.to_string(),
                value: Some(value.clone()),
            });
        }
        match self.stream.set_input_values_live(&sets) {
            Ok(InputValuesSetLive::ApartFromTheGraph(_)) => Ok(()),
            Ok(InputValuesSetLive::OnTheGraph) => {
                panic!("a view node's input of an output's name is set live, apart from the graph")
            }
            Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt { refusal, .. }) => {
                Err(Error::Configuration(refusal.words))
            }
            Err(
                LiveInputValueSetRefusal::TheInputIsResetWhileStopped { runtime_words, .. }
                | LiveInputValueSetRefusal::TheInputIsSetTwice { runtime_words, .. },
            ) => Err(Error::Configuration(runtime_words)),
            Err(
                LiveInputValueSetRefusal::TheNodeDoesNotRun {
                    refusal: not_handed_over,
                    ..
                }
                | LiveInputValueSetRefusal::NotHandedOver(not_handed_over),
            ) => Err(not_handed_over),
        }
    }

    /// What the stream's graph holds of `node_id`: its input values and `studio` fields, and
    /// every link.
    fn what_the_graph_holds_of(&self, node_id: &NodeRunId) -> Value {
        let node = self.stream.compiler.scope(|graph, _tx| {
            let node = graph.traversal().v(node_id).first().unwrap();
            json!({ "input_values": node.input_values, "studio": node.studio })
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
