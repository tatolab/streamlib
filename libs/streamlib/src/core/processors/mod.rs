//! Standard processor traits
//!
//! Defines common processor types (Camera, Display, Audio) that platform implementations
//! provide concrete implementations for.
//!
//! ## Organization (v2.0.0 Architecture)
//!
//! - **sources/**: Source processors (StreamSource trait)
//! - **sinks/**: Sink processors (StreamSink trait) - Phase 3
//! - **transforms/**: Transform processors (StreamTransform trait) - Phase 4
//! - **Legacy processors**: Still using old StreamProcessor trait (will be migrated)

// New v2.0.0 architecture
pub mod sources;

// Legacy processors (to be migrated)
pub mod camera;
pub mod display;
pub mod audio_output;
pub mod audio_capture;
pub mod audio_effect;
pub mod audio_mixer;
pub mod clap_effect;
pub mod parameter_modulation;
pub mod parameter_automation;
pub mod simple_passthrough;

#[cfg(feature = "debug-overlay")]
pub mod performance_overlay;

// v2.0.0 architecture exports (new trait system)
pub use sources::{TestToneGenerator, TestToneGeneratorOutputPorts};

// Legacy exports (old StreamProcessor trait)
pub use camera::{CameraProcessor, CameraDevice, CameraOutputPorts};
pub use display::{DisplayProcessor, WindowId, DisplayInputPorts};
pub use audio_output::{AudioOutputProcessor, AudioDevice, AudioOutputInputPorts};
pub use audio_capture::{AudioCaptureProcessor, AudioInputDevice, AudioCaptureOutputPorts};
pub use audio_effect::{
    AudioEffectProcessor, ParameterInfo, PluginInfo,
    AudioEffectInputPorts, AudioEffectOutputPorts,
};
pub use audio_mixer::{
    AudioMixerProcessor, MixingStrategy,
    AudioMixerInputPorts, AudioMixerOutputPorts,
};
pub use clap_effect::{ClapEffectProcessor, ClapScanner, ClapPluginInfo};
pub use parameter_modulation::{ParameterModulator, LfoWaveform};
pub use parameter_automation::ParameterAutomation;
pub use simple_passthrough::SimplePassthroughProcessor;

#[cfg(feature = "debug-overlay")]
pub use performance_overlay::{
    PerformanceOverlayProcessor, PerformanceOverlayInputPorts, PerformanceOverlayOutputPorts,
};
