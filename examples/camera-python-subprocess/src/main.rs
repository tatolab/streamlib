// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Python Grayscale → Display Pipeline Example
//!
//! The Python processor accesses camera pixels through IOSurface shared memory
//! (no pixel data through pipes), converts to grayscale using numpy, and writes
//! results to a new IOSurface.
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-python-subprocess
//! ```

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;

fn main() -> Result<()> {
    let runtime = Runner::with_auto_build()?;

    // No module-loading call: `@tatolab/camera`, `@tatolab/display`, and this
    // example's own `./python` package all live in this app's
    // `streamlib_modules/` folder (populated by `./setup.sh`). The runtime
    // lazily discovers + loads each on the first `processor_type_ref!` reference.

    // Add processors
    let camera = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::json!({}),
    ))?;

    let grayscale = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera-python-subprocess", "Grayscale"),
        serde_json::json!({}),
    ))?;

    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "Camera → Python Grayscale → Display",
        }),
    ))?;

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
