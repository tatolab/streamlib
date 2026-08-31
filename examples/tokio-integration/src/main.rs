// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A StreamLib graph running inside an existing tokio service.
//!
//! `#[tokio::main]` owns the process and the engine is a guest in it: the
//! engine adopts the ambient tokio handle, the graph is built and observed
//! with the async graph ops, its blocking run loop keeps the process main
//! thread, and async code both reads the graph's bags and decides when it
//! stops.

mod sequenced_tick;
mod sequenced_tick_source;
mod tick_cadence_reporting_sink;

use std::sync::Arc;
use std::time::Duration;

use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::iceoryx2::{FRAME_HEADER_SIZE, source_channel_name};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{Runner, RuntimeOperations};
use streamlib::sdk::serde_json;
use tokio::sync::watch;

use crate::sequenced_tick::SequencedTick;
use crate::sequenced_tick_source::{SequencedTickSource, TICK_OUTPUT_PORT_TO_DOWNSTREAM};
use crate::tick_cadence_reporting_sink::{
    TICK_INPUT_PORT_FROM_UPSTREAM, TickCadenceReportingSink, TickCadenceReportingSinkConfig,
};

/// How long the service runs before its own async code asks the graph to stop.
/// Ctrl-C does the same thing sooner.
const SERVICE_RUN_DURATION: Duration = Duration::from_secs(20);

/// How long the graph gets to bring every processor up. Comfortably shorter
/// than the run duration, so a slow bring-up is reported as one rather than
/// arriving at the same moment as the shutdown deadline.
const GRAPH_STARTUP_BUDGET: Duration = Duration::from_secs(10);

/// How often readiness is re-read while the graph comes up.
const GRAPH_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How often the async side reports the graph's own state back.
const GRAPH_REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// Bags the async consumer collects between reports.
const TICKS_PER_ASYNC_CONSUMER_REPORT: u64 = 25;

/// Ticks the in-graph consumer collects before it reports on their cadence.
const TICKS_PER_CADENCE_REPORT: u32 = 25;

#[tokio::main]
async fn main() -> Result<()> {
    // `Runner::new()` finds this thread's tokio runtime and adopts its handle
    // instead of building a second one. It also installs the engine's logging
    // pathway, which is why every `tracing` call in this app reaches the
    // terminal and the JSONL log with no subscriber set up here.
    let engine_runtime = Runner::new()?;
    tracing::info!("the engine adopted this service's tokio runtime");

    let tick_channel = build_the_tick_graph(&engine_runtime).await?;

    // Sent once, when the run loop returns, so the async jobs below wind
    // themselves up rather than being dropped where they are suspended — the
    // tap in particular has a thread to join.
    let (engine_is_down_sender, engine_is_down_receiver) = watch::channel(false);

    // Spawned before the graph starts; each waits for whatever it needs.
    let tick_consumer = tokio::spawn(consume_ticks_off_the_running_graph(
        Arc::clone(&engine_runtime),
        tick_channel,
        engine_is_down_receiver.clone(),
    ));
    let graph_reporter = tokio::spawn(report_the_graphs_own_state(
        Arc::clone(&engine_runtime),
        engine_is_down_receiver.clone(),
    ));
    let service_deadline = tokio::spawn(stop_the_graph_after(
        Arc::clone(&engine_runtime),
        SERVICE_RUN_DURATION,
        engine_is_down_receiver,
    ));

    // The run loop keeps the process main thread, and this is the one
    // placement the integration has to get right. `#[tokio::main]` drives this
    // future on the main thread while the runtime's workers are threads of
    // their own, so blocking here parks main and starves nothing spawned
    // above. On macOS it is not a preference: the wait becomes the
    // NSApplication event loop, which no other thread is allowed to own.
    let run_outcome = engine_runtime.start_and_wait_for_shutdown();

    let _ = engine_is_down_sender.send(true);
    let (tick_consumer, graph_reporter, service_deadline) =
        tokio::join!(tick_consumer, graph_reporter, service_deadline);
    for (job_name, join_outcome) in [
        ("the tick consumer", tick_consumer),
        ("the graph reporter", graph_reporter),
        ("the service deadline", service_deadline),
    ] {
        if let Err(join_failure) = join_outcome {
            tracing::error!(job_name, error = %join_failure, "an async job ended in a panic");
        }
    }

    match &run_outcome {
        Ok(()) => tracing::info!("the graph and the service came down together"),
        Err(run_failure) => {
            tracing::error!(error = %run_failure, "the graph's run ended in failure");
        }
    }
    run_outcome
}

