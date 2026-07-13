// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Rust Grayscale Plugin → Display Pipeline Example
//!
//! Demonstrates an example-local cdylib plugin: the sibling `plugin/` crate
//! builds as a cdylib carrying the `GrayscaleRust` processor
//! (`@tatolab/camera-rust-plugin`). Under the no-load-call model it is linked
//! into this app's `streamlib_modules/` (by `./setup.sh`, via
//! `streamlib link ./plugin`) alongside `@tatolab/camera` and
//! `@tatolab/display`, and the runtime lazily discovers + loads it on the first
//! `processor_type_ref!` reference — no manual cdylib staging, no `add_module`
//! call in app code.
//!
//! ## Usage
//!
//! ```bash
//! ./setup.sh    # link the SDK + camera/display/plugin into streamlib_modules/
//! cargo run
//! ```

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;

fn main() -> Result<()> {
    let runtime = Runner::with_auto_build()?;

    let camera = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::json!({}),
    ))?;

    let grayscale = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera-rust-plugin", "GrayscaleRust"),
        serde_json::Value::Null,
    ))?;

    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "Camera → Rust Grayscale → Display",
        }),
    ))?;

    // Camera → Grayscale → Display
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&grayscale, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&grayscale, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;

    runtime.start()?;
    runtime.wait_for_signal()?;

    Ok(())
}
