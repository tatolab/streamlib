// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// VideoFrame Counter Processor
//
// Reactive sink that consumes VideoFrames and records observations into
// process-global atomics. Integration tests assert on these after
// `runtime.stop()` to lock that frames actually flowed end-to-end
// through the graph — not just that start/stop bracketed cleanly.
//
// One-counter-per-process is intentional: tests using this sink must
// run `#[serial]` (every existing in-tree integration test already does
// for iceoryx2 service-name reasons). The reset() helper zeroes the
// state at the top of each test.

use crate::_generated_::VideoFrame;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib_plugin_sdk::sdk::error::Result;

/// Total VideoFrames the counter has consumed since the last reset.
pub static FRAMES_OBSERVED: AtomicU64 = AtomicU64::new(0);
/// Width of the first VideoFrame the counter saw since the last reset
/// (0 when no frames have arrived).
pub static FIRST_WIDTH: AtomicU32 = AtomicU32::new(0);
/// Height of the first VideoFrame the counter saw since the last reset
/// (0 when no frames have arrived).
pub static FIRST_HEIGHT: AtomicU32 = AtomicU32::new(0);
/// Length of the first VideoFrame's `surface_id` since the last reset
/// (a non-empty `surface_id` is the cheapest proof the decoder
/// registered a slot in the texture cache before publishing).
pub static FIRST_SURFACE_ID_LEN: AtomicU32 = AtomicU32::new(0);
/// Clone of the first VideoFrame the counter saw since the last reset.
/// Used by tests that want to inspect richer fields (color_info,
/// mastering_display, content_light) the atomic-only summaries can't
/// represent.
pub static FIRST_FRAME: Mutex<Option<VideoFrame>> = Mutex::new(None);

/// Reset the counter statics. Call at the top of each test before
/// `runtime.start()`.
pub fn reset() {
    FRAMES_OBSERVED.store(0, Ordering::Relaxed);
    FIRST_WIDTH.store(0, Ordering::Relaxed);
    FIRST_HEIGHT.store(0, Ordering::Relaxed);
    FIRST_SURFACE_ID_LEN.store(0, Ordering::Relaxed);
    *FIRST_FRAME.lock().expect("FIRST_FRAME mutex poisoned") = None;
}

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/debug-utilities/VideoFrameCounter",
    description = "Counts incoming VideoFrames into process-global atomics so integration tests can assert on frame count + first-frame dimensions after runtime.stop()",
    execution = reactive,
    config = crate::_generated_::VideoFrameCounterConfig,
    input("input", "@tatolab/core/VideoFrame", delivery_profile = "every_sample", description = "VideoFrame stream to observe"),
)]
pub struct VideoFrameCounterProcessor;

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for VideoFrameCounterProcessor::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("input") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("input")?;
        let n = FRAMES_OBSERVED.fetch_add(1, Ordering::Relaxed);
        if n == 0 {
            FIRST_WIDTH.store(frame.width, Ordering::Relaxed);
            FIRST_HEIGHT.store(frame.height, Ordering::Relaxed);
            FIRST_SURFACE_ID_LEN.store(frame.surface_id.len() as u32, Ordering::Relaxed);
            *FIRST_FRAME.lock().expect("FIRST_FRAME mutex poisoned") = Some(frame);
        }
        Ok(())
    }

    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}
