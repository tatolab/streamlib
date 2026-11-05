// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A stepped stream of native nodes runs behind its step barrier: a continuous
//! source, a slow reactive middle and a sink run exactly the steps the clock
//! names, faster than real time, every message stamped with its step's time and
//! none dropped. A node that paces itself is refused in a stepped stream.

use std::path::Path;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serial_test::serial;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner};
use streamlib_engine::core::{
    Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess, StreamClockSpec,
};

/// Stream nanoseconds per step, and the source's interval.
const STEP_NS: u64 = 50_000_000;

/// The steps the stream runs before it stops itself.
const STEPS_THE_STREAM_RUNS: u64 = 12;

/// How long the middle node takes over each message, on the machine clock.
const MIDDLE_NODE_MACHINE_TIME_PER_MESSAGE: Duration = Duration::from_millis(5);

/// Each message the sink took: its tick number and its stamp.
static TICKS_THE_SINK_TOOK: Mutex<Vec<(u64, i64)>> = Mutex::new(Vec::new());

#[streamlib::sdk::processor(execution = continuous(interval_ms = 50), output("ticks"))]
pub struct SteppedTickSource {
    next_tick: u64,
}

impl streamlib_engine::ContinuousProcessor for SteppedTickSource::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let tick = self.next_tick;
        self.next_tick += 1;
        self.outputs.write_msgpack("ticks", &tick)
    }
}

#[streamlib::sdk::processor(
    execution = reactive,
    input("ticks", delivery_profile = "ordered"),
    output("relayed"),
)]
pub struct SlowSteppedRelay;

impl streamlib_engine::ReactiveProcessor for SlowSteppedRelay::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let Some(message) = self.inputs.read_raw("ticks")? else {
            return Ok(());
        };
        std::thread::sleep(MIDDLE_NODE_MACHINE_TIME_PER_MESSAGE);
        let tick: u64 = rmp_serde::from_slice(&message.payload)
            .map_err(|decode| streamlib_engine::core::Error::Runtime(decode.to_string()))?;
        self.outputs.write_msgpack("relayed", &tick)
    }
}

#[streamlib::sdk::processor(execution = reactive, input("relayed", delivery_profile = "ordered"))]
pub struct SteppedTickSink;

impl streamlib_engine::ReactiveProcessor for SteppedTickSink::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw("relayed")? {
            let tick: u64 = rmp_serde::from_slice(&message.payload)
                .map_err(|decode| streamlib_engine::core::Error::Runtime(decode.to_string()))?;
            TICKS_THE_SINK_TOOK
                .lock()
                .push((tick, message.timestamp_ns));
        }
        Ok(())
    }
}

/// Each batch the slow sink took: the ticks it read in one call.
static TICK_BATCHES_THE_SLOW_SINK_TOOK: Mutex<Vec<Vec<u64>>> = Mutex::new(Vec::new());

#[streamlib::sdk::processor(
    execution = continuous(interval_ms = 150),
    input("ticks", delivery_profile = "ordered"),
)]
pub struct EveryThirdStepTickSink;

impl streamlib_engine::ContinuousProcessor for EveryThirdStepTickSink::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let mut batch = Vec::new();
        while let Some(message) = self.inputs.read_raw("ticks")? {
            batch
                .push(rmp_serde::from_slice(&message.payload).map_err(|decode| {
                    streamlib_engine::core::Error::Runtime(decode.to_string())
                })?);
        }
        TICK_BATCHES_THE_SLOW_SINK_TOOK.lock().push(batch);
        Ok(())
    }
}

#[streamlib::sdk::processor(execution = manual, output("frames"))]
pub struct SelfPacedSource;

impl streamlib_engine::ManualProcessor for SelfPacedSource::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn register_the_test_nodes() {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        PROCESSOR_REGISTRY.register::<SteppedTickSource::Processor>();
        PROCESSOR_REGISTRY.register::<SlowSteppedRelay::Processor>();
        PROCESSOR_REGISTRY.register::<SteppedTickSink::Processor>();
        PROCESSOR_REGISTRY.register::<SelfPacedSource::Processor>();
        PROCESSOR_REGISTRY.register::<EveryThirdStepTickSink::Processor>();
    });
}

fn load(
    runner: &Runner,
    project_directory: &Path,
    graph_document: serde_json::Value,
) -> Result<Arc<LoadedStreamInThisRuntime>> {
    runner.load_stream_from_graph_snapshot(
        &GraphSnapshot::from_graph_document(graph_document).expect("a graph document is a graph"),
        OptionsForLoadingOneStream::in_project_directory(project_directory),
    )
}

