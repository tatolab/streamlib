// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-internal test fixtures shared across `#[cfg(test)]` modules.
//!
//! The TestMock processor types live here so engine tests can drive
//! graph + compiler code without depending on any external package's
//! processors. The `#[processor]` macro never auto-registers; tests
//! register the mocks explicitly via [`ensure_test_mocks_registered`].
//!
//! [`CapturedTracingWarnings`] is the one warning-capture layer every module
//! asserts a log line against.

use std::fmt::Write;
use std::sync::{Arc, Mutex, Once};

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use crate::core::processors::PROCESSOR_REGISTRY;

/// Mock processor with two input ports + two output ports.
#[crate::processor(
    execution = manual,
    input("in1", delivery_profile = "newest"),
    input("in2", delivery_profile = "newest"),
    output("out1"),
    output("out2"),
)]
pub(crate) struct MockProcessor;

impl crate::core::ManualProcessor for MockProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only output ports.
#[crate::processor(
    execution = manual,
    output("out1"),
    output("out2"),
)]
pub(crate) struct MockOutputOnlyProcessor;

impl crate::core::ManualProcessor for MockOutputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only input ports.
#[crate::processor(
    execution = manual,
    input("in1", delivery_profile = "newest"),
    input("in2", delivery_profile = "newest"),
)]
pub(crate) struct MockInputOnlyProcessor;

impl crate::core::ManualProcessor for MockInputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor whose one input port reads `ordered`, beside the `newest`
/// ports every other input mock declares.
#[crate::processor(
    execution = manual,
    input("in1", delivery_profile = "ordered"),
)]
pub(crate) struct MockOrderedInputOnlyProcessor;

impl crate::core::ManualProcessor for MockOrderedInputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only input ports, waking on upstream writes rather
/// than driving itself — the one execution mode that consumes the link
/// notifications its listener receives.
#[crate::processor(
    execution = reactive,
    input("in1", delivery_profile = "newest"),
    input("in2", delivery_profile = "newest"),
)]
pub(crate) struct MockReactiveInputOnlyProcessor;

impl crate::core::ReactiveProcessor for MockReactiveInputOnlyProcessor::Processor {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock consumer whose audio input port declares a window contract — the
/// destination shape the read-side windowing stage exists for.
#[crate::processor(
    execution = reactive,
    input(
        "audio",
        delivery_profile = "ordered",
        audio_window(
            sample_rate = 16_000,
            channels = 1,
            dtype = "f32",
            window_size = 512,
            hop = 512
        )
    ),
)]
pub(crate) struct MockWindowedAudioConsumerProcessor;

impl crate::core::ReactiveProcessor for MockWindowedAudioConsumerProcessor::Processor {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock consumer whose audio input port declares the `match_device` sentinel.
/// Nothing resolves it on this rung, which is what makes it a wiring error
/// rather than a default.
#[crate::processor(
    execution = reactive,
    input("audio", delivery_profile = "ordered", audio_window = match_device),
)]
pub(crate) struct MockDeviceMatchedAudioConsumerProcessor;

impl crate::core::ReactiveProcessor for MockDeviceMatchedAudioConsumerProcessor::Processor {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock source whose one output port's name the channel-name grammar cannot
/// carry — an uppercase letter inside the chunk.
///
/// Nothing between the declaration and the first `connect` validates a port name
/// against that grammar, so this is what an author writing camelCase gets: a
/// processor that adds, runs, and holds an output port whose channel can never
/// be named.
#[crate::processor(
    execution = manual,
    output("outOne"),
)]
pub(crate) struct MockProcessorWhoseOutputPortTheChannelGrammarCannotName;

impl crate::core::ManualProcessor
    for MockProcessorWhoseOutputPortTheChannelGrammarCannotName::Processor
{
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Register all engine-internal test mock processors with the global
/// `PROCESSOR_REGISTRY`. Idempotent — safe to call from every test
/// fixture that builds a graph against `lookup_registered_ident` or
/// drives the compiler against a `ProcessorSpec`.
pub(crate) fn ensure_test_mocks_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        PROCESSOR_REGISTRY.register::<MockProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockOutputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockWindowedAudioConsumerProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockDeviceMatchedAudioConsumerProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockInputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockOrderedInputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockReactiveInputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY
            .register::<MockProcessorWhoseOutputPortTheChannelGrammarCannotName::Processor>();
    });
}

/// Every `WARN`-level tracing event raised while this is the default
/// subscriber's layer, each rendered as its `field=value` pairs.
///
/// Structured fields are kept rather than only the message, because the port a
/// warning names is usually a field.
#[derive(Clone, Default)]
pub(crate) struct CapturedTracingWarnings(Arc<Mutex<Vec<String>>>);

impl CapturedTracingWarnings {
    /// Run `raising_them` with this installed on the calling thread, handing
    /// back what it returned alongside the warnings it raised.
    pub(crate) fn captured_while<T>(raising_them: impl FnOnce() -> T) -> (T, Vec<String>) {
        let captured = Self::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let returned = tracing::subscriber::with_default(subscriber, raising_them);
        let warnings = captured.0.lock().expect("no test panics holding this lock");
        (returned, warnings.clone())
    }
}

