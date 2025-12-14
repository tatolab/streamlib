// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor infrastructure and implementations.

pub mod graph;
pub mod traits;

mod dyn_processor;
mod dyn_processor_impl;
mod processor_registry_factory;

// Re-export graph types
pub use graph::{ProcessorState, ProcessorStateComponent};

// Re-export traits
pub use traits::{Config, ConfigValidationError, Processor};

pub use dyn_processor::DynProcessor;
pub use processor_registry_factory::{macro_codegen, ProcessorInstance, ProcessorRegistryFactory};

/// Empty config type for processors that don't need configuration.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmptyConfig;

// Sources
pub mod audio_capture;
pub mod camera;
pub mod chord_generator;

// Sinks
pub mod audio_output;
pub mod display;
pub mod mp4_writer;

// Transformers
pub mod audio_channel_converter;
pub mod audio_mixer;
pub mod audio_resampler;
pub mod buffer_rechunker;
pub mod clap_effect;
pub mod simple_passthrough;

pub use audio_capture::{AudioCaptureConfig, AudioCaptureProcessor, AudioInputDevice};
pub use camera::{CameraConfig, CameraDevice, CameraProcessor};
pub use chord_generator::{ChordGeneratorConfig, ChordGeneratorProcessor};

pub use audio_output::{AudioDevice, AudioOutputConfig, AudioOutputProcessor};
pub use display::{DisplayConfig, DisplayProcessor, WindowId};
pub use mp4_writer::{Mp4WriterConfig, Mp4WriterProcessor};

pub use audio_channel_converter::{
    AudioChannelConverterConfig, AudioChannelConverterProcessor, ChannelConversionMode,
};
pub use audio_mixer::{AudioMixerConfig, AudioMixerProcessor, MixingStrategy};
pub use audio_resampler::{AudioResamplerConfig, AudioResamplerProcessor};
pub use buffer_rechunker::{BufferRechunkerConfig, BufferRechunkerProcessor};
pub use clap_effect::{ClapEffectConfig, ClapEffectProcessor, ClapPluginInfo, ClapScanner};
pub use simple_passthrough::{SimplePassthroughConfig, SimplePassthroughProcessor};
