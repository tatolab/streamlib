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
//! The runner owns a sibling `plugin/` crate (a workspace member) that builds
//! as a cdylib carrying the `GrayscaleCompute` processor; the host stages that
//! cdylib into `plugin/lib/<host_triple>/` before calling
//! `add_module_with_blocking(..., Strategy::Path)`. `@tatolab/camera` and
//! `@tatolab/display` resolve from the Gitea registry via `Strategy::Registry`.
//!
//! ## Prerequisites
//!
//! Build the plugin cdylib (a member of this example's workspace):
//! ```bash
//! cargo build -p grayscale-compute-plugin
//! ```
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-plugin-sdk-compute
//! ```

use std::path::PathBuf;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Runner, SemVerRange, Strategy};
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let runtime = Runner::with_auto_build()?;

    // 1. Resolve `@tatolab/camera` and `@tatolab/display` from the Gitea
    //    generic registry by version. Endpoint comes from
    //    `STREAMLIB_REGISTRY_URL` (or `GITEA_URL`).
    let registry = || Strategy::Registry {
        version_req: SemVerRange::Any,
        build: BuildPolicy::IfStale,
    };
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "camera"), registry())?;
    runtime
        .add_module_with_blocking(module_ident_any_version!("tatolab", "display"), registry())?;

    // 2. Stage the example-local grayscale-compute plugin cdylib at
    //    `plugin/lib/<triple>/` so the `Path` resolver picks it up via the
    //    same triple-keyed convention `streamlib pack` produces.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let plugin_dir = manifest_dir.join("plugin");
    let host_triple = streamlib::sdk::runtime::host_target_triple();
    let triple_lib_dir = plugin_dir.join("lib").join(host_triple);
    std::fs::create_dir_all(&triple_lib_dir).map_err(|e| {
        streamlib::sdk::error::Error::Configuration(format!("Failed to create lib dir: {}", e))
    })?;

    // This example is its own workspace root (the `plugin/` cdylib is a
    // member), so `cargo build -p grayscale-compute-plugin` writes the dylib
    // into this crate's own `target/` dir.
    let dylib_name = "libgrayscale_compute_plugin.so";
    let debug_dylib = manifest_dir.join("target").join("debug").join(dylib_name);
    let release_dylib = manifest_dir
        .join("target")
        .join("release")
        .join(dylib_name);

    let source_dylib = if debug_dylib.exists() {
        &debug_dylib
    } else if release_dylib.exists() {
        &release_dylib
    } else {
        return Err(streamlib::sdk::error::Error::Configuration(format!(
            "Grayscale-compute plugin dylib not found. Build it first: \
             cargo build -p grayscale-compute-plugin\nLooked in:\n  {}\n  {}",
            debug_dylib.display(),
            release_dylib.display()
        )));
    };

    let dest_dylib = triple_lib_dir.join(dylib_name);
    std::fs::copy(source_dylib, &dest_dylib).map_err(|e| {
        streamlib::sdk::error::Error::Configuration(format!(
            "Failed to copy dylib from {} to {}: {}",
            source_dylib.display(),
            dest_dylib.display(),
            e
        ))
    })?;
    println!("Copied plugin dylib to {}", dest_dylib.display());

    // 3. Load the example-local plugin project (registers the
    //    `GrayscaleCompute` processor from the staged cdylib). The `Path`
    //    strategy reads `plugin/streamlib.yaml`, walks declared deps
    //    (`@tatolab/core` from the registry), and registers the local
    //    plugin's processors + schemas.
    runtime.add_module_with_blocking(
        module_ident_any_version!("tatolab", "camera-plugin-sdk-compute"),
        Strategy::Path {
            path: plugin_dir.clone(),
            build: BuildPolicy::IfStale,
        },
    )?;

    // 4. Add processors.
    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        serde_json::json!({}),
    ))?;

    let grayscale = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident_any_version!(
            "tatolab",
            "camera-plugin-sdk-compute",
            "GrayscaleCompute"
        )?,
        serde_json::Value::Null,
    ))?;

    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "Camera → Engine-Free Grayscale Compute → Display",
        }),
    ))?;

    // 5. Connect: Camera → GrayscaleCompute → Display
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&grayscale, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&grayscale, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;

    // 6. Run.
    runtime.start()?;
    runtime.wait_for_signal()?;

    Ok(())
}
