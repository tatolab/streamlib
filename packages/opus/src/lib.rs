// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/opus` — Opus audio encoder + decoder processors via libopus.
//!
//! Cross-platform Rust gated to macOS / iOS / Linux (no Windows — the
//! upstream `opus` crate isn't available there).

pub mod _generated_;

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub mod opus_encoder;

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub mod opus_decoder;

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub use opus_decoder::{OpusDecoder, OpusDecoderProcessor};

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub use opus_encoder::{
    AudioEncoderConfig, AudioEncoderOpus, OpusEncoder, OpusEncoderProcessor,
};

pub use _generated_::{OpusDecoderConfig, OpusEncoderConfig};
