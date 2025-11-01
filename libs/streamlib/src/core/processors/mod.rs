//! Standard processor traits
//!
//! Defines common processor types (Camera, Display, Audio) that platform implementations
//! provide concrete implementations for.

pub mod camera;
pub mod display;
pub mod audio_output;
pub mod audio_capture;
pub mod audio_effect;
pub mod audio_mixer;
pub mod clap_effect;
pub mod parameter_modulation;
pub mod parameter_automation;
pub mod test_tone;
pub mod simple_passthrough;

#[cfg(feature = "debug-overlay")]
pub mod performance_overlay;

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
pub use test_tone::{TestToneGenerator, TestToneGeneratorOutputPorts};
pub use simple_passthrough::SimplePassthroughProcessor;

#[cfg(feature = "debug-overlay")]
pub use performance_overlay::{
    PerformanceOverlayProcessor, PerformanceOverlayInputPorts, PerformanceOverlayOutputPorts,
};
