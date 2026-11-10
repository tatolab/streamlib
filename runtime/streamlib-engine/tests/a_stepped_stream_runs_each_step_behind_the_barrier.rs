// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A stepped stream of native nodes runs behind its step barrier: a continuous
//! source, a slow reactive middle and a sink run exactly the steps the clock
//! names, faster than real time, every message stamped with its step's time and
//! none dropped. A scaled clock faster than real time runs behind the same
//! barrier, paced at its scale, from its load or from a live rescale across
//! 1×. A manual node is refused in a stream whose clock steps unless it is
//! paced by real time, and a node paced by real time never holds a step.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

use parking_lot::Mutex;
use serial_test::serial;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner};
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

#[streamlib::sdk::processor(
    execution = continuous(interval_ms = 50),
    output("ticks", family = Records),
)]
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
    input("ticks", delivery_profile = "ordered", family = Records),
    output("relayed", family = Records),
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

#[streamlib::sdk::processor(
    execution = reactive,
    input("relayed", delivery_profile = "ordered", family = Records),
)]
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
    input("ticks", delivery_profile = "ordered", family = Records),
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

#[streamlib::sdk::processor(execution = manual, output("frames", family = Image))]
pub struct SelfPacedSource;

impl streamlib_engine::ManualProcessor for SelfPacedSource::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

#[streamlib::sdk::processor(
    execution = manual,
    paced_by_real_time,
    output("frames", family = Image),
)]
pub struct RealTimePacedManualSource;

impl streamlib_engine::ManualProcessor for RealTimePacedManualSource::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// How long the node paced by real time takes over each call, on the machine clock.
const REAL_TIME_PACED_NODE_MACHINE_TIME_PER_CALL: Duration = Duration::from_millis(300);

#[streamlib::sdk::processor(
    execution = continuous(interval_ms = 50),
    paced_by_real_time,
    output("ticks", family = Records),
)]
pub struct SlowRealTimePacedTickSource;

impl streamlib_engine::ContinuousProcessor for SlowRealTimePacedTickSource::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.outputs.write_msgpack("ticks", &0u64)?;
        std::thread::sleep(REAL_TIME_PACED_NODE_MACHINE_TIME_PER_CALL);
        Ok(())
    }
}

/// How many ticks of the node paced by real time its sink took.
static TICKS_THE_REAL_TIME_SINK_TOOK: AtomicU64 = AtomicU64::new(0);

#[streamlib::sdk::processor(
    execution = reactive,
    input("ticks", delivery_profile = "ordered", family = Records),
)]
pub struct RealTimePacedTickSink;

impl streamlib_engine::ReactiveProcessor for RealTimePacedTickSink::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while self.inputs.read_raw("ticks")?.is_some() {
            TICKS_THE_REAL_TIME_SINK_TOOK.fetch_add(1, Ordering::Relaxed);
        }
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
        PROCESSOR_REGISTRY.register::<RealTimePacedManualSource::Processor>();
        PROCESSOR_REGISTRY.register::<SlowRealTimePacedTickSource::Processor>();
        PROCESSOR_REGISTRY.register::<RealTimePacedTickSink::Processor>();
    });
}

fn load(
    runner: &Runner,
    project_directory: &Path,
    graph_document: serde_json::Value,
) -> Result<Arc<LoadedStreamInThisRuntime>> {
    let stream_name = graph_document["stream"]
        .as_str()
        .expect("a test's graph document names its stream")
        .to_owned();
    runner.load_stream_from_graph_snapshot(
        &GraphSnapshot::from_graph_document(graph_document).expect("a graph document is a graph"),
        options_loading_a_stream_of_the_test_project_at(project_directory, &stream_name),
    )
}

