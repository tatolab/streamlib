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
use streamlib::sdk::runtime::graph_renders_counted_for_a_test::graph_renders_of;

use super::input_value_set_tests::{MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer};
use super::*;

/// How many live sets a run sends, and how far apart.
const LIVE_SETS_SENT: u32 = 600;
const ONE_LIVE_SET_EVERY: Duration = Duration::from_micros(16_667);

/// What one stream's topic carried: compiles begun, graph changes and input value changes.
#[derive(Default)]
struct EventsOfOneStreamCounted {
    compiles: u64,
    graph_changes: u64,
    input_value_changes: u64,
}

impl EventListener for EventsOfOneStreamCounted {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        if let Event::OfALoadedStream { event, .. } = event {
            match event {
                RuntimeEvent::CompilerWillCompile => self.compiles += 1,
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
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    the_run_of_the_project_once_finished(&operations_on_the_loaded_streams, project.path()).await;
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        Arc::clone(&operations_on_the_loaded_streams),
    )
    .await;
    let queued = served
        .control_call(
            "start_playback",
            json!({ "project_directory": project.path() }),
        )
        .await;
    let action = queued["result"]["action"]
        .as_str()
        .unwrap_or_else(|| panic!("start_playback is queued: {queued}"))
        .to_owned();
    let started = the_stream_action_once_finished(&operations_on_the_loaded_streams, &action).await;
    assert_eq!(started["state"], "done", "{started}");
    let stream_id = project.stream_id_of("main");
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

/// The input `input` of `rendered_node`, a node of a `graph` render.
fn the_input_named(rendered_node: &Value, input: &str) -> Value {
    rendered_node["ports"]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|port| port["name"] == input)
        .cloned()
        .unwrap_or_else(|| panic!("the node renders no input `{input}`: {rendered_node}"))
}

/// Six hundred live sets at sixty a second on a running stream render no graph, start no
/// compile and publish no graph change; each is announced as an input value change; `graph`
/// then renders the last value as live, the document is unchanged, and a commit of the input
/// renders it as saved and drops the live value.
///
/// Mental-revert: a live set through the graph's path renders the graph to stage it and
/// publishes `GraphDidChange`, which starts a commit; leaving the live value in place after the
/// commit renders the committed input as still live.
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
    let graph_renders_before = graph_renders_of(&loaded_stream);

    let last_gain = send_live_gains_at_sixty_a_second(&served, stream_asset).await;

    assert_eq!(graph_renders_of(&loaded_stream), graph_renders_before);
    let deadline = Instant::now() + Duration::from_secs(5);
    while counted.lock().input_value_changes < u64::from(LIVE_SETS_SENT)
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    {
        let counted = counted.lock();
        assert_eq!(counted.compiles, 0, "a live set started a compile");
        assert_eq!(counted.graph_changes, 0, "a live set changed the graph");
        assert_eq!(counted.input_value_changes, u64::from(LIVE_SETS_SENT));
    }
    assert_eq!(project.the_graph_document_of("main"), saved_before);
    let mixer = the_node_named(&engine.the_live_graph_of(&stream_id), "mixer");
    let gain = the_input_named(&mixer, "gain");
    assert_eq!(gain["value"], json!(last_gain), "{gain}");
    assert_eq!(gain["value_comes_from"], "live", "{gain}");
    assert_eq!(mixer["pins"], json!({ "gain": last_gain }), "{mixer}");
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
    assert_eq!(
        counted.lock().compiles,
        0,
        "a commit of a pin started a compile"
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
        &json!({ "stream": stream_id, "node": MIXER_NODE_UUID, "input": "gain", "member": null,
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
