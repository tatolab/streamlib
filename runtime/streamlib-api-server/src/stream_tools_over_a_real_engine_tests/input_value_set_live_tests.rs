// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value set live on a running stream is not a graph edit: it renders no graph, starts no
//! compile and publishes no graph change; `graph` renders where each input's value comes from;
//! a connection subscribed to the stream's input value changes is sent each input at most once
//! each 33 ms and ends on the last value, and one that follows the graph alone is sent `edited`
//! at most once each 500 ms.

use std::time::Instant;

use parking_lot::Mutex;
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};
use streamlib::sdk::runtime::graph_renders_and_commits_counted_for_a_test::{
    commits_begun_of, graph_renders_of,
};

use super::input_value_set_tests::{
    MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer, the_input_named,
};
use super::*;
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

/// How many live sets a run sends, and how far apart.
const LIVE_SETS_SENT: u32 = 600;
const ONE_LIVE_SET_EVERY: Duration = Duration::from_micros(16_667);

/// What one stream's topic carried: graph changes and input value changes.
#[derive(Default)]
struct EventsOfOneStreamCounted {
    graph_changes: u64,
    input_value_changes: u64,
}

/// How long a commit a graph change spawns is given to begin before a test reads that none did.
const A_COMMIT_SPAWNED_BEGINS_WITHIN: Duration = Duration::from_millis(500);

impl EventListener for EventsOfOneStreamCounted {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        if let Event::OfALoadedStream { event, .. } = event {
            match event {
                RuntimeEvent::GraphDidChange => self.graph_changes += 1,
                RuntimeEvent::InputValueDidChange { .. } => self.input_value_changes += 1,
                _ => {}
            }
        }
        Ok(())
    }

    fn on_events_lost(&mut self, lost_event_count: u64) -> Result<()> {
        panic!("the stream's topic lost {lost_event_count} events, which this count needs");
    }
}

/// The project holding a mixer, opened and its stream `main` started, served over a fresh
/// socket; answers the project, the stream id and the control plane serving it.
async fn a_started_stream_holding_a_mixer(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
) -> (
    ProjectWithGraphDocuments,
    String,
    crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient,
) {
    let project = a_project_whose_main_stream_holds_a_mixer();
    let (stream_id, served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    (project, stream_id, served)
}

/// Send [`LIVE_SETS_SENT`] live values of `gain` to the mixer of `stream_asset`, one each
/// [`ONE_LIVE_SET_EVERY`], answering the last value sent.
async fn send_live_gains_at_sixty_a_second(
    served: &crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient,
    stream_asset: Uuid,
) -> f64 {
    let mut each_set = tokio::time::interval(ONE_LIVE_SET_EVERY);
    let mut gain = 0.0;
    for set in 1..=LIVE_SETS_SENT {
        each_set.tick().await;
        gain = f64::from(set) / f64::from(LIVE_SETS_SENT);
        let answered = served
            .control_call(
                "set_node_input_value",
                json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                        "value": gain, "mode": "live" }),
            )
            .await;
        assert_eq!(answered["result"]["mode"], "live", "{answered}");
    }
    gain
}

