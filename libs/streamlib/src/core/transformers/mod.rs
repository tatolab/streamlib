
pub mod audio_mixer;
pub mod clap_effect;
pub mod simple_passthrough;

#[cfg(feature = "debug-overlay")]
pub mod performance_overlay;

pub use audio_mixer::{
    AudioMixerProcessor, MixingStrategy, AudioMixerConfig,
};
pub use clap_effect::{
    ClapEffectProcessor, ClapScanner, ClapPluginInfo, ClapEffectConfig,
};
pub use simple_passthrough::SimplePassthroughProcessor;

#[cfg(feature = "debug-overlay")]
pub use performance_overlay::{
    PerformanceOverlayProcessor, PerformanceOverlayInputPorts, PerformanceOverlayOutputPorts,
    PerformanceOverlayConfig,
};
