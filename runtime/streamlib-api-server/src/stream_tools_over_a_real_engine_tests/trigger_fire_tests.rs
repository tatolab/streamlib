// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Trigger input fired over a real engine: a live `set_node_input_value` of no value fires an
//! unlinked Trigger of a playing stream once, each fire one run of its reader, and is neither
//! saved nor held; a fire carrying a value, one in `commit`, one of a linked Trigger, one in a
//! transaction that is not value sets alone and one on a stopped stream are each refused.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::iceoryx2::InputMailboxes;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, NodeNotRunningBecauseOnTheWire,
};

use super::*;

/// Each event a [`StreamToolsTestTriggerReader`] has read, by any reader.
static TRIGGER_EVENTS_READ: AtomicUsize = AtomicUsize::new(0);

/// How long fires sent to a playing stream may take to reach the node reading them.
const READ_WITHIN: Duration = Duration::from_secs(10);

/// A native reader of the Trigger `go`, which counts each event it reads, and fires its own
/// Trigger `fired`.
#[streamlib::sdk::node(
    execution = reactive,
    input("go", pin_type = Trigger),
    output("fired", pin_type = Trigger)
)]
pub(super) fn stream_tools_test_trigger_reader(
    inputs: InputMailboxes,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    move |_ctx| {
        while let Some(event) = inputs.read_raw("go")? {
            assert_eq!(
                event.payload.bytes(),
                [0x80],
                "a Trigger event holds the empty map"
            );
            TRIGGER_EVENTS_READ.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// The UUID the graph document gives the `button` whose Trigger no link feeds.
const BUTTON_NODE_UUID: &str = "7b7b7b7b-7b7b-4b7b-8b7b-7b7b7b7b7b7b";

/// The UUID the graph document gives the `follower` whose Trigger a link feeds.
const FOLLOWER_NODE_UUID: &str = "7c7c7c7c-7c7c-4c7c-8c7c-7c7c7c7c7c7c";

/// A project whose stream `main` holds three [`StreamToolsTestTriggerReader`]s: `button`, by
/// [`BUTTON_NODE_UUID`], whose `go` no link feeds, and `leader` linked `fired` to `go` of
/// `follower`, by [`FOLLOWER_NODE_UUID`].
fn a_project_whose_main_stream_holds_an_unlinked_and_a_linked_trigger() -> ProjectWithGraphDocuments
{
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<StreamToolsTestTriggerReader::Node>());
    let reader = StreamToolsTestTriggerReader::node_class_import_path();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": BUTTON_NODE_UUID, "name": "button", "type": reader.as_str() },
            { "id": SOURCE_NODE_UUID, "name": "leader", "type": reader.as_str() },
            { "id": FOLLOWER_NODE_UUID, "name": "follower", "type": reader.as_str() },
        ],
        "links": [
            { "source": { "node": "leader", "port": "fired" },
              "target": { "node": "follower", "port": "go" } },
        ],
    }))
}

/// A `set_node_input_value` firing `go` of `node` in `stream_asset`: no value, in `mode`.
fn a_fire_of(stream_asset: Uuid, node: &str, mode: &str) -> Value {
    json!({ "stream": stream_asset, "node": node, "input": "go", "mode": mode })
}