/// Six hundred live sets at sixty a second on a running stream render no graph, start no
/// compile and publish no graph change; each is announced as an input value change; `graph`
/// then renders the last value as live, the document is unchanged, and a commit of the input
/// renders it as saved and drops the live value, beginning no compile either.
///
/// Mental-revert: a live set through the graph's path renders the graph to stage it and
/// publishes `GraphDidChange`, which begins a commit; so does a commit that announces a graph
/// change; leaving the live value in place after the commit renders the committed input as still
/// live.
#[tokio::test(flavor = "multi_thread")]
async fn six_hundred_live_sets_render_no_graph_start_no_compile_and_change_no_graph() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, stream_id, served) = a_started_stream_holding_a_mixer(&engine).await;
    let stream_asset = project.stream_asset_of("main");
    let saved_before = project.the_graph_document_of("main");
    let loaded_stream = engine
        .operations_on_the_loaded_streams()
        .loaded_stream_identity_of_the_stream_a_call_names(&stream_id)
        .expect("the stream is loaded");
    let counted = Arc::new(Mutex::new(EventsOfOneStreamCounted::default()));
    PUBSUB
        .subscribe(
            &topics::loaded_stream(&loaded_stream),
            Arc::clone(&counted) as Arc<Mutex<dyn EventListener>>,
        )
        .expect("the stream's topic is subscribed");
    let pins_before =
        the_node_named(&engine.the_live_graph_of(&stream_id), "mixer")["pins"].clone();
    let graph_renders_before = graph_renders_of(&loaded_stream);
    let commits_begun_before = commits_begun_of(&loaded_stream);

    let last_gain = send_live_gains_at_sixty_a_second(&served, stream_asset).await;

    assert_eq!(graph_renders_of(&loaded_stream), graph_renders_before);
    let deadline = Instant::now() + Duration::from_secs(5);
    while counted.lock().input_value_changes < u64::from(LIVE_SETS_SENT)
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(A_COMMIT_SPAWNED_BEGINS_WITHIN).await;
    assert_eq!(
        commits_begun_of(&loaded_stream),
        commits_begun_before,
        "a live set began a commit"
    );
    {
        let counted = counted.lock();
        assert_eq!(counted.graph_changes, 0, "a live set changed the graph");
        assert_eq!(counted.input_value_changes, u64::from(LIVE_SETS_SENT));
    }
    assert_eq!(project.the_graph_document_of("main"), saved_before);
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    let gain = the_input_named(&mixer, "gain");
    assert_eq!(gain["value"], json!(last_gain), "{gain}");
    assert_eq!(gain["value_comes_from"], "live", "{gain}");
    assert_eq!(
        mixer["pins"], pins_before,
        "pins stay the saved values: {mixer}"
    );
    let video = the_input_named(&mixer, "video");
    assert!(
        video.get("value").is_none() && video.get("value_comes_from").is_none(),
        "an Image input holds no value: {video}"
    );

    let committed = served
        .control_call(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                    "value": 0.125, "author": an_agent_named("alice") }),
        )
        .await;
    assert!(
        committed["result"]["transaction"].is_string(),
        "{committed}"
    );
    let gain = the_input_named(
        &the_node_named(&engine.the_live_graph_of(&stream_id), "mixer"),
        "gain",
    );
    assert_eq!(gain["value"], json!(0.125), "{gain}");
    assert_eq!(gain["value_comes_from"], "saved", "{gain}");
    tokio::time::sleep(A_COMMIT_SPAWNED_BEGINS_WITHIN).await;
    assert_eq!(
        commits_begun_of(&loaded_stream),
        commits_begun_before,
        "a commit of a value read each run began a compile"
    );
    assert_eq!(
        counted.lock().graph_changes,
        0,
        "a commit of a value read each run changed the graph"
    );
}

/// Every message `connection` is sent until `until`, each with the time it arrived.
async fn every_message_until(
    mut connection: crate::control_json_rpc_adapter::tests::ControlWebSocketMessages,
    until: tokio::time::Instant,
) -> Vec<(Instant, Value)> {
    let mut messages = Vec::new();
    while let Ok(message) = tokio::time::timeout_at(until, connection.next_message()).await {
        messages.push((Instant::now(), message));
    }
    messages
}

