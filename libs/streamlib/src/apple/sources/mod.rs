//! Apple platform source implementations
//!
//! Sources generate data without consuming inputs.
//! They implement the StreamSource trait (v2.0.0 architecture).

pub mod camera;
pub mod audio_capture;

pub use camera::AppleCameraProcessor;
pub use audio_capture::AppleAudioCaptureProcessor;
