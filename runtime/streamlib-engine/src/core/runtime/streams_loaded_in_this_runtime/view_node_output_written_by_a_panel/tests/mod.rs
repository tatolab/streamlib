// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
use crate::core::descriptors::{NodeClassImportPath, PortFamily};
use crate::core::graph::NodeInstanceWithItsOutOfProcessLinkWiring;
use crate::core::nodes::NodeSpec;
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::{Runner, RuntimeOperations};
use crate::core::test_support::{
    MockOutputOnlyNode, a_temporary_directory_at_owner_only_mode, ensure_test_mocks_registered,
};
use crate::core::{InputLinkPortRef, NodeRunId, OutputLinkPortRef};
use crate::iceoryx2::{MessageGroupBoundary, OutputWriterInner};

mod graph_input_tests;
mod view_node_tests;

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
    fn what_the_graph_holds_of(&self, node_id: &NodeRunId) -> Value {
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