/// Build the two-processor graph and answer with the channel name a tap needs
/// to reach its link.
async fn build_the_tick_graph(engine_runtime: &Runner) -> Result<String> {
    let sink_config = serde_json::to_value(TickCadenceReportingSinkConfig {
        ticks_per_cadence_report: TICKS_PER_CADENCE_REPORT,
    })
    .map_err(|encode_failure| {
        Error::Configuration(format!("the sink config does not encode: {encode_failure}"))
    })?;

    // `add_local` puts an already-compiled `#[processor]` type on the registry
    // under the import path the type itself carries — no package, no manifest
    // and no build step, because the class is in this binary. It only touches
    // the registry, so async code calls it directly.
    let source_class =
        engine_runtime.add_local::<SequencedTickSource::Processor>(serde_json::Value::Null)?;
    let sink_class =
        engine_runtime.add_local::<TickCadenceReportingSink::Processor>(sink_config.clone())?;

    // Every graph op has a sync spelling and an async one. The sync spelling
    // blocks the calling thread until the reply arrives, so async callers take
    // the `*_async` twin.
    let source_id = engine_runtime
        .add_processor_async(ProcessorSpec::new(source_class, serde_json::Value::Null))
        .await?;
    let sink_id = engine_runtime
        .add_processor_async(ProcessorSpec::new(sink_class, sink_config))
        .await?;
    engine_runtime
        .connect_async(
            OutputLinkPortRef::new(&source_id, TICK_OUTPUT_PORT_TO_DOWNSTREAM),
            InputLinkPortRef::new(&sink_id, TICK_INPUT_PORT_FROM_UPSTREAM),
        )
        .await?;

    // A channel is the source processor's id joined to its output port —
    // derived, never spelled by hand.
    Ok(source_channel_name(source_id.as_str(), TICK_OUTPUT_PORT_TO_DOWNSTREAM)?.into_string())
}

/// The graph's data reaching async code: a read-only tap on the link, awaited
/// bag by bag.
async fn consume_ticks_off_the_running_graph(
    engine_runtime: Arc<Runner>,
    tick_channel: String,
    mut engine_is_down_receiver: watch::Receiver<bool>,
) {
    if let Err(never_came_up) =
        await_every_processor_running(&engine_runtime, &mut engine_is_down_receiver).await
    {
        tracing::error!(error = %never_came_up, "no tap: the graph never came up");
        return;
    }

    let mut tap = match engine_runtime.tap_async(tick_channel.clone(), None).await {
        Ok(tap) => tap,
        Err(attach_failure) => {
            tracing::error!(
                channel = %tick_channel,
                error = %attach_failure,
                "the tap did not attach"
            );
            return;
        }
    };
    tracing::info!(channel = %tick_channel, "async code is tapping the graph's link");

    let mut ticks_received = 0u64;
    loop {
        tokio::select! {
            tapped_bag = tap.recv() => {
                let Some(tapped_bag) = tapped_bag else { break };
                match tick_of_tapped_bag(&tapped_bag) {
                    Ok(tick) => {
                        ticks_received += 1;
                        if ticks_received.is_multiple_of(TICKS_PER_ASYNC_CONSUMER_REPORT) {
                            tracing::info!(
                                ticks_received,
                                newest_sequence_number = tick.sequence_number,
                                dropped_bags = tap.dropped_bags(),
                                "async code read the graph's bags off a tap"
                            );
                        }
                    }
                    Err(decode_failure) => {
                        tracing::warn!(error = %decode_failure, "a tapped bag did not decode");
                    }
                }
            }
            _ = engine_is_down_receiver.changed() => break,
        }
    }

    tracing::info!(ticks_received, "the tap is detaching");
    // Dropping the subscription joins the tap's forwarder thread, so the drop
    // runs off the async worker rather than on one.
    if let Err(join_failure) = tokio::task::spawn_blocking(move || drop(tap)).await {
        tracing::warn!(error = %join_failure, "the tap's detach did not join");
    }
}

/// The graph observed from async code while it runs.
async fn report_the_graphs_own_state(
    engine_runtime: Arc<Runner>,
    mut engine_is_down_receiver: watch::Receiver<bool>,
) {
    let mut report_interval = tokio::time::interval(GRAPH_REPORT_INTERVAL);
    // The first tick of a tokio interval fires immediately, and the graph has
    // nothing to say yet.
    report_interval.tick().await;

    loop {
        tokio::select! {
            _ = report_interval.tick() => match engine_runtime.to_json_async().await {
                Ok(graph) => tracing::info!(
                    processors = %processor_states_of(&graph).join(", "),
                    link_count = graph["links"].as_array().map_or(0, |links| links.len()),
                    "async code read the graph's own state"
                ),
                Err(read_failure) => {
                    tracing::warn!(error = %read_failure, "the graph's state did not read");
                }
            },
            _ = engine_is_down_receiver.changed() => break,
        }
    }
}

