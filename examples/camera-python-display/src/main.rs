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
//! - `BlendingCompositor` (graphics-kernel + texture-cache RHI, #485)
//!   alpha-blends every layer into a render-target VkImage downstream
//!   consumers resolve via Path 1
//!
//! macOS support was removed in #485 — the original CGL+IOSurface path
//! predated the RHI and could not consume the tiled DMA-BUF VkImages
//! every modern producer in the codebase emits. Reintroducing a parity
//! macOS port belongs in a follow-up scoped against the surface
//! adapters (`streamlib-adapter-skia`, etc.) that already work on
//! macOS in tree.
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

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "camera-python-display is Linux-only. The pre-RHI macOS path was \
         removed in #485; see `src/main.rs` for context."
    );
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
mod blending_compositor;
// Sandboxed kernel wrapper for [`blending_compositor`] — was in
// `libs/streamlib/src/vulkan/rhi/` pre-#487; relocated as
// transitional content. See module-level doc for the migration path
// to RDG (#631).
#[cfg(target_os = "linux")]
mod blending_compositor_kernel;
#[cfg(target_os = "linux")]
mod camera_to_cuda_copy;
// CRT/Film-Grain post-effect: processor + sandboxed kernel wrapper.
// Wired into the Linux pipeline as
// `BlendingCompositor → CrtFilmGrain → Glitch → Display` (#487).
#[cfg(target_os = "linux")]
mod crt_film_grain;
#[cfg(target_os = "linux")]
mod crt_film_grain_kernel;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> streamlib::Result<()> {
    linux::main()
}
