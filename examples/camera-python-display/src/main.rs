// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Cyberpunk Pipeline (Breaking News PiP) — Linux only.
//!
//! Parallel video processing pipeline with multi-layer compositing on the
//! canonical RHI / surface-adapter stack:
//!
//! - Camera feed always visible as base layer
//! - Python avatar character (PyTorch YOLOv8 pose + ModernGL) as PiP overlay
//! - PiP slides in after `BlendingCompositor` config delay (Breaking-News-style)
//! - Python lower third + watermark overlays via `streamlib.adapters.skia`
//!   (Skia-on-GL via `MakeGL(MakeEGL())`) — continuous RGBA generators
//! - `BlendingCompositor` (graphics-kernel + texture-cache RHI)
//!   alpha-blends every layer into a render-target VkImage downstream
//!   consumers resolve via Path 1
//!
//! The runner is pure glue — its dependencies, processors, and schemas
//! are all contributed by packages it loads at runtime via
//! `runtime.add_module` / `runtime.add_module_with`. The
//! `CrtFilmGrain` + `BlendingCompositor` Rust-backed processors live in
//! the sibling `effects/` package; the cyberpunk Python processors
//! live in the sibling `python/` package.
//!
//! macOS support was removed when the host pipeline standardised on
//! tiled DMA-BUF VkImages — the pre-RHI CGL+IOSurface path could not
//! consume those. A parity macOS port belongs in a follow-up scoped
//! against the surface adapters (`streamlib-adapter-skia`, etc.) that
//! already work on macOS in tree.
//!
//! ## Prerequisites
//!
//! - `uv` must be installed: <https://docs.astral.sh/uv/>
//! - The sibling effects cdylib must have been built first:
//!   `cargo build -p camera-python-display-effects`.
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-python-display
//! ```

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "camera-python-display is Linux-only. The pre-RHI macOS path was \
         removed; see `src/main.rs` for context."
    );
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> streamlib::sdk::error::Result<()> {
    linux::main()
}
