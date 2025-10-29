//! Apple platform processor implementations

pub mod camera;
pub mod display;
pub mod audio_output;
pub mod audio_capture;

pub use camera::AppleCameraProcessor;
pub use display::AppleDisplayProcessor;
pub use audio_output::AppleAudioOutputProcessor;
pub use audio_capture::AppleAudioCaptureProcessor;
