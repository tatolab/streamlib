//! Apple sink processors - data consumers
//!
//! Sinks consume data without producing outputs.
//! They implement the StreamSink trait (v2.0.0 architecture).
//!
//! All sinks in this module:
//! - Have only inputs, no outputs
//! - Implement StreamElement + StreamSink traits
//! - May provide pipeline clock (audio, vsync)
//!
//! ## Available Sinks
//!
//! - **AppleDisplayProcessor**: Renders video to NSWindow (provides vsync clock)
//! - **AppleAudioOutputProcessor**: Plays audio to speakers (provides audio clock)

pub mod display;
pub mod audio_output;

pub use display::AppleDisplayProcessor;
pub use audio_output::AppleAudioOutputProcessor;
