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
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{ModuleResolverStrategy, Runner};
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let runtime = Runner::new()?;

    // 1. Load `@tatolab/camera` and `@tatolab/display` from the
    //    workspace-staged location via the default resolver chain
    //    (workspace stage → installed cache). `cargo xtask build-plugins
    //    --package @tatolab/camera --package @tatolab/display` must
    //    have run first.
    runtime.add_module(module_ident_any_version!("tatolab", "camera"))?;
    runtime.add_module(module_ident_any_version!("tatolab", "display"))?;

    // 2. Load the sibling Deno sub-package — it lives at `./deno`
    //    relative to this example, isn't workspace-staged, so we
    //    resolve it by its manifest directory. The recursive dep
    //    walker follows its own dependencies.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    runtime.add_module_with(
        module_ident_any_version!("tatolab", "camera-deno-subprocess"),
        ModuleResolverStrategy::ManifestDirectory {
            path: manifest_dir.join("deno"),
        },
    )?;

    // 3. Add processors
    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        serde_json::json!({}),
    ))?;

    let halftone = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident_any_version!(
            "tatolab",
            "camera-deno-subprocess",
            "HalftoneProcessor"
        )?,
        serde_json::json!({}),
    ))?;

    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
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
