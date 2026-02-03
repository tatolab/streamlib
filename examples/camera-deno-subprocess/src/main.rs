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
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("deno");

    // 1. Load processor package from streamlib.toml
    runtime.load_package(&project_path)?;

    // 2. Add processors
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;

    let halftone = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.halftone-ts",
        serde_json::json!({
            "project_path": project_path.to_str().unwrap()
        }),
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
