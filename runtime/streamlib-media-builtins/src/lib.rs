// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! First-party media built-ins: native pre-built blocks statically linked
//! into the wheel and instantiated from Python by configuration
//! (`rt.add(TestPatternSource)`), whose per-frame paths never enter the
//! interpreter.
//!
//! Written against the SDK's handle-shaped primitives only — pixel-buffer
//! pool, texture cache, present target — never private engine guts.

pub mod audio_block;
#[cfg(target_os = "linux")]
pub mod camera_source;
#[cfg(target_os = "linux")]
pub mod display_window;
#[cfg(test)]
mod msgpack_wire_test_support;
pub mod test_pattern_source;
#[cfg(target_os = "linux")]
pub mod v4l2_color;
pub mod video_frame;

pub use audio_block::{AudioBlock, AudioSampleDtype};
#[cfg(target_os = "linux")]
pub use camera_source::{CameraSource, CameraSourceConfig};
#[cfg(target_os = "linux")]
pub use display_window::{DisplayWindow, DisplayWindowConfig};
pub use test_pattern_source::{TestPatternSource, TestPatternSourceConfig};
pub use video_frame::VideoFrame;

use streamlib::sdk::processors::PROCESSOR_REGISTRY;

/// Register every media built-in on the process-wide registry. In-process
/// static registration (the api-server precedent) — no dlopen, and idempotent,
/// so hosts may call it more than once.
pub fn register_media_builtin_processor_types() {
    PROCESSOR_REGISTRY.register::<test_pattern_source::TestPatternSource::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<camera_source::CameraSource::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<display_window::DisplayWindow::Processor>();
}