fn wait_for_the_stream_to_end(stream: &LoadedStreamInThisRuntime, budget: Duration) {
    let ends_by = Instant::now() + budget;
    while !stream.has_ended() {
        assert!(
            Instant::now() < ends_by,
            "the stream did not stop itself within {budget:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[serial]
fn a_stepped_stream_runs_exactly_its_steps_faster_than_real_time_stamped_on_each_step() {
    register_the_test_nodes();
    TICKS_THE_SINK_TOOK.lock().clear();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let started = Instant::now();

    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "stepped",
            "clock": serde_json::to_value(StreamClockSpec::Stepped {
                step_ns: STEP_NS,
                pace: None,
                stop_after_steps: Some(STEPS_THE_STREAM_RUNS),
            }).expect("a clock spec serializes"),
            "nodes": [
                {"name": "source", "type": SteppedTickSource::processor_class_import_path().as_str()},
                {"name": "relay", "type": SlowSteppedRelay::processor_class_import_path().as_str()},
                {"name": "sink", "type": SteppedTickSink::processor_class_import_path().as_str()},
            ],
            "links": [
                {"source": {"node": "source", "port": "ticks"}, "target": {"node": "relay", "port": "ticks"}},
                {"source": {"node": "relay", "port": "relayed"}, "target": {"node": "sink", "port": "relayed"}},
            ],
        }),
    )
    .expect("a stepped stream of native nodes loads");
    stream.start().expect("the stepped stream starts");
    wait_for_the_stream_to_end(&stream, Duration::from_secs(30));
    let ran_for = started.elapsed();

    let ticks_the_sink_took = TICKS_THE_SINK_TOOK.lock().clone();
    assert_eq!(
        ticks_the_sink_took
            .iter()
            .map(|(tick, _)| *tick)
            .collect::<Vec<_>>(),
        (0..STEPS_THE_STREAM_RUNS).collect::<Vec<_>>(),
        "one tick per step, each relayed and taken, none dropped"
    );
    let first_stamp = ticks_the_sink_took[0].1;
    for (tick, stamp) in &ticks_the_sink_took {
        assert_eq!(
            stamp - first_stamp,
            *tick as i64 * STEP_NS as i64,
            "tick {tick} carries its step's time: {ticks_the_sink_took:?}"
        );
    }
    let the_steps_in_real_time = Duration::from_nanos(STEP_NS * STEPS_THE_STREAM_RUNS);
    assert!(
        ran_for < the_steps_in_real_time,
        "{STEPS_THE_STREAM_RUNS} unpaced steps took {ran_for:?}, no faster than the \
         {the_steps_in_real_time:?} they span"
    );
}

#[test]
#[serial]
fn a_paced_stepped_stream_takes_its_steps_share_of_real_time() {
    register_the_test_nodes();
    TICKS_THE_SINK_TOOK.lock().clear();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");

    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "paced",
            "clock": serde_json::to_value(StreamClockSpec::Stepped {
                step_ns: STEP_NS,
                pace: Some(2.0),
                stop_after_steps: Some(STEPS_THE_STREAM_RUNS),
            }).expect("a clock spec serializes"),
            "nodes": [
                {"name": "source", "type": SteppedTickSource::processor_class_import_path().as_str()},
                {"name": "relay", "type": SlowSteppedRelay::processor_class_import_path().as_str()},
                {"name": "sink", "type": SteppedTickSink::processor_class_import_path().as_str()},
            ],
            "links": [
                {"source": {"node": "source", "port": "ticks"}, "target": {"node": "relay", "port": "ticks"}},
                {"source": {"node": "relay", "port": "relayed"}, "target": {"node": "sink", "port": "relayed"}},
            ],
        }),
    )
    .expect("a paced stepped stream loads");
    stream.start().expect("the paced stream starts");
    let started = Instant::now();
    wait_for_the_stream_to_end(&stream, Duration::from_secs(30));
    let ran_for = started.elapsed();

    let the_steps_at_twice_real_time =
        Duration::from_nanos(STEP_NS * (STEPS_THE_STREAM_RUNS - 1) / 2);
    assert!(
        ran_for >= the_steps_at_twice_real_time,
        "{STEPS_THE_STREAM_RUNS} steps paced at 2× took {ran_for:?}, under \
         {the_steps_at_twice_real_time:?}"
    );
    assert_eq!(
        TICKS_THE_SINK_TOOK.lock().len() as u64,
        STEPS_THE_STREAM_RUNS
    );
}

#[test]
#[serial]
fn a_node_that_paces_itself_fails_a_stepped_stream_naming_it() {
    register_the_test_nodes();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");

    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "stepped-with-a-device",
            "clock": {"mode": "stepped", "step_ns": STEP_NS},
            "nodes": [
                {"name": "camera", "type": SelfPacedSource::processor_class_import_path().as_str()},
            ],
        }),
    )
    .expect("the stream loads");
    stream.start().expect("the stream starts");

    let refusal = stream
        .wait_until_every_processor_is_running(Duration::from_secs(10))
        .expect_err("a self-paced node never runs in a stepped stream")
        .to_string();
    assert!(
        refusal.contains("`camera`") && refusal.contains("stepped stream"),
        "the refusal names the node and why: {refusal}"
    );
    wait_for_the_stream_to_end(&stream, Duration::from_secs(30));
}

#[test]
#[serial]
fn a_continuous_consumer_slower_than_the_steps_takes_what_waited_for_it_at_its_deadline() {
    register_the_test_nodes();
    TICK_BATCHES_THE_SLOW_SINK_TOOK.lock().clear();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");

    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "slow-consumer",
            "clock": serde_json::to_value(StreamClockSpec::Stepped {
                step_ns: STEP_NS,
                pace: None,
                stop_after_steps: Some(STEPS_THE_STREAM_RUNS),
            }).expect("a clock spec serializes"),
            "nodes": [
                {"name": "source", "type": SteppedTickSource::processor_class_import_path().as_str()},
                {"name": "sink", "type": EveryThirdStepTickSink::processor_class_import_path().as_str()},
            ],
            "links": [
                {"source": {"node": "source", "port": "ticks"}, "target": {"node": "sink", "port": "ticks"}},
            ],
        }),
    )
    .expect("a stepped stream with a slow continuous consumer loads");
    stream.start().expect("the stream starts");
    wait_for_the_stream_to_end(&stream, Duration::from_secs(30));

    let batches = TICK_BATCHES_THE_SLOW_SINK_TOOK.lock().clone();
    assert_eq!(
        batches.len() as u64,
        STEPS_THE_STREAM_RUNS.div_ceil(3),
        "the sink runs every third step: {batches:?}"
    );
    assert_eq!(
        batches.concat(),
        (0..batches.concat().len() as u64).collect::<Vec<_>>(),
        "the sink takes every tick in order, none dropped: {batches:?}"
    );
}
