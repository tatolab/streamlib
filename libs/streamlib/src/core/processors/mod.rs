// Sources
pub mod chord_generator;
pub mod camera;
pub mod audio_capture;

// Sinks
pub mod display;
pub mod audio_output;
pub mod mp4_writer;

// Transformers
pub mod audio_mixer;
pub mod audio_resampler;
pub mod audio_channel_converter;
pub mod buffer_rechunker;
pub mod clap_effect;
pub mod simple_passthrough;

#[cfg(feature = "debug-overlay")]
pub mod performance_overlay;

// Source exports
pub use chord_generator::{ChordGeneratorProcessor, ChordGeneratorConfig};
pub use camera::{CameraProcessor, CameraDevice, CameraConfig};
pub use audio_capture::{AudioCaptureProcessor, AudioInputDevice, AudioCaptureConfig};

// Sink exports
pub use display::{DisplayProcessor, WindowId, DisplayConfig};
pub use audio_output::{AudioOutputProcessor, AudioDevice, AudioOutputConfig};
pub use mp4_writer::{Mp4WriterProcessor, Mp4WriterConfig};

// Transformer exports
pub use audio_mixer::{
    AudioMixerProcessor, MixingStrategy, AudioMixerConfig,
};
pub use audio_resampler::{
    AudioResamplerProcessor, AudioResamplerConfig, ResamplingQuality,
};
pub use audio_channel_converter::{
    AudioChannelConverterProcessor, AudioChannelConverterConfig, ChannelConversionMode,
};
pub use buffer_rechunker::{
    BufferRechunkerProcessor, BufferRechunkerConfig,
};
pub use clap_effect::{
    ClapEffectProcessor, ClapScanner, ClapPluginInfo, ClapEffectConfig,
};
pub use simple_passthrough::SimplePassthroughProcessor;

#[cfg(feature = "debug-overlay")]
pub use performance_overlay::{
    PerformanceOverlayProcessor,
    PerformanceOverlayConfig,
};