/// A connection subscribed to a stream's graph and its input value changes is sent no `edited`
/// over six hundred live sets at sixty a second, and `input_value_changed` for the input at most
/// once each 33 ms, ending on the last value; a connection that follows the graph alone is
/// sent `edited` at most once each 500 ms and no `input_value_changed`.
///
/// Mental-revert: forwarding each value change as heard sends six hundred notifications; sending
/// `edited` to the subscribed connection makes its client read the whole graph for each set;
/// sending nothing to the connection that follows the graph alone leaves an older client stale.
#[tokio::test(flavor = "multi_thread")]
async fn a_subscribed_follower_is_sent_each_input_at_most_once_in_33_ms_and_another_edited() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, stream_id, served) = a_started_stream_holding_a_mixer(&engine).await;
    let stream_asset = project.stream_asset_of("main");

    let mut follower = served.control_websocket().await;
    for (id, method) in [
        (1, "subscribe_stream_graph_changes"),
        (2, "subscribe_input_value_changes"),
    ] {
        follower
            .send(json!({ "jsonrpc": "2.0", "id": id, "method": method,
                          "params": { "streams": [stream_id] } }))
            .await;
        assert_eq!(
            the_next_answer_but_its_notifications(&mut follower).await["result"]["streams"],
            json!([stream_id]),
            "{method}"
        );
    }
    let mut graph_follower = served.control_websocket().await;
    graph_follower
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_stream_graph_changes",
                      "params": { "streams": [stream_id] } }),
        )
        .await;
    assert_eq!(
        the_next_answer_but_its_notifications(&mut graph_follower).await["result"]["streams"],
        json!([stream_id])
    );

    let sent_from = Instant::now();
    let until =
        tokio::time::Instant::now() + ONE_LIVE_SET_EVERY * LIVE_SETS_SENT + Duration::from_secs(4);
    let follower_messages = tokio::spawn(every_message_until(follower, until));
    let graph_follower_messages = tokio::spawn(every_message_until(graph_follower, until));
    let last_gain = send_live_gains_at_sixty_a_second(&served, stream_asset).await;
    let sent_for = sent_from.elapsed();
    let follower_messages = follower_messages.await.expect("the follower is read");
    let graph_follower_messages = graph_follower_messages
        .await
        .expect("the graph follower is read");

    let notifications_under =
        |messages: &[(Instant, Value)], method: &str, change: Option<&str>| {
            messages
                .iter()
                .filter(|(_, message)| message["method"] == method)
                .filter(|(_, message)| {
                    change.is_none_or(|change| message["params"]["change"] == change)
                })
                .map(|(arrived, message)| (*arrived, message["params"].clone()))
                .collect::<Vec<_>>()
        };
    let most_sent_each =
        |interval: Duration| (sent_for.as_millis() / interval.as_millis()) as usize + 2;

    assert!(
        notifications_under(&follower_messages, "stream_graph_changed", Some("edited")).is_empty(),
        "a subscribed follower was sent `edited` for a value set live"
    );
    let input_value_changes = notifications_under(&follower_messages, "input_value_changed", None);
    assert!(
        !input_value_changes.is_empty()
            && input_value_changes.len() <= most_sent_each(Duration::from_millis(33)),
        "{} input value changes over {sent_for:?}",
        input_value_changes.len()
    );
    let (_, last_change) = input_value_changes.last().expect("one change was sent");
    assert_eq!(
        last_change,
        &json!({ "stream": stream_id, "node": MIXER_NODE_UUID, "input": "gain",
                 "value": last_gain, "value_comes_from": "live" })
    );

    assert!(
        notifications_under(&graph_follower_messages, "input_value_changed", None).is_empty(),
        "a connection that did not subscribe was sent `input_value_changed`"
    );
    let edited = notifications_under(
        &graph_follower_messages,
        "stream_graph_changed",
        Some("edited"),
    );
    assert!(
        !edited.is_empty() && edited.len() <= most_sent_each(Duration::from_millis(500)),
        "{} `edited` over {sent_for:?}",
        edited.len()
    );
}

