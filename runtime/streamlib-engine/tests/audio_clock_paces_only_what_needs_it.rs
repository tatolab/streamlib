// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The narrowed clock role, in a real graph: the timerfd audio clock paces
//! deviceless audio and nothing else.
//!
//! Every runtime used to start it, which is roughly 94 wakeups a second in a
//! graph with no audio in it, invoking nothing. What makes "a graph's audio
//! path has exactly one cadence source" true in the tree rather than merely
//! stated is that the timer is not running unless something is pacing on it —
//! device ticks and timer ticks cannot interleave if the timer is stopped.
//!
//! Starts a real `Runner` (GPU + iceoryx2), so this runs outside the `--lib`
//! gate, which never builds `tests/` integration binaries.

use std::sync::atomic::{AtomicBool, Ordering};

use serial_test::serial;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib_engine::core::context::{
    AudioCaptureStream, AudioDeviceBackend, AudioDeviceStreamRequest, SilentNullAudioDeviceBackend,
};
use streamlib_engine::core::processors::PROCESSOR_REGISTRY;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};

/// Recorded from inside the graph, because a processor's `start()` is exactly
/// where the runtime used to have the clock already running.
static AUDIO_CLOCK_WAS_RUNNING_IN_A_GRAPH_WITH_NO_AUDIO: AtomicBool = AtomicBool::new(false);
static AUDIO_CLOCK_WAS_RUNNING_ONCE_A_CAPTURE_STREAM_DELIVERED: AtomicBool = AtomicBool::new(false);

/// A processor with nothing to do with audio — the graph the clock must not
/// wake up for.
#[streamlib::sdk::processor(execution = manual)]
pub struct ProcessorThatNeedsNoAudioClock;

impl streamlib_engine::ManualProcessor for ProcessorThatNeedsNoAudioClock::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        AUDIO_CLOCK_WAS_RUNNING_IN_A_GRAPH_WITH_NO_AUDIO
            .store(ctx.audio_clock().is_running(), Ordering::SeqCst);
        Ok(())
    }
}

/// A processor that opens a deviceless capture stream through the seam, the
/// way `MicrophoneSource` does.
#[streamlib::sdk::processor(execution = manual)]
pub struct ProcessorThatPacesOnTheAudioClock {
    capture_stream: Option<Box<dyn AudioCaptureStream>>,
}

impl streamlib_engine::ManualProcessor for ProcessorThatPacesOnTheAudioClock::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // The null arm by name rather than through the chain's probe: what is
        // under test is the timer that paces a *deviceless* graph, and the
        // probe hands back whatever audio server the machine running this
        // happens to have — a device-paced arm ignores the clock by design,
        // which is the very thing that makes one cadence source true.
        self.capture_stream = Some(SilentNullAudioDeviceBackend.open_capture_stream(
            &AudioDeviceStreamRequest {
                device_id: None,
                deviceless_pacing_clock: std::sync::Arc::clone(ctx.audio_clock()),
            },
        )?);
        Ok(())
    }
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.capture_stream = None;
        Ok(())
    }
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let capture_stream = self
            .capture_stream
            .as_mut()
            .expect("setup opened the capture stream");
        capture_stream.start_delivering_to(Box::new(|_| {}))?;
        AUDIO_CLOCK_WAS_RUNNING_ONCE_A_CAPTURE_STREAM_DELIVERED
            .store(ctx.audio_clock().is_running(), Ordering::SeqCst);
        Ok(())
    }
}

fn run_a_graph_of(processor: ProcessorSpec) {
    let runtime = Runner::new().expect("Runner::new");
    runtime.add_processor(processor).expect("add the processor");
    runtime.start().expect("runtime start");
    runtime.stop().expect("runtime stop");
}

/// Mental revert: put `audio_clock.start()` back into `Runner::start` and this
/// fails — which is the whole of what the entry claims.
#[test]
#[serial]
fn a_graph_with_no_audio_in_it_never_starts_the_audio_clock() {
    PROCESSOR_REGISTRY.register::<ProcessorThatNeedsNoAudioClock::Processor>();
    AUDIO_CLOCK_WAS_RUNNING_IN_A_GRAPH_WITH_NO_AUDIO.store(true, Ordering::SeqCst);

    run_a_graph_of(ProcessorSpec::new(
        ProcessorThatNeedsNoAudioClock::processor_class_import_path(),
        serde_json::json!({}),
    ));

    assert!(
        !AUDIO_CLOCK_WAS_RUNNING_IN_A_GRAPH_WITH_NO_AUDIO.load(Ordering::SeqCst),
        "the timerfd clock was running in a graph with no audio in it — ~94 \
         wakeups a second invoking nothing"
    );
}

#[test]
#[serial]
fn a_capture_stream_that_paces_on_the_clock_starts_it() {
    PROCESSOR_REGISTRY.register::<ProcessorThatPacesOnTheAudioClock::Processor>();
    AUDIO_CLOCK_WAS_RUNNING_ONCE_A_CAPTURE_STREAM_DELIVERED.store(false, Ordering::SeqCst);

    run_a_graph_of(ProcessorSpec::new(
        ProcessorThatPacesOnTheAudioClock::processor_class_import_path(),
        serde_json::json!({}),
    ));

    assert!(
        AUDIO_CLOCK_WAS_RUNNING_ONCE_A_CAPTURE_STREAM_DELIVERED.load(Ordering::SeqCst),
        "a deviceless capture stream has nothing else to pace it, so beginning \
         delivery has to start the clock"
    );
}
