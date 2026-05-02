// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS path for camera-python-display.
//!
//! Wires the full Breaking-News-PiP pipeline: camera + four Python
//! processors (avatar, lower third, watermark, glitch) + Rust compositor +
//! CRT/film-grain. The Python processors all use CGL+IOSurface zero-copy
//! against the host's Metal GpuContext via the legacy
//! `gpu_full_access.acquire_surface` path. See `linux.rs` for the
//! adapter-cuda + adapter-opengl variant.

use std::path::PathBuf;

use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{
    ApiServerConfig, ApiServerProcessor, CameraProcessor, DisplayProcessor, ProcessorSpec, Result,
    StreamRuntime,
};

use crate::blending_compositor::BlendingCompositorProcessor;
use crate::crt_film_grain::CrtFilmGrainProcessor;

pub fn main() -> Result<()> {
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

    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&blending, "video_in"),
    )?;
    println!("   ✓ Camera → BlendingCompositor.video_in (always visible)");

    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&avatar, "video_in"),
    )?;
    println!("   ✓ Camera → AvatarCharacter (pose detection)");

    runtime.connect(
        OutputLinkPortRef::new(&avatar, "video_out"),
        InputLinkPortRef::new(&blending, "pip_in"),
    )?;
    println!("   ✓ AvatarCharacter → BlendingCompositor.pip_in (Breaking News PiP)");

    runtime.connect(
        OutputLinkPortRef::new(&lower_third, "video_out"),
        InputLinkPortRef::new(&blending, "lower_third_in"),
    )?;
    println!("   ✓ LowerThird → BlendingCompositor.lower_third_in");

    runtime.connect(
        OutputLinkPortRef::new(&watermark, "video_out"),
        InputLinkPortRef::new(&blending, "watermark_in"),
    )?;
    println!("   ✓ Watermark → BlendingCompositor.watermark_in");

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
    println!("   Press Cmd+Q to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