fn wait_for_the_sink_to_take(ticks: usize, budget: Duration) {
    let taken_by = Instant::now() + budget;
    while TICKS_THE_SINK_TOOK.lock().len() < ticks {
        assert!(
            Instant::now() < taken_by,
            "the sink took {} of {ticks} ticks within {budget:?}",
            TICKS_THE_SINK_TOOK.lock().len()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn a_tick_chain_on(clock: StreamClockSpec, stream: &str) -> serde_json::Value {
    serde_json::json!({
        "stream": stream,
        "clock": serde_json::to_value(clock).expect("a clock spec serializes"),
        "nodes": [
            {"name": "source", "type": SteppedTickSource::processor_class_import_path().as_str()},
            {"name": "relay", "type": SlowSteppedRelay::processor_class_import_path().as_str()},
            {"name": "sink", "type": SteppedTickSink::processor_class_import_path().as_str()},
        ],
        "links": [
            {"source": {"node": "source", "port": "ticks"}, "target": {"node": "relay", "port": "ticks"}},
            {"source": {"node": "relay", "port": "relayed"}, "target": {"node": "sink", "port": "relayed"}},
        ],
    })
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
        refusal.contains("`camera`") && refusal.contains("clock steps"),
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

#[test]
#[serial]
fn nodes_paced_by_real_time_load_in_a_stepped_stream_and_never_hold_its_steps() {
    register_the_test_nodes();
    TICKS_THE_SINK_TOOK.lock().clear();
    TICKS_THE_REAL_TIME_SINK_TOOK.store(0, Ordering::Relaxed);
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");

    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "stepped-beside-real-time",
            "clock": serde_json::to_value(StreamClockSpec::Stepped {
                step_ns: STEP_NS,
                pace: None,
                stop_after_steps: Some(STEPS_THE_STREAM_RUNS),
            }).expect("a clock spec serializes"),
            "nodes": [
                {"name": "source", "type": SteppedTickSource::processor_class_import_path().as_str()},
                {"name": "relay", "type": SlowSteppedRelay::processor_class_import_path().as_str()},
                {"name": "sink", "type": SteppedTickSink::processor_class_import_path().as_str()},
                {"name": "camera", "type": RealTimePacedManualSource::processor_class_import_path().as_str()},
                {"name": "slow-live", "type": SlowRealTimePacedTickSource::processor_class_import_path().as_str()},
                {"name": "live-sink", "type": RealTimePacedTickSink::processor_class_import_path().as_str()},
            ],
            "links": [
                {"source": {"node": "source", "port": "ticks"}, "target": {"node": "relay", "port": "ticks"}},
                {"source": {"node": "relay", "port": "relayed"}, "target": {"node": "sink", "port": "relayed"}},
                {"source": {"node": "slow-live", "port": "ticks"}, "target": {"node": "live-sink", "port": "ticks"}},
            ],
        }),
    )
    .expect("a stepped stream holding nodes paced by real time loads");
    let started = Instant::now();
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_processor_is_running(Duration::from_secs(10))
        .expect("a manual node paced by real time runs in a stepped stream");
    wait_for_the_stream_to_end(&stream, Duration::from_secs(30));
    let ran_for = started.elapsed();

    let held_by_the_slow_live_node =
        REAL_TIME_PACED_NODE_MACHINE_TIME_PER_CALL * STEPS_THE_STREAM_RUNS as u32;
    assert!(
        ran_for < held_by_the_slow_live_node / 2,
        "{STEPS_THE_STREAM_RUNS} unpaced steps took {ran_for:?} beside a node paced by real time \
         taking {REAL_TIME_PACED_NODE_MACHINE_TIME_PER_CALL:?} a call"
    );
    let ticks_the_sink_took = TICKS_THE_SINK_TOOK.lock().clone();
    assert_eq!(
        ticks_the_sink_took
            .iter()
            .map(|(tick, _)| *tick)
            .collect::<Vec<_>>(),
        (0..STEPS_THE_STREAM_RUNS).collect::<Vec<_>>(),
        "the step chain runs every step: {ticks_the_sink_took:?}"
    );
    assert!(
        TICKS_THE_REAL_TIME_SINK_TOOK.load(Ordering::Relaxed) >= 1,
        "the tick of the node paced by real time reaches its sink"
    );
}

#[test]
#[serial]
fn a_stream_scaled_above_real_time_runs_its_steps_at_the_scale_stamped_on_each_step() {
    register_the_test_nodes();
    TICKS_THE_SINK_TOOK.lock().clear();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    const TICKS_TAKEN: usize = 24;

    let stream = load(
        &runner,
        project_directory.path(),
        a_tick_chain_on(
            StreamClockSpec::Scaled {
                scale: 4.0,
                step_ns: STEP_NS,
            },
            "four-times-real-time",
        ),
    )
    .expect("a stream scaled above real time loads");
    stream.start().expect("the stream starts");
    let started = Instant::now();
    wait_for_the_sink_to_take(TICKS_TAKEN, Duration::from_secs(30));
    let ran_for = started.elapsed();
    stream.stop().expect("the stream stops");

    let ticks_the_sink_took = TICKS_THE_SINK_TOOK.lock().clone();
    let first_stamp = ticks_the_sink_took[0].1;
    for (index, (tick, stamp)) in ticks_the_sink_took.iter().enumerate() {
        assert_eq!(
            *tick, index as u64,
            "every tick in order: {ticks_the_sink_took:?}"
        );
        assert_eq!(
            stamp - first_stamp,
            *tick as i64 * STEP_NS as i64,
            "tick {tick} carries its step's time: {ticks_the_sink_took:?}"
        );
    }
    let the_steps_at_four_times_real_time =
        Duration::from_nanos(STEP_NS * (TICKS_TAKEN as u64 - 1) / 4);
    let the_steps_in_real_time = Duration::from_nanos(STEP_NS * TICKS_TAKEN as u64);
    assert!(
        the_steps_at_four_times_real_time <= ran_for && ran_for < the_steps_in_real_time,
        "{TICKS_TAKEN} steps at 4× took {ran_for:?}, outside \
         {the_steps_at_four_times_real_time:?}..{the_steps_in_real_time:?}"
    );
}

#[test]
#[serial]
fn a_live_rescale_across_real_time_moves_a_running_stream_into_its_steps_and_out() {
    register_the_test_nodes();
    TICKS_THE_SINK_TOOK.lock().clear();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");

    let stream = load(
        &runner,
        project_directory.path(),
        a_tick_chain_on(
            StreamClockSpec::Scaled {
                scale: 0.5,
                step_ns: STEP_NS,
            },
            "crosses-real-time",
        ),
    )
    .expect("a stream at half real time loads");
    stream.start().expect("the stream starts");
    wait_for_the_sink_to_take(3, Duration::from_secs(30));

    let stepping = stream
        .set_stream_clock_scale(4.0)
        .expect("a running scaled stream rescales above real time");
    assert_eq!((stepping.mode, stepping.step_ns), ("scaled", Some(STEP_NS)));
    let ticks_before_the_steps = TICKS_THE_SINK_TOOK.lock().len();
    let stepping_from = Instant::now();
    wait_for_the_sink_to_take(ticks_before_the_steps + 16, Duration::from_secs(30));
    let sixteen_steps_took = stepping_from.elapsed();

    let free = stream
        .set_stream_clock_scale(0.5)
        .expect("a stream in steps rescales back under real time");
    assert_eq!((free.step_ns, free.step_index), (None, None));
    let ticks_before_running_free = TICKS_THE_SINK_TOOK.lock().len();
    wait_for_the_sink_to_take(ticks_before_running_free + 3, Duration::from_secs(30));
    stream.stop().expect("the stream stops");

    let ticks_the_sink_took = TICKS_THE_SINK_TOOK.lock().clone();
    for (index, (tick, _)) in ticks_the_sink_took.iter().enumerate() {
        assert_eq!(
            *tick, index as u64,
            "every tick in order: {ticks_the_sink_took:?}"
        );
    }
    for pair in ticks_the_sink_took.windows(2) {
        let stamp_gap = pair[1].1 - pair[0].1;
        assert!(
            0 < stamp_gap && stamp_gap <= 2 * STEP_NS as i64,
            "stream time neither steps back nor jumps across a rescale: {ticks_the_sink_took:?}"
        );
    }
    for pair in
        ticks_the_sink_took[ticks_before_the_steps + 1..ticks_before_running_free - 1].windows(2)
    {
        assert_eq!(
            pair[1].1 - pair[0].1,
            STEP_NS as i64,
            "in steps every tick lands one step after the last: {ticks_the_sink_took:?}"
        );
    }
    let fifteen_steps_at_four_times_real_time = Duration::from_nanos(STEP_NS * 15 / 4);
    assert!(
        fifteen_steps_at_four_times_real_time <= sixteen_steps_took
            && sixteen_steps_took < Duration::from_nanos(STEP_NS * 16),
        "16 steps at 4× took {sixteen_steps_took:?}, outside \
         {fifteen_steps_at_four_times_real_time:?}..{:?}",
        Duration::from_nanos(STEP_NS * 16)
    );
}

#[test]
#[serial]
fn a_scale_above_real_time_is_refused_naming_a_node_that_paces_itself() {
    register_the_test_nodes();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");

    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "scaled-with-devices",
            "clock": {"mode": "scaled", "scale": 0.5},
            "nodes": [
                {"name": "camera", "type": SelfPacedSource::processor_class_import_path().as_str()},
                {"name": "live-camera", "type": RealTimePacedManualSource::processor_class_import_path().as_str()},
            ],
        }),
    )
    .expect("the stream loads");
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_processor_is_running(Duration::from_secs(10))
        .expect("manual nodes run in a stream under real time");

    let refusal = stream
        .set_stream_clock_scale(2.0)
        .expect_err("a node that paces itself keeps its stream under real time")
        .to_string();
    assert!(
        refusal.contains("`camera`") && !refusal.contains("live-camera"),
        "the refusal names the node that paces itself: {refusal}"
    );
    assert_eq!(stream.set_stream_clock_scale(0.75).unwrap().scale, 0.75);
    stream.stop().expect("the stream stops");

    let beside_real_time_only = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "scaled-beside-real-time",
            "clock": {"mode": "scaled", "scale": 0.5},
            "nodes": [
                {"name": "live-camera", "type": RealTimePacedManualSource::processor_class_import_path().as_str()},
            ],
        }),
    )
    .expect("the stream loads");
    beside_real_time_only.start().expect("the stream starts");
    beside_real_time_only
        .wait_until_every_processor_is_running(Duration::from_secs(10))
        .expect("a manual node paced by real time runs");
    assert_eq!(
        beside_real_time_only
            .set_stream_clock_scale(2.0)
            .expect("a node paced by real time never holds a step")
            .step_ns,
        Some(16_666_667)
    );
    beside_real_time_only.stop().expect("the stream stops");
}