/// The service's own deadline, asking the graph to stop.
///
/// A request rather than a teardown: the run loop observes it and runs the
/// normal stop sequence. It is the one sync runtime op that never waits on a
/// reply, which is what makes it safe to call from a task.
async fn stop_the_graph_after(
    engine_runtime: Arc<Runner>,
    run_duration: Duration,
    mut engine_is_down_receiver: watch::Receiver<bool>,
) {
    tokio::select! {
        _ = tokio::time::sleep(run_duration) => {}
        // Ctrl-C got there first.
        _ = engine_is_down_receiver.changed() => return,
    }

    tracing::info!(
        ?run_duration,
        "the service's deadline is asking the graph to stop"
    );
    if let Err(request_failure) =
        engine_runtime.request_runtime_shutdown("the tokio service's run duration elapsed")
    {
        tracing::error!(
            error = %request_failure,
            "the shutdown request did not reach the run loop"
        );
    }
}

/// Wait for every processor to finish `setup` and reach `Running`, by polling
/// the graph's own JSON.
///
/// The engine ships this as `wait_until_every_processor_is_running`, and that
/// is the call to reach for first. Polling buys two things the blocking one
/// cannot give a service: it gives up the moment the engine comes down, and it
/// leaves nothing behind — a `spawn_blocking` wait that outlives its budget is
/// still running when the tokio runtime shuts down, and the runtime waits for
/// it.
///
/// The cost is worth knowing before copying this: `to_json_async` serializes
/// the whole graph under the graph lock, and processor threads need that lock
/// to publish the very transitions being waited on. Cheap at two nodes, not
/// free at fifty.
async fn await_every_processor_running(
    engine_runtime: &Runner,
    engine_is_down_receiver: &mut watch::Receiver<bool>,
) -> Result<()> {
    let give_up_at = tokio::time::Instant::now() + GRAPH_STARTUP_BUDGET;
    loop {
        let graph = engine_runtime.to_json_async().await?;
        if every_processor_is_running(&graph) {
            return Ok(());
        }
        if let Some(settled_short) = a_processor_that_settled_short_of_running(&graph) {
            return Err(Error::Runtime(format!(
                "{settled_short}; every processor: {}",
                processor_states_of(&graph).join(", ")
            )));
        }
        if tokio::time::Instant::now() >= give_up_at {
            return Err(Error::Runtime(format!(
                "the graph was still {} after {GRAPH_STARTUP_BUDGET:?}",
                processor_states_of(&graph).join(", ")
            )));
        }
        tokio::select! {
            _ = tokio::time::sleep(GRAPH_READINESS_POLL_INTERVAL) => {}
            _ = engine_is_down_receiver.changed() => {
                return Err(Error::Runtime(
                    "the engine came down before the graph was up".into(),
                ));
            }
        }
    }
}

/// A tapped bag is the wire form: the engine's frame header, then the msgpack
/// the producer wrote. The engine forwards it verbatim and inspects none of
/// it, so the payload is entirely the reader's to decode.
fn tick_of_tapped_bag(tapped_bag: &[u8]) -> Result<SequencedTick> {
    let payload = tapped_bag.get(FRAME_HEADER_SIZE..).ok_or_else(|| {
        Error::Runtime(format!(
            "a tapped bag of {} bytes is shorter than the {FRAME_HEADER_SIZE}-byte frame header",
            tapped_bag.len()
        ))
    })?;
    rmp_serde::from_slice(payload).map_err(|decode_failure| {
        Error::Runtime(format!("a tapped bag did not decode: {decode_failure}"))
    })
}

/// The graph's processor nodes. The one place that knows the shape of the
/// JSON; an absent or malformed `nodes` reads as no processors at all.
fn processor_nodes_of(graph: &serde_json::Value) -> &[serde_json::Value] {
    graph["nodes"].as_array().map_or(&[], Vec::as_slice)
}

/// Every node as `<display name>=<state>`, read off the graph's own JSON — the
/// same rendering `streamlib graph` serves an API consumer.
fn processor_states_of(graph: &serde_json::Value) -> Vec<String> {
    processor_nodes_of(graph)
        .iter()
        .map(|node| {
            format!(
                "{}={}",
                node["display_name"].as_str().unwrap_or("?"),
                node["components"]["state"].as_str().unwrap_or("?"),
            )
        })
        .collect()
}

