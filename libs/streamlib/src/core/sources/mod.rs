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
//! - **CameraProcessor**: Platform trait for camera video capture
//! - **AudioCaptureProcessor**: Platform trait for microphone audio capture

pub mod test_tone_source;
pub mod camera;
pub mod audio_capture;

pub use test_tone_source::{TestToneGenerator, TestToneGeneratorOutputPorts};
pub use camera::{CameraProcessor, CameraDevice, CameraOutputPorts};
pub use audio_capture::{AudioCaptureProcessor, AudioInputDevice, AudioCaptureOutputPorts};
