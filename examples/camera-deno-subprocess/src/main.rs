// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Deno Halftone → Display Pipeline Example
//!
//! Demonstrates GPU-accelerated pixel processing via a Deno/TypeScript subprocess
//! using WebGPU compute shaders (TypeGPU). The TypeScript processor accesses camera
//! pixels through IOSurface shared memory via FFI, applies a halftone dot pattern
//! effect on the GPU, and writes results to a new IOSurface.
//!
//! ## Prerequisites
//!
//! 1. Install Deno: `curl -fsSL https://deno.land/install.sh | sh`
//! 2. Build the native FFI lib: `cargo build -p streamlib-deno-native`
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-deno-subprocess
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
    // example's own `./deno` package all live in this app's
    // `streamlib_modules/` folder (populated by `./setup.sh`). The runtime
    // lazily discovers + loads each on the first `processor_type_ref!` reference.

    // Add processors
    let camera = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::json!({}),
    ))?;

    let halftone = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera-deno-subprocess", "HalftoneProcessor"),
        serde_json::json!({}),
    ))?;

    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "Camera → TypeGPU Halftone → Display",
        }),
    ))?;

    // 3. Connect: Camera → Deno Halftone → Display
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&halftone, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&halftone, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;

    // 4. Run
    runtime.start()?;
    runtime.wait_for_signal()?;

    Ok(())
}