/// Live sets sent on one connection without waiting for their answers all end on the last one
/// made: `graph` renders the value the connection was last sent as `input_value_changed`.
///
/// Mental-revert: with no change of an input value made one at a time, a set announced after
/// the lock is released can be announced after a later one, and a follower ends on a value the
/// graph no longer holds.
#[tokio::test(flavor = "multi_thread")]
async fn live_sets_sent_at_once_end_on_the_last_one_made() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, stream_id, served) = a_started_stream_holding_a_mixer(&engine).await;
    let stream_asset = project.stream_asset_of("main");
    let mut connection = served.control_websocket().await;
    connection
        .send(
            json!({ "jsonrpc": "2.0", "id": 0, "method": "subscribe_input_value_changes",
                      "params": { "streams": [stream_id] } }),
        )
        .await;
    assert_eq!(
        the_next_answer_but_its_notifications(&mut connection).await["result"]["streams"],
        json!([stream_id])
    );

    const SETS_SENT_AT_ONCE: u32 = 200;
    for set in 1..=SETS_SENT_AT_ONCE {
        connection
            .send(
                json!({ "jsonrpc": "2.0", "id": set, "method": "set_node_input_value",
                          "params": { "stream": stream_asset, "node": MIXER_NODE_UUID,
                                      "input": "gain", "mode": "live",
                                      "value": f64::from(set) / f64::from(SETS_SENT_AT_ONCE) } }),
            )
            .await;
    }
    let messages = every_message_until(
        connection,
        tokio::time::Instant::now() + Duration::from_secs(5),
    )
    .await;

    let answers: Vec<&Value> = messages
        .iter()
        .map(|(_, message)| message)
        .filter(|message| message.get("id").is_some())
        .collect();
    assert_eq!(answers.len(), SETS_SENT_AT_ONCE as usize);
    assert!(
        answers
            .iter()
            .all(|answer| answer["result"]["mode"] == "live"),
        "{answers:?}"
    );
    let last_sent = messages
        .iter()
        .rev()
        .map(|(_, message)| message)
        .find(|message| message["method"] == "input_value_changed")
        .expect("a change was sent");
    let gain = the_input_named(
        &the_node_named(&engine.the_live_graph_of(&stream_id), "mixer"),
        "gain",
    );
    assert_eq!(gain["value_comes_from"], "live", "{gain}");
    assert_eq!(
        last_sent["params"]["value"], gain["value"],
        "{last_sent} {gain}"
    );
}

/// A live value its input does not take is refused naming the place of the value, and nothing
/// is set; a live transaction mixing a value set with another action takes the path a graph edit
/// takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_live_set_sets_nothing_and_a_mixed_live_transaction_is_staged() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (project, stream_id, _served) = a_started_stream_holding_a_mixer(&engine).await;
    let stream_asset = project.stream_asset_of("main");
    let loaded_stream = engine
        .operations_on_the_loaded_streams()
        .loaded_stream_identity_of_the_stream_a_call_names(&stream_id)
        .expect("the stream is loaded");
    let gain_before = the_input_named(
        &the_node_named(&engine.the_live_graph_of(&stream_id), "mixer"),
        "gain",
    );

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "gain",
                    "value": "loud", "mode": "live" }),
        )
        .await
        .expect_err("the value is refused");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputValueNotValid,
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_params_pointer.as_deref(),
        Some("/value"),
        "{refusal:?}"
    );
    assert_eq!(
        the_input_named(
            &the_node_named(&engine.the_live_graph_of(&stream_id), "mixer"),
            "gain"
        ),
        gain_before
    );

    let graph_renders_before = graph_renders_of(&loaded_stream);
    let applied = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "mode": "live", "operations": [
                { "operation": "set_node_input_value", "stream": stream_asset,
                  "node": MIXER_NODE_UUID, "input": "gain", "value": 0.4 },
                { "operation": "rename_node", "stream": stream_asset,
                  "node": MIXER_NODE_UUID, "name": "mixer renamed" },
            ] }),
        )
        .await
        .expect("the mixed live transaction is applied");
    assert_eq!(applied["mode"], "live", "{applied}");
    assert!(
        graph_renders_of(&loaded_stream) > graph_renders_before,
        "a mixed live transaction takes the staged path, which renders the graph"
    );
    let gain = the_input_named(
        &the_node_named(&engine.the_live_graph_of(&stream_id), "mixer-renamed"),
        "gain",
    );
    assert_eq!(gain["value"], json!(0.4), "{gain}");
}
