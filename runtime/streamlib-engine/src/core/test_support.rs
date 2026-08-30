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
        PROCESSOR_REGISTRY.register::<MockReactiveInputOnlyProcessor::Processor>();
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
