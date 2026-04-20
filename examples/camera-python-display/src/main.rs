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
//! - Rust blending compositor (alpha blends all layers)
//! - Rust CRT + Film Grain effect (80s Blade Runner look)
//! - Python glitch effect (RGB separation, scanlines, slice displacement)
//!
//! All Python processors run as isolated subprocesses with their own CGL contexts.
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

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn main() {
    eprintln!(
        "camera-python-display currently requires macOS — the in-tree \
         blending compositor and CRT/film-grain effect use Metal. A Vulkan \
         port is tracked as a follow-up to issue #358."
    );
    std::process::exit(2);
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod blending_compositor;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod crt_film_grain;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use std::path::PathBuf;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::{
    ApiServerConfig, ApiServerProcessor, CameraProcessor, DisplayProcessor, ProcessorSpec, Result,
    StreamRuntime,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
use blending_compositor::BlendingCompositorProcessor;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use crt_film_grain::CrtFilmGrainProcessor;

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn main() -> Result<()> {
    // Telemetry is initialized automatically by StreamRuntime::new()

    println!("=== Cyberpunk Pipeline (Breaking News PiP) ===\n");

    let runtime = StreamRuntime::new()?;
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");

    // Load processor package from streamlib.yaml
    runtime.load_project(&project_path)?;
    println!("✓ Loaded processor package from streamlib.yaml\n");

    // =========================================================================
    // Camera Source
    // =========================================================================

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!("✓ Camera added: {}\n", camera);

    // =========================================================================
    // Python Subprocess Processors
    // =========================================================================

    // Avatar Character (MediaPipe pose detection + stylized character)
    println!(
        "🐍 Adding Python avatar character (subprocess, MediaPipe pose + stylized character)..."
    );
    let avatar = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.avatar_character",
        serde_json::json!({}),
    ))?;
    println!("✓ Avatar character processor added: {}\n", avatar);

    // =========================================================================
    // Blending Compositor (Parallel layer blending)
    // =========================================================================

    println!("🎨 Adding blending compositor (parallel layer blending)...");
    let blending = runtime.add_processor(BlendingCompositorProcessor::node(Default::default()))?;
    println!("✓ Blending compositor added: {}\n", blending);

    // =========================================================================
    // PARALLEL: Lower Third (continuous RGBA generator)
    // =========================================================================

    println!("🐍 Adding Python lower third GENERATOR (subprocess, 16ms)...");
    let lower_third = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.cyberpunk_lower_third",
        serde_json::json!({}),
    ))?;
    println!("✓ Lower third generator added: {}\n", lower_third);

    // =========================================================================
    // PARALLEL: Watermark (continuous RGBA generator)
    // =========================================================================

    println!("🐍 Adding Python watermark GENERATOR (subprocess, 16ms)...");
    let watermark = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.cyberpunk_watermark",
        serde_json::json!({}),
    ))?;
    println!("✓ Watermark generator added: {}\n", watermark);

    // =========================================================================
    // CRT + Film Grain Effect (80s Blade Runner look)
    // =========================================================================

    println!("📺 Adding CRT + Film Grain processor (80s Blade Runner look)...");
    let crt_film_grain = runtime.add_processor(CrtFilmGrainProcessor::node(Default::default()))?;
    println!("✓ CRT + Film Grain processor added: {}\n", crt_film_grain);

    // =========================================================================
    // Glitch Effect
    // =========================================================================

    println!("🐍 Adding Python glitch processor (subprocess, RGB separation, scanlines)...");
    let glitch = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.cyberpunk_glitch",
        serde_json::json!({}),
    ))?;
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
        vsync: Some(true),
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
        name: None,
        log_path: None,
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
    println!("     Camera ──┬──→ BlendingCompositor ──→ CRT/FilmGrain ──→ Glitch ──→ Display");
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

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
