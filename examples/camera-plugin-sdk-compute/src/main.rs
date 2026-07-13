// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Engine-Free Grayscale **Compute** Plugin → Display Pipeline
//!
//! The first example whose plugin links ONLY the engine-free
//! `streamlib-plugin-sdk` (never the `streamlib` engine facade). It proves a
//! cdylib built against the engine-free SDK can:
//!   1. resolve an incoming `VideoFrame` surface to a GPU `Texture`
//!      (`resolve_texture_registration_by_surface_id`), and
//!   2. run a SPIR-V **compute** kernel on it
//!      (`create_compute_kernel` + `create_texture_ring`),
//! then emit the transformed frame — the exact build configuration
//! `tatolab/drone-racer` ships its on-GPU ONNX preprocessing under.
//!
//! The sibling `plugin/` crate is a cdylib package
//! (`@tatolab/camera-plugin-sdk-compute`) carrying the `GrayscaleCompute`
//! processor. Under the no-load-call model it is linked into this app's
//! `streamlib_modules/` (by `./setup.sh`, via `streamlib link ./plugin`)
//! alongside `@tatolab/camera` and `@tatolab/display`, and the runtime lazily
//! discovers + loads it on first reference — no cdylib staging, no `add_module`.
//!
//! ## Usage
//!
//! ```bash
//! ./setup.sh
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
        processor_type_ref!("tatolab", "camera-plugin-sdk-compute", "GrayscaleCompute"),
        serde_json::Value::Null,
    ))?;

    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "Camera → Engine-Free Grayscale Compute → Display",
        }),
    ))?;

    // Camera → GrayscaleCompute → Display
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
