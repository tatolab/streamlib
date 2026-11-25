// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A live value follows its input's lifetime at Stop, over a real engine: a `kept` input's live
//! value is still set after Stop and after the runtime restarts, a `reset` input's is gone at
//! Stop and the input reads its saved value, and a live set on a stopped stream is held for a
//! `kept` input and refused for a `reset` one.

use parking_lot::Mutex;
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, ControlOperationRefusalOnTheWire, NodeNotRunningBecauseOnTheWire,
};

use super::input_value_set_tests::the_input_named;
use super::*;

/// A native node with a Float input `gain`, whose live value is kept, and a Float input
/// `flash`, whose live value is reset at Stop.
#[streamlib::sdk::node(
    execution = manual,
    input("gain", delivery_profile = "newest", pin_type = Float),
    input("flash", delivery_profile = "newest", pin_type = Float, lifetime = reset),
)]
pub(super) struct InputLifetimeTestNode;

impl ManualNode for InputLifetimeTestNode::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn register_the_input_lifetime_test_node_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<InputLifetimeTestNode::Node>());
}

const DIMMER_NODE_UUID: &str = "7c7c7c7c-7c7c-4c7c-8c7c-7c7c7c7c7c7c";

/// A project whose stream `main` holds an [`InputLifetimeTestNode`] `dimmer`, by
/// [`DIMMER_NODE_UUID`], whose `flash` is saved at 0.1.
fn a_project_whose_main_stream_holds_a_dimmer() -> ProjectWithGraphDocuments {
    register_the_input_lifetime_test_node_once();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": DIMMER_NODE_UUID, "name": "dimmer",
              "type": InputLifetimeTestNode::node_class_import_path().as_str(),
              "input_values": { "flash": 0.1 } },
        ],
        "links": [],
    }))
}

/// Every input value change one stream's topic carried, as `(input, value read)`.
#[derive(Default)]
struct InputValueChangesOfOneStream(Vec<(String, Value)>);

impl EventListener for InputValueChangesOfOneStream {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        if let Event::OfALoadedStream {
            event:
                RuntimeEvent::InputValueDidChange {
                    input, value_read, ..
                },
            ..
        } = event
        {
            self.0.push((
                input.clone(),
                serde_json::to_value(value_read).expect("a value read serializes"),
            ));
        }
        Ok(())
    }

    fn on_events_lost(&mut self, lost_event_count: u64) -> Result<()> {
        panic!("the stream's topic lost {lost_event_count} events, which this test reads");
    }
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// Set `value` live on input `input` of the dimmer of `project`'s stream `main`.
    async fn set_live_on_the_dimmer(
        &self,
        project: &ProjectWithGraphDocuments,
        input: &str,
        value: f64,
    ) -> std::result::Result<Value, Box<ControlOperationRefusalOnTheWire>> {
        self.the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "node": DIMMER_NODE_UUID,
                    "input": input, "value": value, "mode": "live" }),
        )
        .await
    }

    /// What input `input` of the dimmer of the stream `stream_id` renders: its value and where
    /// it comes from.
    fn what_the_dimmer_reads(&self, stream_id: &str, input: &str) -> (Value, Value) {
        let rendered = the_input_named(
            &the_node_named(&self.the_live_graph_of(stream_id), "dimmer"),
            input,
        );
        (
            rendered["value"].clone(),
            rendered["value_comes_from"].clone(),
        )
    }

    /// Run `tool_name` on `project` and wait until it is done.
    pub(super) async fn the_project_action_done(
        &self,
        tool_name: &str,
        project: &ProjectWithGraphDocuments,
    ) {
        let finished = the_project_action_once_finished(
            tool_name,
            &self.operations_on_the_loaded_streams(),
            project.path(),
        )
        .await;
        assert_eq!(finished["state"], "done", "{tool_name}: {finished}");
    }
}

