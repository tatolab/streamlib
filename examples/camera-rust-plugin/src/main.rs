// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Rust Grayscale Plugin → Display Pipeline Example
//!
//! Demonstrates the in-tree cdylib plugin pattern: the example owns a
//! sibling `plugin/` crate that builds as a cdylib carrying the
//! `GrayscaleRust` processor, and the host manually stages that cdylib
//! into `plugin/lib/<host_triple>/` before calling
//! `runtime.add_module_with_blocking(..., Strategy::ManifestDirectory)`
//! to register it. This is the "build from source + load from path"
//! shape, distinct from the `cargo xtask build-plugins` +
//! `runtime.add_module_blocking(...)` flow that the other examples use for
//! canonical workspace packages.
//!
//! `@tatolab/camera` and `@tatolab/display` themselves load via
//! `add_module` against the workspace-staged plugin dir — only the
//! example-local plugin subdir goes through the manual stage path.
//!
//! ## Prerequisites
//!
//! Build the plugin cdylib + stage the canonical packages:
//! ```bash
//! cargo build -p grayscale-plugin
//! cargo xtask build-plugins --package @tatolab/camera --package @tatolab/display
//! ```
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-rust-plugin
//! ```

use std::path::PathBuf;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Strategy, Runner};
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let runtime = Runner::new_with_orchestrator(streamlib::sdk::PolyglotBuildOrchestrator::default())?;

    // 1. Load `@tatolab/camera` and `@tatolab/display` via the canonical
    //    workspace-staged path. `cargo xtask build-plugins --package
    //    @tatolab/camera --package @tatolab/display` must have run first.
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "camera"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/camera"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "display"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/display"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;

    // 2. Stage the example-local grayscale plugin cdylib at
    //    `plugin/lib/<triple>/` so the explicit `ManifestDirectory`
    //    resolver picks it up via the same triple-keyed convention
    //    `streamlib pack` produces. The plugin lives in this example's
    //    repo (sibling crate, not a workspace package) so the canonical
    //    xtask doesn't stage it; the example handles its own copy step.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let plugin_dir = manifest_dir.join("plugin");
    let host_triple = streamlib::sdk::runtime::host_target_triple();
    let triple_lib_dir = plugin_dir.join("lib").join(host_triple);
    std::fs::create_dir_all(&triple_lib_dir).map_err(|e| {
        streamlib::sdk::error::Error::Configuration(format!("Failed to create lib dir: {}", e))
    })?;

    // Derive workspace target dir: CARGO_MANIFEST_DIR is examples/camera-rust-plugin/,
    // workspace root is 2 levels up, target dir is workspace_root/target/
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to find workspace root");
    let dylib_name = if cfg!(target_os = "macos") {
        "libgrayscale_plugin.dylib"
    } else if cfg!(target_os = "windows") {
        "grayscale_plugin.dll"
    } else {
        "libgrayscale_plugin.so"
    };

    // Try debug first, then release
    let debug_dylib = workspace_root.join("target").join("debug").join(dylib_name);
    let release_dylib = workspace_root
        .join("target")
        .join("release")
        .join(dylib_name);

    let source_dylib = if debug_dylib.exists() {
        &debug_dylib
    } else if release_dylib.exists() {
        &release_dylib
    } else {
        eprintln!(
            "ERROR: Grayscale plugin dylib not found.\n\
             Build it first: cargo build -p grayscale-plugin\n\
             Looked in:\n  {}\n  {}",
            debug_dylib.display(),
            release_dylib.display()
        );
        std::process::exit(1);
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
    //    grayscale processor from the staged cdylib). The
    //    `ManifestDirectory` strategy preserves the recursive
    //    dep-walker shape — it reads `plugin/streamlib.yaml`, walks
    //    declared deps (`@tatolab/core` patched to a workspace path),
    //    and registers the local plugin's processors + schemas.
    runtime.add_module_with_blocking(
        module_ident_any_version!("tatolab", "camera-rust-plugin"),
        Strategy::Path { path: plugin_dir.clone(), build: BuildPolicy::IfStale },
    )?;

    // 4. Add processors
    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        serde_json::json!({}),
    ))?;

    let grayscale = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident_any_version!(
            "tatolab",
            "camera-rust-plugin",
            "GrayscaleRust"
        )?,
        serde_json::Value::Null,
    ))?;

    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "Camera → Rust Grayscale → Display",
        }),
    ))?;

    // 5. Connect: Camera → Grayscale → Display
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&grayscale, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&grayscale, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;

    // 6. Run
    runtime.start()?;
    runtime.wait_for_signal()?;

    Ok(())
}
