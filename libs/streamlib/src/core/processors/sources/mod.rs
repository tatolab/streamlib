//! Source processors - data generators
//!
//! Sources are processors that generate data without consuming inputs.
//! They implement the StreamSource trait.
//!
//! All sources in this module:
//! - Have no inputs, only outputs
//! - Run in loops/callbacks (runtime-scheduled)
//! - Implement StreamElement + StreamSource traits
//!
//! ## Available Sources
//!
//! - **TestToneGenerator**: Generates sine wave test tones

pub mod test_tone_source;

pub use test_tone_source::{TestToneGenerator, TestToneGeneratorOutputPorts};
