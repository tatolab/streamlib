// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Sources
pub mod audio_capture;
pub mod camera;

// Sinks
pub mod audio_output;
pub mod display;
pub mod mp4_writer;

// Note: WebRTC WHIP/WHEP processors are now in core::processors

// Source exports
pub use audio_capture::AppleAudioCaptureProcessor;
pub use camera::AppleCameraProcessor;

// Sink exports
pub use audio_output::AppleAudioOutputProcessor;
pub use display::AppleDisplayProcessor;
pub use mp4_writer::AppleMp4WriterProcessor;
