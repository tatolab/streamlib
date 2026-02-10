// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Rust Grayscale Plugin → Display Pipeline Example
//!
//! Demonstrates loading a Rust cdylib processor via `load_project()`.
//! The grayscale plugin accesses camera pixels through IOSurface shared memory,
//! converts to grayscale using direct CVPixelBuffer access, and writes results
//! to a new IOSurface.
//!
//! ## Prerequisites
//!
//! Build the plugin cdylib first:
//! ```bash
//! cargo build -p grayscale-plugin
//! ```
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-rust-plugin
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

    // 1. Copy built dylib into plugin/lib/ so load_project() can find it
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let plugin_dir = manifest_dir.join("plugin");
    let lib_dir = plugin_dir.join("lib");
    std::fs::create_dir_all(&lib_dir).map_err(|e| {
        streamlib::StreamError::Configuration(format!("Failed to create lib dir: {}", e))
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

    let dest_dylib = lib_dir.join(dylib_name);
    std::fs::copy(source_dylib, &dest_dylib).map_err(|e| {
        streamlib::StreamError::Configuration(format!(
            "Failed to copy dylib from {} to {}: {}",
            source_dylib.display(),
            dest_dylib.display(),
            e
        ))
    })?;
    tracing::info!("Copied plugin dylib to {}", dest_dylib.display());

    // 2. Load plugin project (registers processors from the dylib)
    runtime.load_project(&plugin_dir)?;

    // 3. Add processors
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;

    let grayscale = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.grayscale_rust",
        serde_json::Value::Null,
    ))?;

    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("Camera → Rust Grayscale → Display".to_string()),
        ..Default::default()
    }))?;

    // 4. Connect: Camera → Grayscale → Display
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&grayscale, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&grayscale, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;

    // 5. Run
    runtime.start()?;
    runtime.wait_for_signal()?;

    Ok(())
}
