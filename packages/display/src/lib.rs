// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/display` — display processor carved out of the streamlib engine.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

// Cross-platform shim that re-exports the per-platform impl under a unified name.
pub mod display;

#[cfg(target_os = "linux")]
pub mod linux;

// The Apple module is ported into this package but intentionally NOT
// compiled in: the macOS rewrite onto a Metal-equivalent present target
// is tracked as a separate follow-up. Keeping the source in-tree as
// `#[allow(dead_code)] mod apple` would still drag every Metal /
// objc2 / RhiTextureCache reach-through into the compile, which is the
// wrong contract for this carve-out.

pub use display::{DisplayConfig, DisplayProcessor};

#[cfg(all(feature = "plugin", target_os = "linux"))]
streamlib_plugin_abi::export_plugin!(crate::DisplayProcessor::Processor);
