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

#[cfg(feature = "debug-overlay")]
pub mod performance_overlay;

// Source exports
pub use audio_capture::{AudioCaptureConfig, AudioCaptureProcessor, AudioInputDevice};
pub use camera::{CameraConfig, CameraDevice, CameraProcessor};
pub use chord_generator::{ChordGeneratorConfig, ChordGeneratorProcessor};

// Sink exports
pub use audio_output::{AudioDevice, AudioOutputConfig, AudioOutputProcessor};
pub use display::{DisplayConfig, DisplayProcessor, WindowId};
pub use mp4_writer::{Mp4WriterConfig, Mp4WriterProcessor};

// Transformer exports
pub use audio_channel_converter::{
    AudioChannelConverterConfig, AudioChannelConverterProcessor, ChannelConversionMode,
};
pub use audio_mixer::{AudioMixerConfig, AudioMixerProcessor, MixingStrategy};
pub use audio_resampler::{AudioResamplerConfig, AudioResamplerProcessor, ResamplingQuality};
pub use buffer_rechunker::{BufferRechunkerConfig, BufferRechunkerProcessor};
pub use clap_effect::{ClapEffectConfig, ClapEffectProcessor, ClapPluginInfo, ClapScanner};
pub use simple_passthrough::SimplePassthroughProcessor;

#[cfg(feature = "debug-overlay")]
pub use performance_overlay::{PerformanceOverlayConfig, PerformanceOverlayProcessor};
