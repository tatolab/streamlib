// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph endpoints `SingleProcessorTestPipeline` feeds and collects
//! through.
//!
//! A test hands bags to a processor and asserts on what comes back, so the
//! feeder and the collector have to be reachable from the test — which runs
//! in the app process. They are native processors for exactly that reason:
//! every Python processor runs in its own child interpreter, and a queue this
//! module holds is the app's, one process away from any child that tried to
//! read it. Their per-frame path never enters an interpreter, the same
//! boundary the media built-ins sit on.
//!
//! Queues are keyed by a channel name because configuration is JSON on the
//! graph node — a queue cannot travel through `config`, but the name of one
//! can.

use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{ContinuousProcessor, ReactiveProcessor};

use crate::python_bag_conversion::{json_value_to_python_object, python_object_to_json_value};

/// Which channel an endpoint reads from or writes to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TestHarnessChannelConfig {
    /// The name this endpoint's queue is registered under.
    #[serde(default)]
    pub channel: String,
}

/// One channel's two queues: what a test fed, and what the processor under
/// test produced.
#[derive(Default)]
struct TestHarnessChannelQueues {
    fed_by_the_test: VecDeque<serde_json::Value>,
    collected_from_the_processor: VecDeque<serde_json::Value>,
}

/// Every open channel, and the signal a waiter blocks on.
///
/// One condvar for all channels rather than one each: a test pipeline runs a
/// handful of ports, and a spurious wake costs a re-check of an empty queue.
struct TestHarnessChannelRegistry {
    open_channels: Mutex<HashMap<String, TestHarnessChannelQueues>>,
    a_channel_collected_a_bag: Condvar,
}

static TEST_HARNESS_CHANNELS: LazyLock<TestHarnessChannelRegistry> =
    LazyLock::new(|| TestHarnessChannelRegistry {
        open_channels: Mutex::new(HashMap::new()),
        a_channel_collected_a_bag: Condvar::new(),
    });

/// Open a channel. Answers `false` if the name was already taken, which is a
/// harness bug rather than something a test can recover from.
fn open_channel(channel: &str) -> bool {
    let mut open_channels = TEST_HARNESS_CHANNELS.open_channels.lock();
    if open_channels.contains_key(channel) {
        return false;
    }
    open_channels.insert(channel.to_string(), TestHarnessChannelQueues::default());
    true
}

/// Close a channel and drop whatever was still queued on it.
fn close_channel(channel: &str) {
    TEST_HARNESS_CHANNELS.open_channels.lock().remove(channel);
}

/// Queue one bag for the feeder to publish.
fn push_fed_bag(channel: &str, bag: serde_json::Value) -> bool {
    let mut open_channels = TEST_HARNESS_CHANNELS.open_channels.lock();
    let Some(queues) = open_channels.get_mut(channel) else {
        return false;
    };
    // No notify: nothing waits on the fed queue. Waking a collector waiter
    // here would restart nothing but its own re-check, and every feed from
    // a test thread would do it.
    queues.fed_by_the_test.push_back(bag);
    true
}

/// Take the next bag a test fed, if any. A closed channel reads as empty:
/// the feeder can still be ticking while its pipeline tears down.
fn take_fed_bag(channel: &str) -> Option<serde_json::Value> {
    TEST_HARNESS_CHANNELS
        .open_channels
        .lock()
        .get_mut(channel)?
        .fed_by_the_test
        .pop_front()
}

/// Record one bag the processor under test produced.
fn push_collected_bag(channel: &str, bag: serde_json::Value) {
    let mut open_channels = TEST_HARNESS_CHANNELS.open_channels.lock();
    if let Some(queues) = open_channels.get_mut(channel) {
        queues.collected_from_the_processor.push_back(bag);
        TEST_HARNESS_CHANNELS.a_channel_collected_a_bag.notify_all();
    }
}

/// What a wait for a collected bag ended in.
enum CollectedBagWaitOutcome {
    Collected(serde_json::Value),
    /// The deadline passed — the failure a test is looking for.
    TimedOut,
    /// The channel was never opened, or its pipeline already closed it.
    /// Distinguished from a timeout because a mistyped channel name that
    /// reads as "your processor produced nothing" sends a test author
    /// looking at the wrong thing.
    ChannelNotOpen,
}

/// Wait for the next collected bag until `timeout` has elapsed.
///
/// The deadline is taken once and waited against absolutely: a condvar wake
/// that finds no bag must not restart the clock, or the bounded wait a test
/// relies on to fail is not bounded at all.
fn take_collected_bag(channel: &str, timeout: Duration) -> CollectedBagWaitOutcome {
    let deadline = std::time::Instant::now() + timeout;
    let mut open_channels = TEST_HARNESS_CHANNELS.open_channels.lock();
    loop {
        let Some(queues) = open_channels.get_mut(channel) else {
            return CollectedBagWaitOutcome::ChannelNotOpen;
        };
        if let Some(bag) = queues.collected_from_the_processor.pop_front() {
            return CollectedBagWaitOutcome::Collected(bag);
        }
        if TEST_HARNESS_CHANNELS
            .a_channel_collected_a_bag
            .wait_until(&mut open_channels, deadline)
            .timed_out()
        {
            return CollectedBagWaitOutcome::TimedOut;
        }
    }
}

