//! Stream processor organization
//!
//! Processors are organized by their I/O pattern following GStreamer's architecture.
//!
//! **Note:** Processor implementations have been moved to:
//! - `core::sources` - Source processors (no inputs, only outputs)
//! - `core::sinks` - Sink processors (only inputs, no outputs)
//! - `core::transformers` - Transform processors (inputs and outputs)
//!
//! This module re-exports them for backward compatibility.
//!
//! ## Trait Hierarchy
//!
//! ```text
//! StreamElement (base trait)
//!     ├─ StreamSource (generate data)
//!     ├─ StreamSink (consume data)
//!     └─ StreamTransform (process data)
//! ```

// Re-export all processors from their categorized modules at core level
pub use crate::core::sources::{
    TestToneGenerator, TestToneGeneratorOutputPorts,
    CameraProcessor, CameraDevice, CameraOutputPorts,
    AudioCaptureProcessor, AudioInputDevice, AudioCaptureOutputPorts,
};

pub use crate::core::sinks::{
    DisplayProcessor, WindowId, DisplayInputPorts,
    AudioOutputProcessor, AudioDevice, AudioOutputInputPorts,
};

pub use crate::core::transformers::{
    AudioEffectProcessor, ParameterInfo, PluginInfo,
    AudioEffectInputPorts, AudioEffectOutputPorts,
    AudioMixerProcessor, MixingStrategy,
    AudioMixerInputPorts, AudioMixerOutputPorts,
    ClapEffectProcessor, ClapScanner, ClapPluginInfo,
    ParameterModulator, LfoWaveform,
    ParameterAutomation,
    SimplePassthroughProcessor,
};

#[cfg(feature = "debug-overlay")]
pub use crate::core::transformers::{
    PerformanceOverlayProcessor, PerformanceOverlayInputPorts, PerformanceOverlayOutputPorts,
};
