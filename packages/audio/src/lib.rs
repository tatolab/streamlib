// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/audio` — audio processors carved out of the streamlib engine.

pub mod _generated_;

// Cross-platform processors
pub mod audio_channel_converter;
pub mod audio_mixer;
pub mod audio_resampler;
pub mod buffer_rechunker;
pub mod chord_generator;

// Cross-platform shims that re-export the per-platform impl under a unified name.
pub mod audio_capture;
pub mod audio_output;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

pub use audio_capture::{AudioCaptureProcessor, AudioInputDevice};
pub use audio_channel_converter::AudioChannelConverterProcessor;
pub use audio_mixer::AudioMixerProcessor;
pub use audio_output::{AudioDevice, AudioOutputProcessor};
pub use audio_resampler::AudioResamplerProcessor;
pub use buffer_rechunker::BufferRechunkerProcessor;
pub use chord_generator::ChordGeneratorProcessor;
