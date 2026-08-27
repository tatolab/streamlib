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

use crate::python_bag_conversion::{msgpack_value_to_python_object, python_object_to_msgpack_value};

/// Which channel an endpoint reads from or writes to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TestHarnessChannelConfig {
    /// The name this endpoint's queue is registered under.
    #[serde(default)]
    pub channel: String,
}

/// One channel's two queues: what a test fed, and what the processor under
/// test produced.
///
/// Bags are held as the msgpack value the wire carries, which is the only
/// representation that holds every bag: a byte payload is `bin`, and JSON has
/// no bytes.
#[derive(Default)]
struct TestHarnessChannelQueues {
    fed_by_the_test: VecDeque<rmpv::Value>,
    collected_from_the_processor: VecDeque<rmpv::Value>,
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
fn push_fed_bag(channel: &str, bag: rmpv::Value) -> bool {
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
fn take_fed_bag(channel: &str) -> Option<rmpv::Value> {
    TEST_HARNESS_CHANNELS
        .open_channels
        .lock()
        .get_mut(channel)?
        .fed_by_the_test
        .pop_front()
}

/// Record one bag the processor under test produced.
fn push_collected_bag(channel: &str, bag: rmpv::Value) {
    let mut open_channels = TEST_HARNESS_CHANNELS.open_channels.lock();
    if let Some(queues) = open_channels.get_mut(channel) {
        queues.collected_from_the_processor.push_back(bag);
        TEST_HARNESS_CHANNELS.a_channel_collected_a_bag.notify_all();
    }
}

/// What a wait for a collected bag ended in.
enum CollectedBagWaitOutcome {
    Collected(rmpv::Value),
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
    description = "Collects every bag the processor under test produced",
    execution = reactive,
    config = crate::python_test_harness_endpoints::TestHarnessChannelConfig,
    input(
        "bags_from_upstream",
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
            push_collected_bag(
                &self.config.channel,
                decode_collected_bag_from_wire_bytes(&raw_bag)?,
            );
        }
        Ok(())
    }
}

/// Decode one bag the collector read off the wire.
///
/// Through `rmpv` rather than `serde_json::Value`, the way the tap path
/// already does: a byte payload is msgpack `bin`, and JSON's value tree has no
/// bytes in it at all — its visitor implements no `visit_bytes`, so every bag
/// carrying bytes failed to decode here with an `invalid_type` error.
fn decode_collected_bag_from_wire_bytes(raw_bag: &[u8]) -> Result<rmpv::Value> {
    rmpv::decode::read_value(&mut &raw_bag[..]).map_err(|decode| {
        streamlib::sdk::error::Error::Link(format!(
            "the test harness could not decode a collected bag: {decode}"
        ))
    })
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
pub(crate) fn test_harness_class_import_path(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> Option<streamlib::sdk::descriptors::ProcessorClassImportPath> {
    if processor_class.is(python.get_type::<PythonTestBagFeederBlock>()) {
        return Some(TestBagFeeder::Processor::processor_class_import_path());
    }
    if processor_class.is(python.get_type::<PythonTestBagCollectorBlock>()) {
        return Some(TestBagCollector::Processor::processor_class_import_path());
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
    let bag = python_object_to_msgpack_value(bag)?;
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
            msgpack_value_to_python_object(python, &bag).map(Some)
        }
        CollectedBagWaitOutcome::TimedOut => Ok(None),
        CollectedBagWaitOutcome::ChannelNotOpen => Err(PyRuntimeError::new_err(format!(
            "the test harness channel {channel:?} is not open; the pipeline that owned it has \
             been closed"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::{PyBytes, PyDict};

    /// A byte payload is msgpack `bin` on the wire, and the harness has to
    /// carry it back to the test as `bytes`.
    ///
    /// The defect this locks is not audio-specific: `serde_json::Value`'s
    /// visitor implements no `visit_bytes`, so decoding a collected bag
    /// through it failed with `invalid_type` for every bag carrying bytes —
    /// including one a Python processor writes.
    #[test]
    fn a_collected_bag_carrying_bytes_reaches_the_test_as_bytes() {
        Python::initialize();
        let channel = "collected-bytes-survive-the-harness";
        assert!(open_channel(channel), "the channel must be ours to close");

        let bag_on_the_wire = rmpv::Value::Map(vec![
            (
                rmpv::Value::from("samples"),
                rmpv::Value::Binary(vec![0x00, 0x80, 0xff]),
            ),
            (rmpv::Value::from("sample_rate"), rmpv::Value::from(48_000)),
        ]);
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &bag_on_the_wire).expect("msgpack encode");

        push_collected_bag(
            channel,
            decode_collected_bag_from_wire_bytes(&wire_bytes).expect("a bin payload decodes"),
        );

        Python::attach(|python| {
            let collected = await_test_harness_bag(python, channel, 1.0)
                .expect("the channel is open")
                .expect("a bag was queued before the wait");
            let samples = collected
                .cast::<PyDict>()
                .expect("a bag is a named map")
                .get_item("samples")
                .expect("lookup")
                .expect("the bag carries its samples");
            assert_eq!(
                samples
                    .cast::<PyBytes>()
                    .expect("a bin payload reaches Python as bytes")
                    .as_bytes(),
                &[0x00, 0x80, 0xff]
            );
        });
        close_channel(channel);
    }

    /// The same payload in the other direction: what a test feeds reaches the
    /// feeder as `bin`, so what the feeder publishes is what a processor
    /// under test reads.
    #[test]
    fn a_fed_bag_carrying_bytes_keeps_them_on_the_way_to_the_feeder() {
        Python::initialize();
        let channel = "fed-bytes-survive-the-harness";
        assert!(open_channel(channel), "the channel must be ours to close");

        Python::attach(|python| {
            let bag = PyDict::new(python);
            bag.set_item("samples", PyBytes::new(python, &[0x01, 0x02, 0x03]))
                .expect("set");
            feed_test_harness_bag(channel, bag.as_any()).expect("the channel is open");
        });

        let rmpv::Value::Map(entries) = take_fed_bag(channel).expect("a bag was queued") else {
            panic!("a bag is a named map");
        };
        assert_eq!(
            entries
                .iter()
                .find(|(key, _)| key.as_str() == Some("samples"))
                .expect("the bag carries its samples")
                .1,
            rmpv::Value::Binary(vec![0x01, 0x02, 0x03])
        );
        close_channel(channel);
    }
}
