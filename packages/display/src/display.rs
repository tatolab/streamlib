// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform-specific re-exports with unified names.

// Generated config type is platform-agnostic; re-export at crate root so
// callers can name `streamlib_display::DisplayConfig` without worrying
// about the codegen tree shape.
pub use crate::_generated_::tatolab__display::DisplayConfig;

#[cfg(target_os = "linux")]
pub use crate::linux::{LinuxDisplayProcessor as DisplayProcessor, LinuxWindowId as WindowId};

// macOS port is in-tree at `packages/display/src/apple/` but not compiled —
// the Metal-side rewrite onto a Metal-equivalent present target is tracked
// as a follow-up. Until then, building this crate on macOS yields no
// `DisplayProcessor` symbol.
#[cfg(any(target_os = "macos", target_os = "ios"))]
compile_error!(
    "streamlib-display does not yet build on macOS/iOS — the Metal rewrite onto a Metal present \
     target is tracked as a follow-up to #674. Build on Linux today."
);
