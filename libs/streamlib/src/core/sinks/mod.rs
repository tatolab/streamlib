//! Sink processors - data consumers
//!
//! Sinks are processors that consume data without producing outputs.
//! They implement the StreamSink trait.
//!
//! All sinks in this module:
//! - Have only inputs, no outputs
//! - Render/output to external systems (displays, speakers, files)
//! - Implement StreamElement + StreamSink traits
//!
//! ## Available Sinks
//!
//! - **DisplayProcessor**: Platform trait for video display output
//! - **AudioOutputProcessor**: Platform trait for speaker audio output

pub mod display;
pub mod audio_output;

pub use display::{DisplayProcessor, WindowId, DisplayInputPorts};
pub use audio_output::{AudioOutputProcessor, AudioDevice, AudioOutputInputPorts};
