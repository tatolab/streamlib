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

use std::path::PathBuf;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let runtime = Runner::new()?;

    // 1. Load `@tatolab/camera` and `@tatolab/display` from the
    //    workspace-staged location. `cargo xtask build-plugins
    //    --package @tatolab/camera --package @tatolab/display` must
    //    have run first.
    runtime.load_workspace_packages(["@tatolab/camera", "@tatolab/display"])?;

    // 2. Load the runner's project — its streamlib.yaml declares
    //    the sibling Python sub-package via `patch: path: ../python`,
    //    so this single call registers the Python processor + schemas.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    runtime.load_project(&manifest_dir)?;

    // 3. Add processors
    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        serde_json::json!({}),
    ))?;

    let grayscale = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident_any_version!(
            "tatolab",
            "camera-python-subprocess",
            "Grayscale"
        )?,
        serde_json::json!({}),
    ))?;

    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
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
