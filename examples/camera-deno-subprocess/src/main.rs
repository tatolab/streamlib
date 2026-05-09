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

use std::path::PathBuf;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::{CameraProcessor, DisplayProcessor, ProcessorSpec};
use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::Runner;

fn main() -> Result<()> {
    let runtime = Runner::new()?;
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("deno");

    // 1. Load processor package from streamlib.yaml
    runtime.load_project(&project_path)?;

    // 2. Add processors
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;

    let halftone = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident!(
            "tatolab",
            "camera-deno-subprocess",
            "HalftoneProcessor",
            "0.1.0"
        ),
        serde_json::json!({}),
    ))?;

    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("Camera → TypeGPU Halftone → Display".to_string()),
        ..Default::default()
    }))?;

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
