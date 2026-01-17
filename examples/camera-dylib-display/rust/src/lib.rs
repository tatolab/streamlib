// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Rust processors for camera-python-display example.
//!
//! This crate is compiled as a cdylib plugin that can be loaded at runtime
//! by the StreamLib CLI or any application using `PluginLoader`.

mod blending_compositor;
mod crt_film_grain;
mod cyberpunk_compositor;

pub use blending_compositor::{BlendingCompositorConfig, BlendingCompositorProcessor};
pub use crt_film_grain::{CrtFilmGrainConfig, CrtFilmGrainProcessor};
pub use cyberpunk_compositor::{CyberpunkCompositorConfig, CyberpunkCompositorProcessor};

use streamlib_plugin_abi::export_plugin;

// The #[streamlib::processor] macro creates a module with a `Processor` type inside
export_plugin!(
    BlendingCompositorProcessor::Processor,
    CrtFilmGrainProcessor::Processor,
    CyberpunkCompositorProcessor::Processor,
);
