// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A StreamLib graph running inside an existing tokio service.
//!
//! `#[tokio::main]` owns the process and the engine is a guest in it: the
//! runtime adopts the ambient tokio handle, the graph is built and observed
//! with the async graph ops, its blocking run loop lives on the blocking pool,
//! and async code both reads the graph's bags and decides when it stops.

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

/// How long the graph gets to bring every processor up before the async
/// observers give up on it.
const GRAPH_STARTUP_BUDGET: Duration = Duration::from_secs(20);

/// How often readiness is re-read while the graph comes up.
const GRAPH_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

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
    let runtime = Runner::new()?;
    tracing::info!("the engine adopted this service's tokio runtime");

    let tick_channel = build_the_tick_graph(&runtime).await?;

    // The run loop owns the shutdown signals and then polls until one lands.
    // It blocks, so it goes to the blocking pool rather than parking an async
    // worker for the whole run.
    let graph_run = tokio::task::spawn_blocking({
        let runtime = Arc::clone(&runtime);
        move || runtime.start_and_wait_for_shutdown()
    });

    // Sent once, when the run loop returns, so the async jobs below wind
    // themselves up rather than being aborted at an await point — the tap in
    // particular has a thread to join.
    let (engine_is_down_sender, engine_is_down) = watch::channel(false);

    let tick_consumer = tokio::spawn(consume_ticks_off_the_running_graph(
        Arc::clone(&runtime),
        tick_channel,
        engine_is_down.clone(),
    ));
    let graph_reporter = tokio::spawn(report_the_graphs_own_state(
        Arc::clone(&runtime),
        engine_is_down.clone(),
    ));
    let service_deadline = tokio::spawn(stop_the_graph_after(
        Arc::clone(&runtime),
        SERVICE_RUN_DURATION,
        engine_is_down,
    ));

    let run_outcome = graph_run.await.map_err(|join_failure| {
        Error::Runtime(format!("the graph's run loop failed to join: {join_failure}"))
    })?;

    let _ = engine_is_down_sender.send(true);
    let _ = tokio::join!(tick_consumer, graph_reporter, service_deadline);

    tracing::info!("the graph and the service came down together");
    run_outcome
}

/// Build the two-processor graph and answer with the channel name a tap needs
/// to reach its link.
async fn build_the_tick_graph(runtime: &Arc<Runner>) -> Result<String> {
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
        runtime.add_local::<SequencedTickSource::Processor>(serde_json::Value::Null)?;
    let sink_class =
        runtime.add_local::<TickCadenceReportingSink::Processor>(sink_config.clone())?;

    // Every graph op has a sync spelling and an async one. The sync spelling
    // blocks the calling thread until the reply arrives, so async callers take
    // the `*_async` twin.
    let source_id = runtime
        .add_processor_async(ProcessorSpec::new(source_class, serde_json::Value::Null))
        .await?;
    let sink_id = runtime
        .add_processor_async(ProcessorSpec::new(sink_class, sink_config))
        .await?;
    runtime
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
    runtime: Arc<Runner>,
    tick_channel: String,
    mut engine_is_down: watch::Receiver<bool>,
) {
    if let Err(never_came_up) = await_every_processor_running(&runtime, &mut engine_is_down).await {
        tracing::error!(error = %never_came_up, "no tap: the graph never came up");
        return;
    }

    let mut tap = match runtime.tap_async(tick_channel.clone(), None).await {
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
            _ = engine_is_down.changed() => break,
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
    runtime: Arc<Runner>,
    mut engine_is_down: watch::Receiver<bool>,
) {
    let mut report_interval = tokio::time::interval(GRAPH_REPORT_INTERVAL);
    // The first tick of a tokio interval fires immediately, and the graph has
    // nothing to say yet.
    report_interval.tick().await;

    loop {
        tokio::select! {
            _ = report_interval.tick() => match runtime.to_json_async().await {
                Ok(graph) => tracing::info!(
                    processors = %processor_states_of(&graph).join(", "),
                    link_count = graph["links"].as_array().map_or(0, |links| links.len()),
                    "async code read the graph's own state"
                ),
                Err(read_failure) => {
                    tracing::warn!(error = %read_failure, "the graph's state did not read");
                }
            },
            _ = engine_is_down.changed() => break,
        }
    }
}

/// The service's own deadline, asking the graph to stop.
///
/// A request rather than a teardown: the run loop observes it and runs the
/// normal stop sequence. It is the one sync runtime op that never waits on a
/// reply, which is what makes it safe to call from a task.
async fn stop_the_graph_after(
    runtime: Arc<Runner>,
    run_duration: Duration,
    mut engine_is_down: watch::Receiver<bool>,
) {
    tokio::select! {
        _ = tokio::time::sleep(run_duration) => {}
        // Ctrl-C got there first.
        _ = engine_is_down.changed() => return,
    }

    tracing::info!(
        ?run_duration,
        "the service's deadline is asking the graph to stop"
    );
    if let Err(request_failure) =
        runtime.request_runtime_shutdown("the tokio service's run duration elapsed")
    {
        tracing::error!(
            error = %request_failure,
            "the shutdown request did not reach the run loop"
        );
    }
}

/// Wait for the graph to finish coming up without blocking a thread for it.
///
/// The engine's own `wait_until_every_processor_is_running` answers the same
/// question, but it blocks; a service whose run loop already holds a blocking
/// thread reads the same states off `to_json_async` instead.
async fn await_every_processor_running(
    runtime: &Arc<Runner>,
    engine_is_down: &mut watch::Receiver<bool>,
) -> Result<()> {
    let give_up_at = tokio::time::Instant::now() + GRAPH_STARTUP_BUDGET;
    loop {
        let graph = runtime.to_json_async().await?;
        if every_processor_is_running(&graph) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= give_up_at {
            return Err(Error::Runtime(format!(
                "the graph was still {} after {GRAPH_STARTUP_BUDGET:?}",
                processor_states_of(&graph).join(", ")
            )));
        }
        tokio::select! {
            _ = tokio::time::sleep(GRAPH_READINESS_POLL_INTERVAL) => {}
            _ = engine_is_down.changed() => {
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

/// Every node as `<display name>=<state>`, read off the graph's own JSON — the
/// same rendering `streamlib graph` serves an API consumer.
fn processor_states_of(graph: &serde_json::Value) -> Vec<String> {
    let Some(nodes) = graph["nodes"].as_array() else {
        return Vec::new();
    };
    nodes
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
    let Some(nodes) = graph["nodes"].as_array() else {
        return false;
    };
    !nodes.is_empty()
        && nodes
            .iter()
            .all(|node| node["components"]["state"].as_str() == Some("Running"))
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
        assert_eq!(
            processor_states_of(&graph),
            ["SequencedTickSource=Running", "TickCadenceReportingSink=Running"]
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
    }

    /// Vacuous truth would report an empty graph as up, and the readiness wait
    /// would return before a single processor existed.
    #[test]
    fn an_empty_graph_is_not_up() {
        assert!(!every_processor_is_running(&graph_of(serde_json::json!([]))));
        assert!(!every_processor_is_running(&serde_json::json!({})));
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