/// Ten live fires of the unlinked Trigger of a playing stream run its reader ten times, one
/// event each, and leave the graph document and the live graph holding no value of it; a fire
/// carrying a value, one in `commit`, one of the linked Trigger and one beside a rename are each
/// refused, reading no event.
///
/// Mental-revert: a fire taken as a reset clears a value nobody set and reaches no reader; one
/// coalesced like a live value reaches it once, not ten times; one stored like a value renders in
/// the live graph and is announced.
#[tokio::test(flavor = "multi_thread")]
async fn each_live_fire_of_an_unlinked_trigger_runs_its_reader_once_and_the_rest_are_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_unlinked_and_a_linked_trigger();
    engine.playing(&project).await;
    let stream_asset = project.stream_asset_of("main");
    let stream_id = project.stream_id_of("main");
    let saved_before = project.the_graph_document_of("main");
    let read_before = TRIGGER_EVENTS_READ.load(Ordering::SeqCst);

    for _ in 0..10 {
        let fired = engine
            .the_answer_or_the_refusal_of(
                "set_node_input_value",
                a_fire_of(stream_asset, BUTTON_NODE_UUID, "live"),
            )
            .await
            .expect("a live fire of an unlinked Trigger is taken");
        assert_eq!(fired["mode"], "live", "{fired}");
    }
    let gave_up_at = Instant::now() + READ_WITHIN;
    while TRIGGER_EVENTS_READ.load(Ordering::SeqCst) - read_before < 10 {
        assert!(
            Instant::now() < gave_up_at,
            "the reader read {} of ten fires",
            TRIGGER_EVENTS_READ.load(Ordering::SeqCst) - read_before
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(project.the_graph_document_of("main"), saved_before);
    let button_go = the_node_named(&engine.the_live_graph_of(&stream_id), "button")["pins"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|pin| pin["name"] == "go")
        .cloned()
        .unwrap_or(Value::Null);
    assert!(
        button_go.get("value").is_none_or(Value::is_null),
        "a fire holds no value: {button_go}"
    );

    let mut a_fire_with_a_value = a_fire_of(stream_asset, BUTTON_NODE_UUID, "live");
    a_fire_with_a_value["value"] = json!(true);
    let refused_with_their_codes = [
        (
            engine
                .the_answer_or_the_refusal_of("set_node_input_value", a_fire_with_a_value)
                .await,
            ControlOperationRefusalCode::InputValueNotValid,
        ),
        (
            engine
                .the_answer_or_the_refusal_of(
                    "set_node_input_value",
                    a_fire_of(stream_asset, BUTTON_NODE_UUID, "commit"),
                )
                .await,
            ControlOperationRefusalCode::InputTakesNoValue,
        ),
        (
            engine
                .the_answer_or_the_refusal_of(
                    "set_node_input_value",
                    a_fire_of(stream_asset, FOLLOWER_NODE_UUID, "live"),
                )
                .await,
            ControlOperationRefusalCode::InputTakesNoValue,
        ),
        (
            engine
                .the_answer_or_the_refusal_of(
                    "transact",
                    json!({ "mode": "live", "operations": [
                        { "operation": "set_node_input_value", "stream": stream_asset,
                          "node": BUTTON_NODE_UUID, "input": "go" },
                        { "operation": "rename_node", "stream": stream_asset,
                          "node": BUTTON_NODE_UUID, "name": "button renamed" },
                    ] }),
                )
                .await,
            ControlOperationRefusalCode::InputTakesNoValue,
        ),
    ];
    for (refused, code) in refused_with_their_codes {
        let refusal = refused.expect_err("the fire is refused");
        assert_eq!(refusal.refusal_code, code, "{refusal:?}");
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        TRIGGER_EVENTS_READ.load(Ordering::SeqCst) - read_before,
        10,
        "a refused fire reached a reader"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// A fire of a stopped stream's Trigger is refused naming the stopped playback, and so is the
/// first of two fires in one live transaction: two fires of one input are two events, not one
/// input set twice.
///
/// Mental-revert: a fire checked as a reset is refused naming no stopped playback; the set-twice
/// check refuses the second fire as setting the input twice.
#[tokio::test(flavor = "multi_thread")]
async fn a_fire_of_a_stopped_streams_trigger_is_refused_naming_the_stopped_playback() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_unlinked_and_a_linked_trigger();
    engine.playing(&project).await;
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
    let stream_asset = project.stream_asset_of("main");

    let one_fire = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            a_fire_of(stream_asset, BUTTON_NODE_UUID, "live"),
        )
        .await
        .expect_err("a stopped stream's Trigger is refused a fire");
    let a_fire_in_a_transaction = json!({ "operation": "set_node_input_value",
        "stream": stream_asset, "node": BUTTON_NODE_UUID, "input": "go" });
    let two_fires = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "mode": "live",
                    "operations": [a_fire_in_a_transaction.clone(), a_fire_in_a_transaction] }),
        )
        .await
        .expect_err("a stopped stream's Trigger is refused two fires");
    for refusal in [one_fire, two_fires] {
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
        assert_eq!(
            refusal.refused_operation_index.unwrap_or(0),
            0,
            "{refusal:?}"
        );
    }
}