/// Publishes whatever a test hands it, in order.
#[streamlib::sdk::processor(
    "@tatolab/wheel-testing/TestBagFeeder",
    description = "Publishes bags a test queued on its channel, in order",
    execution = continuous(interval_ms = 1),
    config = crate::python_test_harness_endpoints::TestHarnessChannelConfig,
    output("bags_to_downstream", description = "Bags the test fed"),
)]
pub struct TestBagFeeder {}

impl ContinuousProcessor for TestBagFeeder::Processor {
    fn process(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        if let Some(bag) = take_fed_bag(&self.config.channel) {
            self.outputs.write("bags_to_downstream", &bag)?;
        }
        Ok(())
    }
}

/// Collects everything the processor under test produces.
///
/// `every_sample` rather than the default: a test asserts on what was
/// produced, so dropping a bag under a burst would make the assertion lie.
#[streamlib::sdk::processor(
    "@tatolab/wheel-testing/TestBagCollector",
    description = "Collects every bag the processor under test produced",
    execution = reactive,
    config = crate::python_test_harness_endpoints::TestHarnessChannelConfig,
    input(
        "bags_from_upstream",
        any,
        delivery_profile = "every_sample",
        description = "Bags the processor under test produced"
    ),
)]
pub struct TestBagCollector {}

impl ReactiveProcessor for TestBagCollector::Processor {
    fn process(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        while let Some((raw_bag, _timestamp_ns)) = self.inputs.read_raw("bags_from_upstream")? {
            let bag: serde_json::Value = rmp_serde::from_slice(&raw_bag).map_err(|decode| {
                streamlib::sdk::error::Error::Link(format!(
                    "the test harness could not decode a collected bag: {decode}"
                ))
            })?;
            push_collected_bag(&self.config.channel, bag);
        }
        Ok(())
    }
}

/// Register the harness endpoints so `rt.add` can resolve them.
pub(crate) fn register_test_harness_processor_types() {
    streamlib::sdk::processors::PROCESSOR_REGISTRY.register::<TestBagFeeder::Processor>();
    streamlib::sdk::processors::PROCESSOR_REGISTRY.register::<TestBagCollector::Processor>();
}

/// `streamlib.testing`'s feeder, as the marker type `Runtime.add` resolves.
#[pyclass(name = "TestBagFeeder", module = "streamlib", frozen)]
pub(crate) struct PythonTestBagFeederBlock;

#[pymethods]
impl PythonTestBagFeederBlock {
    /// pytest collects `Test*` classes by name; this tells it not to.
    #[classattr]
    #[pyo3(name = "__test__")]
    fn dunder_test() -> bool {
        false
    }
}

/// `streamlib.testing`'s collector, as the marker type `Runtime.add` resolves.
#[pyclass(name = "TestBagCollector", module = "streamlib", frozen)]
pub(crate) struct PythonTestBagCollectorBlock;

#[pymethods]
impl PythonTestBagCollectorBlock {
    /// pytest collects `Test*` classes by name; this tells it not to.
    #[classattr]
    #[pyo3(name = "__test__")]
    fn dunder_test() -> bool {
        false
    }
}

/// The harness marker classes, resolved the same way the media built-ins are.
pub(crate) fn test_harness_type_reference(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> Option<streamlib::sdk::processors::ProcessorTypeReference> {
    if processor_class.is(python.get_type::<PythonTestBagFeederBlock>()) {
        return Some(TestBagFeeder::Processor::schema_ident().into());
    }
    if processor_class.is(python.get_type::<PythonTestBagCollectorBlock>()) {
        return Some(TestBagCollector::Processor::schema_ident().into());
    }
    None
}

/// Open a harness channel under `channel`.
#[pyfunction]
pub(crate) fn open_test_harness_channel(channel: &str) -> PyResult<()> {
    if open_channel(channel) {
        return Ok(());
    }
    Err(PyRuntimeError::new_err(format!(
        "the test harness channel {channel:?} is already open; channel names are minted per \
         port and must not be reused"
    )))
}

/// Close a harness channel, dropping anything still queued.
#[pyfunction]
pub(crate) fn close_test_harness_channel(channel: &str) {
    close_channel(channel);
}

/// Queue one bag for delivery through `channel`'s feeder.
#[pyfunction]
pub(crate) fn feed_test_harness_bag(channel: &str, bag: &Bound<'_, PyAny>) -> PyResult<()> {
    let bag = python_object_to_json_value(bag)?;
    if push_fed_bag(channel, bag) {
        return Ok(());
    }
    Err(PyRuntimeError::new_err(format!(
        "the test harness channel {channel:?} is not open; the pipeline that owned it has been \
         closed"
    )))
}

/// The next bag collected on `channel`, or `None` if `timeout_seconds` ran out.
#[pyfunction]
pub(crate) fn await_test_harness_bag<'py>(
    python: Python<'py>,
    channel: &str,
    timeout_seconds: f64,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let timeout = Duration::from_secs_f64(timeout_seconds.max(0.0));
    // Detached: the wait blocks this thread until a native collector running
    // on an engine thread publishes, and holding the GIL through it would
    // stall every other thread in the test's interpreter.
    match python.detach(|| take_collected_bag(channel, timeout)) {
        CollectedBagWaitOutcome::Collected(bag) => {
            json_value_to_python_object(python, &bag).map(Some)
        }
        CollectedBagWaitOutcome::TimedOut => Ok(None),
        CollectedBagWaitOutcome::ChannelNotOpen => Err(PyRuntimeError::new_err(format!(
            "the test harness channel {channel:?} is not open; the pipeline that owned it has \
             been closed"
        ))),
    }
}
