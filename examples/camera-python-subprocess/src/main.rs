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
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Runner, SemVerRange, Strategy};
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let runtime = Runner::with_auto_build()?;

    // 1. Resolve `@tatolab/camera` and `@tatolab/display` from the Gitea
    //    generic registry by version — the cross-repo consumer path. The
    //    orchestrator downloads each package's `.slpkg`, then prefers a
    //    matching prebuilt or builds the bundled source on the host. The
    //    registry endpoint comes from `STREAMLIB_REGISTRY_URL` (or
    //    `GITEA_URL`); run with e.g. `STREAMLIB_REGISTRY_URL=http://localhost:3300`.
    runtime.add_module_with_blocking(
        module_ident_any_version!("tatolab", "camera"),
        Strategy::Registry { version_req: SemVerRange::Any, build: BuildPolicy::IfStale },
    )?;
    runtime.add_module_with_blocking(
        module_ident_any_version!("tatolab", "display"),
        Strategy::Registry { version_req: SemVerRange::Any, build: BuildPolicy::IfStale },
    )?;

    // 2. Load the sibling Python sub-package — it lives at `./python`
    //    relative to this example, isn't workspace-staged, so we
    //    resolve it by its manifest directory. The recursive dep
    //    walker follows its own dependencies.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    runtime.add_module_with_blocking(
        module_ident_any_version!("tatolab", "camera-python-subprocess"),
        Strategy::Path { path: manifest_dir.join("python"), build: BuildPolicy::IfStale },
    )?;

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
