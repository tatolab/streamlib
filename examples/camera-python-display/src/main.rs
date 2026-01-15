// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Cyberpunk Pipeline (Breaking News PiP)
//!
//! Parallel video processing pipeline with multi-layer compositing:
//! - Camera feed always visible as base layer
//! - Python MediaPipe-based pose detection → Avatar character as PiP overlay
//! - PiP slides in from right when MediaPipe ready ("Breaking News" style)
//! - Python lower third overlay (continuous RGBA generator)
//! - Python watermark overlay (continuous RGBA generator)
//! - Rust blending compositor (alpha blends all layers) - loaded as plugin
//! - Rust CRT + Film Grain effect (80s Blade Runner look) - loaded as plugin
//! - Python glitch effect (RGB separation, scanlines, slice displacement)
//!
//! Pipeline Architecture:
//! ```
//!   Camera ──┬──→ BlendingCompositor ──→ CRT/Film ──→ Glitch ──→ Display
//!            │         ↑  ↑  ↑
//!            │         │  │  └── Watermark (16ms)
//!            │         │  └───── LowerThird (16ms)
//!            │         │
//!            └──→ AvatarCharacter (PiP, slides in from right)
//! ```
//!
//! ## Prerequisites
//!
//! - `uv` must be installed: https://docs.astral.sh/uv/
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-python-display
//! ```

mod plugin_loader;

use std::path::PathBuf;
use streamlib::core::processors::ProcessorSpec;
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{
    ApiServerConfig, ApiServerProcessor, CameraProcessor, DisplayProcessor, Result, StreamRuntime,
};
use streamlib_python::{PythonContinuousProcessor, PythonProcessorConfig};

