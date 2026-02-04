// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Python Grayscale → Display Pipeline Example
//!
//! Demonstrates loading processors from a `.slpkg` package bundle.
//! The Python processor accesses camera pixels through IOSurface shared memory
//! (no pixel data through pipes), converts to grayscale using numpy, and writes
//! results to a new IOSurface.
//!
//! ## Prerequisites
//!
//! Build the `.slpkg` package first:
//! ```bash
//! cargo run -p streamlib-cli -- pack examples/camera-python-subprocess/python
//! ```
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-python-subprocess
//! ```

use std::path::PathBuf;
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{CameraProcessor, DisplayProcessor, ProcessorSpec, Result, StreamRuntime};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,naga=warn,wgpu_core=warn,wgpu_hal=warn"
                    .parse()
                    .unwrap()
            }),
        )
        .init();

    let runtime = StreamRuntime::new()?;

    // 1. Load processors from .slpkg package bundle
    let slpkg_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("python/camera-python-subprocess-0.1.0.slpkg");
    runtime.load_package(&slpkg_path)?;

    // 2. Add processors
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;

    let grayscale = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.grayscale",
        serde_json::json!({}),
    ))?;

    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("Camera → Python Grayscale → Display".to_string()),
        ..Default::default()
    }))?;

    // 3. Connect: Camera → Python Grayscale → Display
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&grayscale, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&grayscale, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;

    // 4. Run
    runtime.start()?;
    runtime.wait_for_signal()?;

    Ok(())
}