/// Renders one event's fields into `name=value` pairs, so a test can assert on
/// a structured field as readily as on the message.
struct EveryFieldOfOneCapturedWarning<'a>(&'a mut String);

impl EveryFieldOfOneCapturedWarning<'_> {
    fn push_one_field(&mut self, name: &str, rendered: std::fmt::Arguments<'_>) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = write!(self.0, "{name}={rendered}");
    }
}

impl Visit for EveryFieldOfOneCapturedWarning<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push_one_field(field.name(), format_args!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push_one_field(field.name(), format_args!("{value}"));
    }
}

impl<S: tracing::Subscriber> Layer<S> for CapturedTracingWarnings {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut rendered = String::new();
        event.record(&mut EveryFieldOfOneCapturedWarning(&mut rendered));
        self.0
            .lock()
            .expect("no test panics holding this lock")
            .push(rendered);
    }
}

/// One port a far side was asked to drop. Named rather than a tuple so a
/// swapped port and link id fails the assert instead of passing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReclaimedLink {
    pub(crate) port_direction: crate::core::PortDirection,
    pub(crate) local_port_name: String,
    pub(crate) link_id: String,
}

/// A far side past its setup command that records every link it is handed and
/// every port it is told to drop, holding each handed link's answer cell for the
/// test to answer.
///
/// Clones share what they record, which is how a test reads what the engine
/// asked of a far side it handed to an envelope.
#[derive(Clone, Default)]
pub(crate) struct RecordingOutOfProcessFarSideLinkDelivery {
    /// Every link handed over, with the direction it was wired in.
    pub(crate) late_wired_links:
        Arc<parking_lot::Mutex<Vec<(crate::core::PortDirection, serde_json::Value)>>>,
    /// The answer cells handed over, in the order their links were — how a
    /// test plays a far side that has not answered yet, opened its port, or
    /// refused.
    pub(crate) wire_answers_owed:
        Arc<parking_lot::Mutex<Vec<Arc<crate::core::processors::OutOfProcessLinkWireReply>>>>,
    /// Every port the far side was told to drop.
    pub(crate) reclaimed_links: Arc<parking_lot::Mutex<Vec<ReclaimedLink>>>,
}

impl crate::core::processors::OutOfProcessFarSideLinkDelivery
    for RecordingOutOfProcessFarSideLinkDelivery
{
    fn hand_over_a_link_wired_after_setup(
        &self,
        port_direction: crate::core::PortDirection,
        link_wiring: &serde_json::Value,
        answer_cell: Arc<crate::core::processors::OutOfProcessLinkWireReply>,
    ) -> crate::core::Result<()> {
        self.late_wired_links
            .lock()
            .push((port_direction, link_wiring.clone()));
        self.wire_answers_owed.lock().push(answer_cell);
        Ok(())
    }

    fn tell_the_far_side_a_link_was_unwired(
        &self,
        port_direction: crate::core::PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) -> crate::core::Result<()> {
        self.reclaimed_links.lock().push(ReclaimedLink {
            port_direction,
            local_port_name: local_port_name.to_string(),
            link_id: link_id.to_string(),
        });
        Ok(())
    }

    fn refuse_every_link_still_awaiting_the_far_sides_answer(&self, reason: &str) {
        for answer_cell in self.wire_answers_owed.lock().iter() {
            answer_cell.note_the_far_sides_answer(
                crate::core::processors::OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                    reason: reason.to_string(),
                },
            );
        }
    }
}

/// A `sleep` parked in a process group of its own, standing in for a helper
/// process.
pub(crate) fn a_process_parked_in_a_process_group_of_its_own() -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("sleep");
    command
        .arg("120")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setpgid` is async-signal-safe, the contract for `pre_exec`.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().expect("a parked process starts")
}

/// Whether every member of `process_group_id` is gone inside `budget`.
///
/// Signal 0 rather than a wait: a group's members are not necessarily this
/// process's children.
pub(crate) fn a_process_group_is_gone_within(
    process_group_id: libc::pid_t,
    budget: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + budget;
    // SAFETY: signal 0 delivers nothing; it only asks whether a member is left.
    while unsafe { libc::killpg(process_group_id, 0) } == 0 {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    true
}

/// Re-run the test at `test_path` in a child process of this test binary with
/// `environment_variable` set to `value`, and wait for it — for a test whose
/// passing ends the process it runs in.
pub(crate) fn rerun_this_test_in_a_child_process(
    test_path: &str,
    environment_variable: &str,
    value: &std::ffi::OsStr,
) -> std::process::Output {
    let child_process_output =
        std::process::Command::new(std::env::current_exe().expect("the test binary's own path"))
            .args([test_path, "--exact", "--test-threads=1", "--nocapture"])
            .env(environment_variable, value)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the test binary re-runs this test in a child process");
    // `--exact` on a name that matches nothing runs no test and exits 0, which
    // reads as a pass for a test that was renamed away.
    let child_standard_output = String::from_utf8_lossy(&child_process_output.stdout);
    assert!(
        child_standard_output.contains("running 1 test"),
        "the child process ran no test named `{test_path}`:\n{child_standard_output}"
    );
    child_process_output
}