/// Live values set on a playing stream follow their inputs' lifetimes at Stop: `gain`, kept,
/// still reads its live value; `flash`, reset, reads its saved value again and its followers are
/// told so. While stopped, a live `gain` is held and a live `flash` refused naming the stopped
/// playback. After the project closes and a new runtime opens it, `gain` reads the live value
/// it held.
///
/// Mental-revert: holding no live value across the Stop's teardown reads `gain` as saved;
/// holding every one keeps `flash` live; dropping the stopped check holds the live `flash`;
/// mirroring nothing into the project's store reads `gain` as default after the restart.
#[tokio::test(flavor = "multi_thread")]
async fn a_kept_live_value_survives_stop_and_a_restart_and_a_reset_one_is_gone_at_stop() {
    let project = a_project_whose_main_stream_holds_a_dimmer();
    let stream_id = project.stream_id_of("main");
    {
        let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
        the_run_of_the_project_once_finished(
            &engine.operations_on_the_loaded_streams(),
            project.path(),
        )
        .await;
        engine
            .the_project_action_done("start_playback", &project)
            .await;
        for (input, value) in [("gain", 0.7), ("flash", 0.9)] {
            engine
                .set_live_on_the_dimmer(&project, input, value)
                .await
                .unwrap_or_else(|refusal| {
                    panic!("a playing stream takes a live {input}: {refusal:?}")
                });
        }
        let loaded_stream = engine
            .operations_on_the_loaded_streams()
            .loaded_stream_identity_of_the_stream_a_call_names(&stream_id)
            .expect("the stream is loaded");
        let changes = Arc::new(Mutex::new(InputValueChangesOfOneStream::default()));
        PUBSUB
            .subscribe(
                &topics::loaded_stream(&loaded_stream),
                Arc::clone(&changes) as Arc<Mutex<dyn EventListener>>,
            )
            .expect("the stream's topic is subscribed");

        engine
            .the_project_action_done("stop_playback", &project)
            .await;

        assert_eq!(
            engine.what_the_dimmer_reads(&stream_id, "gain"),
            (json!(0.7), json!("live"))
        );
        assert_eq!(
            engine.what_the_dimmer_reads(&stream_id, "flash"),
            (json!(0.1), json!("saved"))
        );
        assert_eq!(
            changes.lock().0,
            [(
                "flash".to_owned(),
                json!({ "value": 0.1, "value_comes_from": "saved" })
            )]
        );

        engine
            .set_live_on_the_dimmer(&project, "gain", 0.4)
            .await
            .expect("a stopped stream holds a kept input's live value for its next Play");
        let refusal = engine
            .set_live_on_the_dimmer(&project, "flash", 0.5)
            .await
            .expect_err("a stopped stream refuses a reset input's live value");
        assert_eq!(
            refusal.refusal_code,
            ControlOperationRefusalCode::NodeNotRunning
        );
        assert_eq!(
            refusal.not_running_because,
            Some(NodeNotRunningBecauseOnTheWire::PlaybackStopped),
            "{refusal:?}"
        );
        assert_eq!(
            engine.what_the_dimmer_reads(&stream_id, "flash"),
            (json!(0.1), json!("saved"))
        );

        engine
            .the_project_action_done("close_project", &project)
            .await;
    }

    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    the_run_of_the_project_once_finished(
        &restarted.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;
    assert_eq!(
        restarted.what_the_dimmer_reads(&stream_id, "gain"),
        (json!(0.4), json!("live"))
    );
    assert_eq!(
        restarted.what_the_dimmer_reads(&stream_id, "flash"),
        (json!(0.1), json!("saved"))
    );
}

/// A node taken out of a playing stream takes its kept live values with it: a new runtime opening
/// the project after an undo puts the node back reads its input as saved.
///
/// Mental-revert: leaving the removed node's keys in the project's store reads `gain` as live
/// after the restart.
#[tokio::test(flavor = "multi_thread")]
async fn a_removed_node_takes_its_kept_live_values_with_it() {
    the_dimmer_reads_no_live_gain_after_its_removal_is_undone_and_the_project_reopened(
        PlaybackAtTheRemoval::Playing,
    )
    .await;
}

/// A node taken out of a stopped stream, which drops it with no commit, takes its kept live
/// values with it too.
///
/// Mental-revert: tearing the node down as at a Stop on that drop leaves its key in the
/// project's store and reads `gain` as live after the restart.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_removed_from_a_stopped_stream_takes_its_kept_live_values_with_it() {
    the_dimmer_reads_no_live_gain_after_its_removal_is_undone_and_the_project_reopened(
        PlaybackAtTheRemoval::Stopped,
    )
    .await;
}

/// Whether the stream plays when its dimmer is taken out.
enum PlaybackAtTheRemoval {
    Playing,
    Stopped,
}