fn main() -> Result<()> {
    // Initialize tracing subscriber FIRST
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,naga=warn,wgpu_core=warn,wgpu_hal=warn"
                    .parse()
                    .unwrap()
            }),
        )
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    // THEN initialize LogTracer to forward Python logging (via pyo3-log) to tracing
    tracing_log::LogTracer::init().expect("Failed to initialize LogTracer");

    println!("=== Cyberpunk Pipeline (Breaking News PiP) ===\n");

    // =========================================================================
    // Load Rust processor plugin
    // =========================================================================

    println!("🔌 Loading Rust processor plugin...");
    let mut loader = plugin_loader::PluginLoader::new();

    // The plugin is built to target/debug or target/release depending on build mode
    let plugin_path = get_plugin_path();
    let count = loader.load_plugin(&plugin_path)?;
    println!("✓ Loaded {} processor(s) from plugin\n", count);

    let runtime = StreamRuntime::new()?;
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");

    // =========================================================================
    // Camera Source
    // =========================================================================

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!("✓ Camera added: {}\n", camera);

    // Avatar Character (MediaPipe pose detection + stylized character)
    println!("🐍 Adding Python avatar character (MediaPipe pose + stylized character)...");
    let avatar = runtime.add_processor(PythonContinuousProcessor::node(PythonProcessorConfig {
        project_path: project_path.clone(),
        class_name: "AvatarCharacter".to_string(),
        entry_point: Some("avatar_character.py".to_string()),
    }))?;
    println!("✓ Avatar character processor added: {}\n", avatar);

    // =========================================================================
    // Blending Compositor (Parallel layer blending) - from plugin
    // =========================================================================

    println!("🎨 Adding blending compositor (parallel layer blending)...");
    let blending = runtime.add_processor(ProcessorSpec::new(
        "BlendingCompositor",
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "pip_slide_duration": 0.5
        }),
    ))?;
    println!("✓ Blending compositor added: {}\n", blending);

    // =========================================================================
    // PARALLEL: Lower Third (continuous RGBA generator)
    // =========================================================================

    println!("🐍 Adding Python lower third GENERATOR (parallel, 16ms)...");
    let lower_third =
        runtime.add_processor(PythonContinuousProcessor::node(PythonProcessorConfig {
            project_path: project_path.clone(),
            class_name: "CyberpunkLowerThird".to_string(),
            entry_point: Some("cyberpunk_lower_third.py".to_string()),
        }))?;
    println!("✓ Lower third generator added: {}\n", lower_third);

    // =========================================================================
    // PARALLEL: Watermark (continuous RGBA generator)
    // =========================================================================

    println!("🐍 Adding Python watermark GENERATOR (parallel, 16ms)...");
    let watermark =
        runtime.add_processor(PythonContinuousProcessor::node(PythonProcessorConfig {
            project_path: project_path.clone(),
            class_name: "CyberpunkWatermark".to_string(),
            entry_point: Some("cyberpunk_watermark.py".to_string()),
        }))?;
    println!("✓ Watermark generator added: {}\n", watermark);

    // =========================================================================
    // CRT + Film Grain Effect (80s Blade Runner look) - from plugin
    // =========================================================================

    println!("📺 Adding CRT + Film Grain processor (80s Blade Runner look)...");
    let crt_film_grain = runtime.add_processor(ProcessorSpec::new(
        "CrtFilmGrain",
        serde_json::json!({
            "crt_curve": 0.7,
            "scanline_intensity": 0.6,
            "chromatic_aberration": 0.004,
            "grain_intensity": 0.18,
            "grain_speed": 1.0,
            "vignette_intensity": 0.5,
            "brightness": 2.2
        }),
    ))?;
    println!("✓ CRT + Film Grain processor added: {}\n", crt_film_grain);

    // =========================================================================
    // Glitch Effect
    // =========================================================================

    println!("🐍 Adding Python glitch processor (RGB separation, scanlines)...");
    let glitch = runtime.add_processor(PythonContinuousProcessor::node(PythonProcessorConfig {
        project_path,
        class_name: "CyberpunkGlitch".to_string(),
        entry_point: Some("cyberpunk_glitch.py".to_string()),
    }))?;
    println!("✓ Glitch processor added: {}\n", glitch);

    // =========================================================================
    // DISPLAY: Output
    // =========================================================================

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("Cyberpunk Pipeline (Parallel)".to_string()),
        scaling_mode: Default::default(),
        vsync: false, // Uncapped - run as fast as possible
        ..Default::default()
    }))?;
    println!("✓ Display added: {}\n", display);

    // =========================================================================
    // API Server (for debugging)
    // =========================================================================

    println!("🌐 Adding API server processor...");
    let _api_server = runtime.add_processor(ApiServerProcessor::node(ApiServerConfig {
        host: "127.0.0.1".to_string(),
        port: 9000,
        ..Default::default()
    }))?;
    println!("✓ API server running at http://127.0.0.1:9000");
    println!("   Registry: http://127.0.0.1:9000/registry\n");

    // =========================================================================
    // Connect the Pipeline (Breaking News PiP Architecture)
    // =========================================================================

    println!("🔗 Connecting pipeline (Breaking News PiP)...");

    // Camera → BlendingCompositor.video_in (direct - camera always visible)
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&blending, "video_in"),
    )?;
    println!("   ✓ Camera → BlendingCompositor.video_in (always visible)");

    // Camera → AvatarCharacter (parallel - pose detection)
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&avatar, "video_in"),
    )?;
    println!("   ✓ Camera → AvatarCharacter (pose detection)");

    // AvatarCharacter → BlendingCompositor.pip_in (PiP overlay, slides in from right)
    runtime.connect(
        OutputLinkPortRef::new(&avatar, "video_out"),
        InputLinkPortRef::new(&blending, "pip_in"),
    )?;
    println!("   ✓ AvatarCharacter → BlendingCompositor.pip_in (Breaking News PiP)");

    // Pipeline 2: LowerThird → BlendingCompositor.lower_third_in
    runtime.connect(
        OutputLinkPortRef::new(&lower_third, "video_out"),
        InputLinkPortRef::new(&blending, "lower_third_in"),
    )?;
    println!("   ✓ LowerThird → BlendingCompositor.lower_third_in");

    // Pipeline 3: Watermark → BlendingCompositor.watermark_in
    runtime.connect(
        OutputLinkPortRef::new(&watermark, "video_out"),
        InputLinkPortRef::new(&blending, "watermark_in"),
    )?;
    println!("   ✓ Watermark → BlendingCompositor.watermark_in");

    // BlendingCompositor → CRT/FilmGrain → Glitch → Display
    runtime.connect(
        OutputLinkPortRef::new(&blending, "video_out"),
        InputLinkPortRef::new(&crt_film_grain, "video_in"),
    )?;
    println!("   ✓ BlendingCompositor → CRT/FilmGrain");

    runtime.connect(
        OutputLinkPortRef::new(&crt_film_grain, "video_out"),
        InputLinkPortRef::new(&glitch, "video_in"),
    )?;
    println!("   ✓ CRT/FilmGrain → Glitch");

    runtime.connect(
        OutputLinkPortRef::new(&glitch, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("   ✓ Glitch → Display");
    println!();

    // =========================================================================
    // Run the pipeline
    // =========================================================================

    println!("▶️  Starting pipeline...");
    println!("   Architecture (Breaking News PiP):");
    println!("     Camera ──┬──→ BlendingCompositor ──→ CRT/Film ──→ Glitch ──→ Display");
    println!("              │         ↑  ↑  ↑");
    println!("              │         │  │  └── Watermark");
    println!("              │         │  └───── LowerThird");
    println!("              │         │");
    println!("              └──→ AvatarCharacter (PiP slides in from right)");
    println!();
    #[cfg(target_os = "macos")]
    println!("   Press Cmd+Q to stop\n");
    #[cfg(not(target_os = "macos"))]
    println!("   Press Ctrl+C to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    // Keep plugin loader alive until runtime stops
    drop(loader);

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}

/// Get the path to the plugin library.
fn get_plugin_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Navigate from examples/camera-python-display to workspace root
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();

    // Determine build profile (debug or release)
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    // Platform-specific library name
    #[cfg(target_os = "macos")]
    let lib_name = "libcamera_python_display_processors.dylib";
    #[cfg(target_os = "linux")]
    let lib_name = "libcamera_python_display_processors.so";
    #[cfg(target_os = "windows")]
    let lib_name = "camera_python_display_processors.dll";

    workspace_root.join("target").join(profile).join(lib_name)
}
