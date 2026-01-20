// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor infrastructure and implementations.

pub mod graph;
pub mod traits;

#[doc(hidden)]
pub mod __generated_private;

mod api_server;
mod processor_instance_factory;
mod processor_spec;
// Re-export graph types
pub use graph::{ProcessorState, ProcessorStateComponent};

// Re-export processor traits
pub use traits::{Config, ConfigValidationError};
// Mode-specific processor traits
pub use traits::{ContinuousProcessor, ManualProcessor, ReactiveProcessor};

// Re-export internal traits (doc-hidden but needed by macro and runtime)
#[doc(hidden)]
pub use __generated_private::{DynGeneratedProcessor, GeneratedProcessor};

pub use processor_instance_factory::{
    macro_codegen, DynamicProcessorConstructorFn, ProcessorInstance, ProcessorInstanceFactory,
    RegisterResult, PROCESSOR_REGISTRY,
};
pub use processor_spec::ProcessorSpec;

/// Empty config type for processors that don't need configuration.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmptyConfig;

// Sources
// TODO: Migrate to iceoryx2 API
// pub mod audio_capture;
// pub mod camera;
pub mod chord_generator;

// Sinks
// TODO: Migrate to iceoryx2 API
// pub mod audio_output;
// pub mod display;
// pub mod mp4_writer;

// Transformers
pub mod audio_channel_converter;
pub mod audio_mixer;
pub mod audio_resampler;
pub mod buffer_rechunker;
// TODO: Migrate to iceoryx2 API
// pub mod clap_effect;
pub mod simple_passthrough;

// Test processors for iceoryx2 communication validation
pub mod iceoryx2_test_consumer;
pub mod iceoryx2_test_producer;

// WebRTC Streaming
// TODO: Migrate to iceoryx2 API
// pub mod webrtc_whep;
// pub mod webrtc_whip;

// TODO: Migrate to iceoryx2 API
// pub use audio_capture::{AudioCaptureConfig, AudioCaptureProcessor, AudioInputDevice};
// pub use camera::{CameraConfig, CameraDevice, CameraProcessor};
pub use chord_generator::{ChordGeneratorConfig, ChordGeneratorProcessor};

// TODO: Migrate to iceoryx2 API
// pub use audio_output::{AudioDevice, AudioOutputConfig, AudioOutputProcessor};
// pub use display::{DisplayConfig, DisplayProcessor, WindowId};
// pub use mp4_writer::{Mp4WriterConfig, Mp4WriterProcessor};

pub use api_server::*;
pub use audio_channel_converter::{
    AudioChannelConverterConfig, AudioChannelConverterProcessor, ChannelConversionMode,
};
pub use audio_mixer::{AudioMixerConfig, AudioMixerProcessor, MixingStrategy};
pub use audio_resampler::{
    AudioResampler1chProcessor, AudioResampler2chProcessor, AudioResamplerConfig,
};
pub use buffer_rechunker::{
    BufferRechunker1chProcessor, BufferRechunker2chProcessor, BufferRechunkerConfig,
};
// TODO: Migrate to iceoryx2 API
// pub use clap_effect::{ClapEffectConfig, ClapEffectProcessor, ClapPluginInfo, ClapScanner};
pub use simple_passthrough::{SimplePassthroughConfig, SimplePassthroughProcessor};
// TODO: Migrate to iceoryx2 API
// pub use webrtc_whep::{WebRtcWhepConfig, WebRtcWhepProcessor};
// pub use webrtc_whip::{WebRtcWhipConfig, WebRtcWhipProcessor};

// iceoryx2 test processors
pub use iceoryx2_test_consumer::{Iceoryx2TestConsumerConfig, Iceoryx2TestConsumerProcessor};
pub use iceoryx2_test_producer::{Iceoryx2TestProducerConfig, Iceoryx2TestProducerProcessor};
