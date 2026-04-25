// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Python DMA-BUF consumer → Display pipeline (Linux).
//!
//! Pipeline-level gate for the polyglot consumer DMA-BUF FD path shipped in
//! #394 / #420. The Python subprocess receives camera frames over IPC,
//! calls `ctx.gpu_limited_access.resolve_surface(frame.surface_id)` to import
//! the host-allocated DMA-BUF, locks it, reads a probe byte, then forwards
//! the frame unmodified to the display.
//!
//! Usage:
//!   cargo run -p polyglot-dma-buf-consumer-scenario -- [device] [seconds] [--negative]
//!
//! Defaults to `/dev/video2` (the canonical vivid index in `docs/testing.md`)
//! for 15 seconds. On hosts where vivid landed at a different index (e.g.
//! `/dev/video0` after a UVC device unplug) pass the device path explicitly.
//! The `--negative` flag sets the consumer's `force_bad_surface_id` config so
//! resolve_surface fails deterministically on every frame — the pipeline
//! must still shut down cleanly.
//!
//! Build the .slpkg first (or it will not be found):
//!   cargo run -p streamlib-cli -- pack examples/polyglot-dma-buf-consumer/python

use std::path::PathBuf;

use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{
    CameraProcessor, DisplayProcessor, ProcessorSpec, Result, StreamRuntime,
};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).peekable();

    let mut device = "/dev/video2".to_string();
    let mut duration_secs: u64 = 15;
    let mut negative = false;
    let mut positional: Vec<String> = Vec::new();

    while let Some(a) = args.next() {
        if a == "--negative" {
            negative = true;
        } else {
            positional.push(a);
        }
    }
    if let Some(d) = positional.first() {
        device = d.clone();
    }
    if let Some(s) = positional.get(1) {
        duration_secs = s.parse().unwrap_or(duration_secs);
    }

    println!("=== Polyglot DMA-BUF Consumer Scenario ===");
    println!("Camera:   {device}");
    println!("Duration: {duration_secs}s");
    println!("Mode:     {}", if negative { "negative (force_bad_surface_id)" } else { "normal" });
    println!();

    let runtime = StreamRuntime::new()?;

    let slpkg_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("python/polyglot-dma-buf-consumer-0.1.0.slpkg");
    if !slpkg_path.exists() {
        return Err(streamlib::core::StreamError::Configuration(format!(
            "Package not found: {}\nRun: cargo run -p streamlib-cli -- pack examples/polyglot-dma-buf-consumer/python",
            slpkg_path.display()
        )));
    }
    runtime.load_package(&slpkg_path)?;

    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: Some(device.clone()),
        ..Default::default()
    }))?;
    println!("+ Camera: {camera}");

    let consumer_config = serde_json::json!({
        "force_bad_surface_id": negative,
    });
    let consumer = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.dma_buf_consumer",
        consumer_config,
    ))?;
    println!("+ Consumer: {consumer}");

    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("streamlib polyglot DMA-BUF consumer".to_string()),
        ..Default::default()
    }))?;
    println!("+ Display: {display}");

    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&consumer, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&consumer, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("\nPipeline: camera -> python consumer -> display");

    println!("Starting pipeline for {duration_secs}s...\n");
    runtime.start()?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs));

    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}
