// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The narrowed clock role, in a real graph: the timerfd audio clock paces
//! deviceless audio and nothing else.
//!
//! Every runtime used to start it, which is roughly 94 wakeups a second in a
//! graph with no audio in it, invoking nothing. What makes "a graph's audio
//! path has exactly one cadence source" true in the tree rather than merely
//! stated is that the timer is not running unless something is pacing on it —
//! device ticks and timer ticks cannot interleave if the timer is stopped.
//!
//! Starts a real stream (GPU + iceoryx2), so this runs outside the `--lib`
//! gate, which never builds `tests/` integration binaries.

use std::sync::atomic::{AtomicBool, Ordering};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

use serial_test::serial;
use streamlib::sdk::nodes::NodeSpec;
use streamlib::sdk::runtime::Runner;
use streamlib_engine::core::context::{
    AudioDeviceBackend, AudioDeviceStreamRequest, CaptureDeviceAccessPrompting,
    RequestedAudioStreamFormat, SilentNullAudioDeviceBackend,
};
use streamlib_engine::core::nodes::NODE_REGISTRY;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};

static AUDIO_CLOCK_WAS_RUNNING_IN_A_GRAPH_WITH_NO_AUDIO: AtomicBool = AtomicBool::new(false);
static AUDIO_CLOCK_WAS_RUNNING_ONCE_A_CAPTURE_STREAM_DELIVERED: AtomicBool = AtomicBool::new(false);

#[streamlib::sdk::node(execution = manual)]
pub fn node_that_needs_no_audio_clock(ctx: &RuntimeContextFullAccess<'_>) {
    AUDIO_CLOCK_WAS_RUNNING_IN_A_GRAPH_WITH_NO_AUDIO
        .store(ctx.audio_clock().is_running(), Ordering::SeqCst);
}

#[streamlib::sdk::node(execution = manual)]
pub fn node_that_paces_on_the_audio_clock(ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
    // The null arm by name rather than through the chain's probe: what is
    // under test is the timer that paces a *deviceless* graph, and the
    // probe hands back whatever audio server the machine running this
    // happens to have — a device-paced arm ignores the clock by design,
    // which is the very thing that makes one cadence source true.
    let mut capture_stream =
        SilentNullAudioDeviceBackend.open_capture_stream(&AudioDeviceStreamRequest {
            device_id: None,
            deviceless_pacing_clock: std::sync::Arc::clone(ctx.audio_clock()),
            capture_device_access_prompting: CaptureDeviceAccessPrompting::MayAskTheUser,
            requested_format: RequestedAudioStreamFormat::default(),
        })?;
    capture_stream.start_delivering_to(Box::new(|_| {}))?;
    AUDIO_CLOCK_WAS_RUNNING_ONCE_A_CAPTURE_STREAM_DELIVERED
        .store(ctx.audio_clock().is_running(), Ordering::SeqCst);
    // What is under test is that beginning delivery starts the clock; the stream closes as the
    // body returns.
    Ok(())
}

fn run_a_graph_of(node: NodeSpec) {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    stream.add_node(node).expect("add the node");
    stream.start().expect("stream start");
    stream.stop().expect("stream stop");
}

/// Mental revert: put `audio_clock.start()` back into
/// `LoadedStreamInThisRuntime::start` and this
/// fails — which is the whole of what the entry claims.
#[test]
#[serial]
fn a_graph_with_no_audio_in_it_never_starts_the_audio_clock() {
    NODE_REGISTRY.register::<NodeThatNeedsNoAudioClock::Node>();
    AUDIO_CLOCK_WAS_RUNNING_IN_A_GRAPH_WITH_NO_AUDIO.store(true, Ordering::SeqCst);

    run_a_graph_of(NodeSpec::new(
        NodeThatNeedsNoAudioClock::node_class_import_path(),
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
    NODE_REGISTRY.register::<NodeThatPacesOnTheAudioClock::Node>();
    AUDIO_CLOCK_WAS_RUNNING_ONCE_A_CAPTURE_STREAM_DELIVERED.store(false, Ordering::SeqCst);

    run_a_graph_of(NodeSpec::new(
        NodeThatPacesOnTheAudioClock::node_class_import_path(),
        serde_json::json!({}),
    ));

    assert!(
        AUDIO_CLOCK_WAS_RUNNING_ONCE_A_CAPTURE_STREAM_DELIVERED.load(Ordering::SeqCst),
        "a deviceless capture stream has nothing else to pace it, so beginning \
         delivery has to start the clock"
    );
}
