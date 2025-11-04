//! Transform processors - data processors
//!
//! Transforms are processors that process data from inputs to outputs.
//! They implement the StreamTransform trait.
//!
//! All transforms in this module:
//! - Have both inputs and outputs
//! - Process/modify/combine data streams
//! - Implement StreamElement + StreamTransform traits
//!
//! ## Available Transforms
//!
//! - **AudioEffectProcessor**: Base trait for audio effects
//! - **AudioMixerProcessor**: Mixes multiple audio streams
//! - **ClapEffectProcessor**: CLAP plugin hosting for audio effects
//! - **ParameterModulator**: LFO-based parameter modulation
//! - **ParameterAutomation**: Timeline-based parameter automation
//! - **SimplePassthroughProcessor**: Simple data passthrough for testing
//! - **PerformanceOverlayProcessor**: Video performance metrics overlay (debug feature)

pub mod audio_mixer;
pub mod clap_effect;
pub mod simple_passthrough;

#[cfg(feature = "debug-overlay")]
pub mod performance_overlay;

pub use audio_mixer::{
    AudioMixerProcessor, MixingStrategy, ChannelMode,
    AudioMixerInputPorts, AudioMixerOutputPorts,
    AudioMixerConfig,
};
pub use clap_effect::{
    ClapEffectProcessor, ClapScanner, ClapPluginInfo, ClapEffectConfig,
    ClapEffectInputPorts, ClapEffectOutputPorts,
};
pub use simple_passthrough::SimplePassthroughProcessor;

#[cfg(feature = "debug-overlay")]
pub use performance_overlay::{
    PerformanceOverlayProcessor, PerformanceOverlayInputPorts, PerformanceOverlayOutputPorts,
    PerformanceOverlayConfig,
};