/// Whether the graph holds processors and every one of them has finished
/// `setup` and reached `Running`. An empty graph is not up — it is empty.
fn every_processor_is_running(graph: &serde_json::Value) -> bool {
    let nodes = processor_nodes_of(graph);
    !nodes.is_empty()
        && nodes
            .iter()
            .all(|node| node["components"]["state"].as_str() == Some("Running"))
}

/// The first processor whose `setup` resolved into something other than
/// `Running`, as `<display name>=<state>`, and `None` while every processor is
/// either up or still on its way.
///
/// The split is the engine's own readiness predicate: `Pending` and `Idle` are
/// the two states before `setup` finishes, and every later one means it
/// resolved — `Running` if it returned, something else if it did not. Waiting
/// longer cannot move a processor out of one of those, so finding one ends the
/// wait instead of burning the rest of the budget.
fn a_processor_that_settled_short_of_running(graph: &serde_json::Value) -> Option<String> {
    processor_nodes_of(graph).iter().find_map(|node| {
        let state = node["components"]["state"].as_str()?;
        (!matches!(state, "Pending" | "Idle" | "Running")).then(|| {
            format!(
                "{} is {state} rather than Running",
                node["display_name"].as_str().unwrap_or("?")
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_of(nodes: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "nodes": nodes, "links": [] })
    }

    fn node(display_name: &str, state: &str) -> serde_json::Value {
        serde_json::json!({
            "display_name": display_name,
            "components": { "state": state },
        })
    }

    #[test]
    fn a_graph_whose_every_node_is_running_is_up() {
        let graph = graph_of(serde_json::json!([
            node("SequencedTickSource", "Running"),
            node("TickCadenceReportingSink", "Running"),
        ]));

        assert!(every_processor_is_running(&graph));
        assert!(a_processor_that_settled_short_of_running(&graph).is_none());
        assert_eq!(
            processor_states_of(&graph),
            [
                "SequencedTickSource=Running",
                "TickCadenceReportingSink=Running"
            ]
        );
    }

    /// One processor still coming up holds the whole graph back — a tap on a
    /// half-wired graph is the thing this predicate exists to prevent.
    #[test]
    fn one_node_short_of_running_holds_the_graph_back() {
        let graph = graph_of(serde_json::json!([
            node("SequencedTickSource", "Running"),
            node("TickCadenceReportingSink", "Idle"),
        ]));

        assert!(!every_processor_is_running(&graph));
        assert!(
            a_processor_that_settled_short_of_running(&graph).is_none(),
            "Idle is still on its way, so it must not end the wait"
        );
    }

    /// A failed `setup` is final. Waiting the whole budget out for it would
    /// report the right thing far too late.
    #[test]
    fn a_processor_that_failed_setup_ends_the_wait_rather_than_holding_it() {
        let graph = graph_of(serde_json::json!([
            node("SequencedTickSource", "Running"),
            node("TickCadenceReportingSink", "Error"),
        ]));

        assert!(!every_processor_is_running(&graph));
        assert_eq!(
            a_processor_that_settled_short_of_running(&graph),
            Some("TickCadenceReportingSink is Error rather than Running".to_string())
        );
    }

    /// Vacuous truth would report an empty graph as up, and the readiness wait
    /// would return before a single processor existed.
    #[test]
    fn an_empty_graph_is_not_up() {
        assert!(!every_processor_is_running(&graph_of(
            serde_json::json!([])
        )));
        assert!(!every_processor_is_running(&serde_json::json!({})));
        assert!(processor_nodes_of(&serde_json::json!({})).is_empty());
    }

    /// A bag shorter than the header is a truncation, not a decode failure —
    /// slicing it would panic.
    #[test]
    fn a_bag_shorter_than_the_frame_header_is_refused_rather_than_sliced() {
        let too_short = vec![0u8; FRAME_HEADER_SIZE - 1];

        let refusal = tick_of_tapped_bag(&too_short).expect_err("a truncated bag has no tick");

        assert!(
            refusal.to_string().contains("frame header"),
            "the refusal should name the frame header, got {refusal}"
        );
    }

    #[test]
    fn a_bag_carrying_a_tick_decodes_past_the_frame_header() {
        let tick = SequencedTick {
            sequence_number: 7,
            emitted_at_monotonic_ns: 123_456_789,
        };
        let mut bag = vec![0u8; FRAME_HEADER_SIZE];
        bag.extend_from_slice(&rmp_serde::to_vec_named(&tick).expect("a tick encodes"));

        assert_eq!(tick_of_tapped_bag(&bag).expect("the bag decodes"), tick);
    }
}