#[test]
#[serial]
fn a_node_that_paces_itself_fails_a_stream_scaled_above_real_time_naming_it() {
    register_the_test_nodes();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");

    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "fast-with-a-device",
            "clock": {"mode": "scaled", "scale": 2.0},
            "nodes": [
                {"name": "camera", "type": SelfPacedSource::processor_class_import_path().as_str()},
            ],
        }),
    )
    .expect("the stream loads");
    stream.start().expect("the stream starts");

    let refusal = stream
        .wait_until_every_processor_is_running(Duration::from_secs(10))
        .expect_err("a self-paced node never runs in a stream whose clock steps")
        .to_string();
    assert!(
        refusal.contains("`camera`") && refusal.contains("clock steps"),
        "the refusal names the node and why: {refusal}"
    );
    wait_for_the_stream_to_end(&stream, Duration::from_secs(30));
}

#[test]
#[serial]
fn a_live_rescale_above_real_time_steps_on_while_a_node_is_paused() {
    register_the_test_nodes();
    TICKS_THE_SINK_TOOK.lock().clear();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");

    let stream = load(
        &runner,
        project_directory.path(),
        a_tick_chain_on(
            StreamClockSpec::Scaled {
                scale: 0.5,
                step_ns: STEP_NS,
            },
            "crosses-real-time-while-paused",
        ),
    )
    .expect("a stream at half real time loads");
    let idle_node = stream
        .add_processor(ProcessorSpec::new(
            RealTimePacedTickSink::processor_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("an idle node joins the stream");
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_processor_is_running(Duration::from_secs(10))
        .expect("every node runs");
    stream
        .pause_processor(&idle_node)
        .expect("the idle node pauses");

    stream
        .set_stream_clock_scale(4.0)
        .expect("a running scaled stream rescales above real time");
    let ticks_before_the_steps = TICKS_THE_SINK_TOOK.lock().len();
    wait_for_the_sink_to_take(ticks_before_the_steps + 8, Duration::from_secs(10));
    stream.stop().expect("the stream stops");
}
