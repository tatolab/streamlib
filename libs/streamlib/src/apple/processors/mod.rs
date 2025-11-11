// Sources
pub mod camera;
pub mod audio_capture;

// Sinks
pub mod display;
pub mod audio_output;

// Source exports
pub use camera::AppleCameraProcessor;
pub use audio_capture::AppleAudioCaptureProcessor;

// Sink exports
pub use display::AppleDisplayProcessor;
pub use audio_output::AppleAudioOutputProcessor;
