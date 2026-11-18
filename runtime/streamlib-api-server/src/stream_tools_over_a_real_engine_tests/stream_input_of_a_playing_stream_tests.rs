// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream input set on a playing stream, over a real engine: `write_graph_input` and a live
//! `set_node_input_value` naming no node each reach a node reading the `GraphInput` declaring it,
//! and a `reset` input set while the stream is stopped is refused naming the stopped playback.

use std::time::Instant;

use parking_lot::Mutex;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::nodes::ReactiveNode;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, NodeNotRunningBecauseOnTheWire, ProjectActionRefusalOnTheWire,
};

use super::*;

/// Every record a [`StreamToolsTestRecordsRecorder`] has read, by any recorder.
static RECORDS_READ: Mutex<Vec<Value>> = Mutex::new(Vec::new());

/// How long a value set on a playing stream may take to reach a node reading it.
const ARRIVES_WITHIN: Duration = Duration::from_secs(10);

/// A native reader of Records on its input `cue`, which records each one it reads.
#[streamlib::sdk::node(
    execution = reactive,
    input("cue", delivery_profile = "newest", family = Records)
)]
pub struct StreamToolsTestRecordsRecorder;

impl ReactiveNode for StreamToolsTestRecordsRecorder::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw("cue")? {
            let record: Value =
                rmp_serde::from_slice(message.payload.bytes()).expect("a Records message decodes");
            RECORDS_READ.lock().push(record);
        }
        Ok(())
    }
}

/// The UUID the graph document gives its `cue` Input.
const CUE_NODE_UUID: &str = "c1ec1ec1-ec1e-4c1e-8c1e-c1ec1ec1ec1e";

/// The UUID the graph document gives its `recorder`.
const RECORDER_NODE_UUID: &str = "c2ec2ec2-ec2e-4c2e-8c2e-c2ec2ec2ec2e";

/// A project whose stream `main` holds a top-level Records `GraphInput` declaring `cue`, of
/// `lifetime`, read by a [`StreamToolsTestRecordsRecorder`] `recorder`.
fn a_project_whose_cue_input_a_recorder_reads(lifetime: &str) -> ProjectWithGraphDocuments {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<StreamToolsTestRecordsRecorder::Node>());
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": CUE_NODE_UUID, "name": "cue", "type": "tatolab.stream:GraphInput",
              "config": { "port": "cue", "family": "records", "default": { "track": "intro" },
                          "lifetime": lifetime } },
            { "id": RECORDER_NODE_UUID, "name": "recorder",
              "type": StreamToolsTestRecordsRecorder::node_class_import_path().as_str(),
              "config": {} },
        ],
        "links": [
            { "source": { "node": "cue", "port": "value" },
              "target": { "node": "recorder", "port": "cue" } },
        ],
    }))
}

/// Wait until a recorder has read `record`, failing when none does within the wait.
async fn a_recorder_reads(record: &Value) {
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    while !RECORDS_READ.lock().contains(record) {
        assert!(Instant::now() < gave_up_at, "no recorder read {record}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait until recorders have read `record` `times` times, each set of it being one message,
/// failing when they do not within the wait.
async fn a_recorder_reads_times(record: &Value, times: usize) {
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    let times_read = || {
        RECORDS_READ
            .lock()
            .iter()
            .filter(|read| *read == record)
            .count()
    };
    while times_read() < times {
        assert!(
            Instant::now() < gave_up_at,
            "recorders read {record} {} times, not {times}",
            times_read()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// The project `project` opened and playing.
    pub(super) async fn playing(&self, project: &ProjectWithGraphDocuments) {
        let opened = the_run_of_the_project_once_finished(
            &self.operations_on_the_loaded_streams(),
            project.path(),
        )
        .await;
        assert_eq!(opened["state"], "done", "open_project: {opened}");
        assert!(
            opened["streams"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|stream| stream["state"] != "failed"),
            "open_project: {opened}"
        );
        self.the_project_action_done("start_playback", project)
            .await;
    }

    /// The answer of `write_graph_input` setting `value` on the input `cue` of `project`'s
    /// stream `main`, or its refusal.
    async fn write_the_cue(
        &self,
        project: &ProjectWithGraphDocuments,
        value: Value,
    ) -> std::result::Result<Value, Box<ProjectActionRefusalOnTheWire>> {
        self.the_answer_or_the_refusal_of(
            "write_graph_input",
            json!({ "stream": project.stream_id_of("main"), "input": "cue", "value": value }),
        )
        .await
    }

    /// The answer of a live `set_node_input_value` naming no node, setting `value` on the input
    /// `cue` of `project`'s stream `main`, or its refusal.
    async fn set_the_cue_live(
        &self,
        project: &ProjectWithGraphDocuments,
        value: Value,
    ) -> std::result::Result<Value, Box<ProjectActionRefusalOnTheWire>> {
        self.the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "input": "cue", "value": value,
                    "mode": "live" }),
        )
        .await
    }
}

/// On a playing stream, `write_graph_input` answers the family of the input it names and its
/// value reaches the node reading the input, and so does a live `set_node_input_value` naming no
/// node, the same value set again reaching it again; neither saves anything in the graph
/// document.
///
/// Mental-revert: a set naming no node that is refused without its node, or that sets anything
/// but the `value` of the `GraphInput` declaring the input, leaves the recorder without the value.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_input_set_on_a_playing_stream_reaches_the_node_reading_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_cue_input_a_recorder_reads("kept");
    engine.playing(&project).await;
    let document_before = project.the_graph_document_of("main");
    let written = json!({ "track": format!("drop-{}", Uuid::new_v4()) });

    let answer = engine
        .write_the_cue(&project, written.clone())
        .await
        .expect("a playing stream's input is written");

    assert_eq!(answer["family"], "records", "{answer}");
    a_recorder_reads(&written).await;
    let set_live = json!({ "track": format!("outro-{}", Uuid::new_v4()) });
    engine
        .set_the_cue_live(&project, set_live.clone())
        .await
        .expect("a playing stream's input is set live naming no node");
    a_recorder_reads(&set_live).await;
    engine
        .set_the_cue_live(&project, set_live.clone())
        .await
        .expect("the same value is set live again");
    a_recorder_reads_times(&set_live, 2).await;
    assert_eq!(project.the_graph_document_of("main"), document_before);
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}

/// A `reset` stream input set live while its stream is stopped, by `write_graph_input` or by a
/// `set_node_input_value` naming no node, is refused with `node_not_running`, naming the stopped
/// playback.
///
/// Mental-revert: a write that skips the check of the node's lifetime while stopped holds a value
/// the next Play forgets.
#[tokio::test(flavor = "multi_thread")]
async fn a_reset_stream_input_set_while_stopped_is_refused_naming_the_stopped_playback() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_cue_input_a_recorder_reads("reset");
    engine.playing(&project).await;
    engine
        .the_project_action_done("stop_playback", &project)
        .await;

    for refusal in [
        engine
            .write_the_cue(&project, json!({ "track": "drop" }))
            .await
            .expect_err("a stopped stream's reset input is refused a write"),
        engine
            .set_the_cue_live(&project, json!({ "track": "drop" }))
            .await
            .expect_err("a stopped stream's reset input is refused a live set"),
    ] {
        assert_eq!(
            refusal.refusal_code,
            ControlOperationRefusalCode::NodeNotRunning,
            "{refusal:?}"
        );
        assert_eq!(
            refusal.not_running_because,
            Some(NodeNotRunningBecauseOnTheWire::PlaybackStopped),
            "{refusal:?}"
        );
    }
}
