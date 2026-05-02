// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Cyberpunk Pipeline (Breaking News PiP)
//!
//! Parallel video processing pipeline with multi-layer compositing.
//!
//! On macOS the full pipeline runs end-to-end:
//! - Camera feed always visible as base layer
//! - Python MediaPipe-based pose detection → Avatar character as PiP overlay
//! - PiP slides in from right when MediaPipe ready ("Breaking News" style)
//! - Python lower third + watermark overlays (continuous RGBA generators)
//! - Rust blending compositor (alpha blends all layers)
//! - Rust CRT + Film Grain effect (80s Blade Runner look)
//! - Python glitch effect (RGB separation, scanlines, slice displacement)
//!
//! On Linux only the AvatarCharacter half currently runs (#484): camera →
//! avatar → display. The rest of the pipeline is gated on #485 (Skia
//! overlays) and #486 (Glitch fragment). Python processors run as isolated
//! subprocesses; on Linux they ride `streamlib-adapter-cuda` (camera frame
//! → CUDA tensor for PyTorch pose detection) and `streamlib-adapter-opengl`
//! (ModernGL skinned mesh render → DMA-BUF for the display).
//!
//! ## Prerequisites
//!
//! - `uv` must be installed: <https://docs.astral.sh/uv/>
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-python-display
//! ```

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
fn main() {
    eprintln!(
        "camera-python-display currently supports macOS and Linux only. \
         BSD / Windows ports are out of scope."
    );
    std::process::exit(2);
}

// `blending_compositor` and `crt_film_grain` are cross-platform (macOS
// Metal + Linux Vulkan); the rest of the example pipeline still requires
// macOS until #485 (Skia overlays) and #486 (Glitch) land for Linux.
mod blending_compositor;
mod crt_film_grain;

// Linux-only — host-pipeline producer for the AvatarCharacter cuda
// inference path (#612). The proc-macro must see the module
// unconditionally on Linux to register the processor.
#[cfg(target_os = "linux")]
mod camera_to_cuda_copy;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod macos;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use macos::main as platform_main;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::main as platform_main;

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn main() -> streamlib::Result<()> {
    platform_main()
}