/// Set `gain` live on a playing dimmer, take the dimmer out with the playback as `playback`
/// says, undo, close, and assert a new runtime opening the project reads no live `gain`.
async fn the_dimmer_reads_no_live_gain_after_its_removal_is_undone_and_the_project_reopened(
    playback: PlaybackAtTheRemoval,
) {
    let project = a_project_whose_main_stream_holds_a_dimmer();
    let stream_id = project.stream_id_of("main");
    {
        let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
        the_run_of_the_project_once_finished(
            &engine.operations_on_the_loaded_streams(),
            project.path(),
        )
        .await;
        engine
            .the_project_action_done("start_playback", &project)
            .await;
        engine
            .set_live_on_the_dimmer(&project, "gain", 0.7)
            .await
            .expect("a playing stream takes a live gain");
        if let PlaybackAtTheRemoval::Stopped = playback {
            engine
                .the_project_action_done("stop_playback", &project)
                .await;
            assert_eq!(
                engine.what_the_dimmer_reads(&stream_id, "gain"),
                (json!(0.7), json!("live")),
                "a kept live value is held across the Stop"
            );
        }
        engine
            .the_answer_or_the_refusal_of(
                "remove_node",
                json!({ "stream": project.stream_asset_of("main"), "node": DIMMER_NODE_UUID,
                        "author": an_agent_named("alice") }),
            )
            .await
            .expect("the dimmer is taken out");
        engine
            .the_answer_or_the_refusal_of(
                "undo",
                json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
            )
            .await
            .expect("the removal is undone");
        engine
            .the_project_action_done("close_project", &project)
            .await;
    }

    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    the_run_of_the_project_once_finished(
        &restarted.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;
    assert_eq!(
        restarted.what_the_dimmer_reads(&stream_id, "gain"),
        (Value::Null, Value::Null)
    );
}

/// The table of a project's store the live values of `kept` inputs are mirrored into.
/// Spelled out, with the `<node>/<input>` keys below, to pin the on-disk format from outside.
const LIVE_INPUT_VALUES_TABLE: streamlib_project_repository::ProjectStoreTableName =
    streamlib_project_repository::ProjectStoreTableName::named("live_input_values");

/// A stream loading its graph holds as live only the values its project's store keeps for an
/// input that still keeps a live value and takes it, and forgets every other one there: one on a
/// `reset` input, one on an input the node does not have, one its input no longer takes, and
/// one on a setting its node holds in its config.
///
/// Mental-revert: holding each kept value without its checks reads `flash` as live; dropping the
/// forgetting of a value that no longer fits leaves its key in the store.
#[tokio::test(flavor = "multi_thread")]
async fn a_kept_value_that_no_longer_fits_its_input_is_forgotten_as_the_stream_loads() {
    register_the_input_lifetime_test_node_once();
    super::input_value_set_tests::register_the_input_value_test_node_once();
    let mixer = super::input_value_set_tests::MIXER_NODE_UUID;
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": DIMMER_NODE_UUID, "name": "dimmer",
              "type": InputLifetimeTestNode::node_class_import_path().as_str(),
              "input_values": { "flash": 0.1 } },
            { "id": mixer, "name": "mixer",
              "type": super::input_value_test_node::InputValueTestNode::node_class_import_path()
                  .as_str() },
        ],
        "links": [],
    }));
    let stream_id = project.stream_id_of("main");
    {
        let project_store =
            streamlib_project_repository::ProjectStore::open_in_the_project_at(project.path())
                .expect("the project's store opens");
        for (key, value) in [
            (format!("{DIMMER_NODE_UUID}/flash"), json!(0.9)),
            (format!("{DIMMER_NODE_UUID}/absent"), json!(0.3)),
            (format!("{DIMMER_NODE_UUID}/gain"), json!("loud")),
            (format!("{mixer}/frame_width"), json!(640)),
            (format!("{mixer}/gain"), json!(0.6)),
        ] {
            project_store
                .write(LIVE_INPUT_VALUES_TABLE, &key, &value)
                .expect("the stale value is written");
        }
    }

    {
        let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
        the_run_of_the_project_once_finished(
            &engine.operations_on_the_loaded_streams(),
            project.path(),
        )
        .await;
        let graph = engine.the_live_graph_of(&stream_id);
        let reads = |node: &str, input: &str| {
            let rendered = the_input_named(&the_node_named(&graph, node), input);
            (
                rendered["value"].clone(),
                rendered["value_comes_from"].clone(),
            )
        };
        assert_eq!(reads("mixer", "gain"), (json!(0.6), json!("live")));
        assert_eq!(reads("dimmer", "flash"), (json!(0.1), json!("saved")));
        assert_eq!(reads("dimmer", "gain"), (Value::Null, Value::Null));
        engine
            .the_project_action_done("close_project", &project)
            .await;
    }

    let project_store =
        streamlib_project_repository::ProjectStore::open_in_the_project_at(project.path())
            .expect("the project's store opens again");
    assert_eq!(
        project_store
            .keys_in(LIVE_INPUT_VALUES_TABLE)
            .expect("the kept values list"),
        [format!("{mixer}/gain")]
    );
}
