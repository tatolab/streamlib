// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fixture-driven JPEG decode PSNR rig.
//!
//! Feeds a single JPEG file through `JpegBytesSource → JpegDecoder →
//! Display` so the decoded PNG sampler can capture the decoded frame
//! for external PSNR comparison against the reference PNG that
//! produced the JPEG.
//!
//! Usage:
//!   jpeg-psnr <jpeg-path> <width> <height> <fps> <frame-count>
//!
//! Packages build automatically on `cargo run` via the build orchestrator.
//!` so the runtime can
//! resolve each cdylib at load time.

use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let jpeg_path = args
        .get(1)
        .cloned()
        .expect("missing <jpeg-path>: usage jpeg-psnr <jpeg-path> <w> <h> <fps> <frames>");
    let width: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1920);
    let height: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1080);
    let fps: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(10);
    let frame_count: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(15);

    println!("=== JPEG decode PSNR rig ===");
    println!("Fixture: {jpeg_path}");
    println!("Format:  {width}x{height} @ {fps}fps, {frame_count} frames\n");

    let runtime = Runner::new_with_orchestrator(streamlib::sdk::PolyglotBuildOrchestrator::default())?;

    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "debug-utilities"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/debug-utilities"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "jpeg"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/jpeg"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "display"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/display"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;

    let source = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "debug-utilities", "JpegBytesSource", "1.0.0"),
        serde_json::json!({
            "file_path": jpeg_path,
            "fps": fps,
            "frame_count": frame_count,
        }),
    ))?;
    println!("+ JpegBytesSource: {source}");

    let decoder = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "jpeg", "JpegDecoder", "1.0.0"),
        serde_json::json!({
            "max_width": width.max(1),
            "max_height": height.max(1),
        }),
    ))?;
    println!("+ JpegDecoder: {decoder}");

    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": width,
            "height": height,
            "title": "streamlib JPEG PSNR rig",
        }),
    ))?;
    println!("+ Display: {display}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "encoded_jpeg"),
        InputLinkPortRef::new(&decoder, "encoded_jpeg_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&decoder, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("\nPipeline: jpeg_bytes_source -> jpeg_decoder -> display\n");

    // Source emits `frame_count` JPEGs at `fps` then stops (its
    // background thread exits). Sleep covers the source's emit window
    // plus a small tail for decoder GPU dispatch + display draws.
    let seconds = frame_count / fps.max(1) + 3;
    println!("Starting pipeline for ~{seconds}s...\n");
    runtime.start()?;
    std::thread::sleep(std::time::Duration::from_secs(seconds as u64));
    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}
