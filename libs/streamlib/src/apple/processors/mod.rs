//! Apple platform processor implementations

pub mod camera;
pub mod display;
pub mod audio_output;
pub mod audio_capture;
pub mod sinks;

pub use camera::AppleCameraProcessor;
pub use display::AppleDisplayProcessor;
pub use audio_output::AppleAudioOutputProcessor;
pub use audio_capture::AppleAudioCaptureProcessor;

// Re-export v2.0.0 sink implementations
// Note: These have the same name as legacy implementations above
// The facade layer (libs/streamlib/src/lib.rs) controls which is exposed
pub use sinks::AppleDisplayProcessor as AppleDisplaySink;
pub use sinks::AppleAudioOutputProcessor as AppleAudioOutputSink;
