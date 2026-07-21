// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dynamic Reconfigure — live camera→display graph rewiring
//!
//! Starts a `@tatolab/camera` → `@tatolab/display` pipeline, then repeatedly
//! reconfigures the graph WHILE IT RUNS: it splices a `SimplePassthrough`
//! processor into the middle of the live graph (camera → passthrough → display)
//! and splices it back out (camera → display), N times, then auto-exits.
//!
//! There is no restart between reconfigure cycles — every `add_processor`,
//! `connect`, `disconnect`, and `remove_processor` call lands against the same
//! already-`start()`ed runtime, driven from the `wait_for_signal_with` callback.
//! This is the manual, visual counterpart to the headless regression test in
//! `runtime/streamlib-engine/tests/dynamic_reconfigure_live_splice.rs`.
//!
//! ## What you see
//!
//! `SimplePassthrough` forwards the single frame present when it starts (it is a
//! `manual` one-shot fixture, not a continuous effect), so while it is spliced
//! in the display HOLDS that frame; when it is spliced back out the display
//! resumes live camera video. The live → held → live transition each cycle is
//! the visible proof the reroute took effect on the running graph.
//!
//! ## Visual audit (headless / CI)
//!
//! Set `STREAMLIB_DISPLAY_PNG_SAMPLE_DIR` (and optionally
//! `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY`, default every 30 frames) before running
//! and the display samples frames to PNG throughout, so the pre/mid/post
//! reconfigure frames can be inspected without a window. See `/verify-live`.
//!
//! Tunables (all optional):
//! - `STREAMLIB_RECONFIGURE_CYCLES`   splice in/out this many times (default 3)
//! - `STREAMLIB_RECONFIGURE_DWELL_MS` monotonic dwell per phase   (default 2500)
//! - `STREAMLIB_CAMERA_DEVICE`        camera device id (else the camera default)

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() -> Result<()> {
    println!("=== Dynamic Reconfigure: live camera→display rewiring ===\n");

    let total_cycles = env_u64("STREAMLIB_RECONFIGURE_CYCLES", 3);
    let dwell = Duration::from_millis(env_u64("STREAMLIB_RECONFIGURE_DWELL_MS", 2500));

    let runtime = Runner::with_auto_build()?;

    println!("📷 Adding camera processor...");
    let mut camera_config = serde_json::Map::new();
    if let Ok(id) = std::env::var("STREAMLIB_CAMERA_DEVICE") {
        camera_config.insert("device_id".into(), serde_json::Value::String(id));
    }
    let camera = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::Value::Object(camera_config),
    ))?;
    println!("✓ Camera added: {camera}\n");

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "streamlib Dynamic Reconfigure",
        }),
    ))?;
    println!("✓ Display added: {display}\n");

    println!("🔗 Connecting camera → display (direct)...");
    let mut direct_link = Some(runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&display, "video"),
    )?);
    println!("✓ Pipeline connected\n");

    println!("▶️  Starting pipeline...");
    runtime.start()?;
    println!(
        "✓ Running. Will splice a passthrough in/out {total_cycles} time(s), \
         {} ms per phase, then exit. Ctrl+C to stop early.\n",
        dwell.as_millis()
    );

    // Live-reconfigure state machine, advanced from the wait_for_signal
    // callback (main thread; the display pumps its own event loop). Every op
    // below runs against the ALREADY-STARTED runtime — no restart between
    // cycles. Phase progression is gated on a monotonic `Instant`, never a
    // wall clock.
    let mut spliced: Option<(
        streamlib::sdk::graph::ProcessorUniqueId,
        streamlib::sdk::graph::LinkUniqueId,
        streamlib::sdk::graph::LinkUniqueId,
    )> = None;
    let mut cycles_done: u64 = 0;
    let mut phase_deadline = Instant::now() + dwell;

    // One phase transition. Returns Break once every cycle has run.
    let mut advance = move |rt: &Runner| -> Result<ControlFlow<()>> {
        if Instant::now() < phase_deadline {
            return Ok(ControlFlow::Continue(()));
        }

        match spliced.take() {
            None => {
                // Splice IN: camera → passthrough → display, live.
                println!("  ↳ cycle {}/{}: splicing passthrough IN", cycles_done + 1, total_cycles);
                let link = direct_link
                    .take()
                    .expect("direct link present while un-spliced");
                rt.disconnect(&link)?;

                let passthrough = rt.add_processor(ProcessorSpec::new(
                    processor_type_ref!("tatolab", "debug-utilities", "SimplePassthrough"),
                    serde_json::json!({ "scale": 1.0 }),
                ))?;
                let cam_to_pass = rt.connect(
                    OutputLinkPortRef::new(&camera, "video"),
                    InputLinkPortRef::new(&passthrough, "input"),
                )?;
                let pass_to_disp = rt.connect(
                    OutputLinkPortRef::new(&passthrough, "output"),
                    InputLinkPortRef::new(&display, "video"),
                )?;
                spliced = Some((passthrough, cam_to_pass, pass_to_disp));
            }
            Some((passthrough, cam_to_pass, pass_to_disp)) => {
                // Splice OUT: restore camera → display direct, live.
                println!("  ↳ cycle {}/{}: splicing passthrough OUT", cycles_done + 1, total_cycles);
                rt.disconnect(&cam_to_pass)?;
                rt.disconnect(&pass_to_disp)?;
                rt.remove_processor(&passthrough)?;
                direct_link = Some(rt.connect(
                    OutputLinkPortRef::new(&camera, "video"),
                    InputLinkPortRef::new(&display, "video"),
                )?);

                cycles_done += 1;
                if cycles_done >= total_cycles {
                    return Ok(ControlFlow::Break(()));
                }
            }
        }

        phase_deadline = Instant::now() + dwell;
        Ok(ControlFlow::Continue(()))
    };

    runtime.wait_for_signal_with(|rt| match advance(rt) {
        Ok(flow) => flow,
        Err(e) => {
            println!("✗ reconfigure step failed: {e}");
            ControlFlow::Break(())
        }
    })?;

    println!("\n⏹️  Reconfigure cycles complete — stopped.");
    Ok(())
}
