// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Audio capture / output processors live in `@tatolab/audio` (#672).

pub mod camera;
pub mod display;
pub mod mp4_writer;
pub mod screen_capture;

pub use camera::AppleCameraProcessor;
pub use display::AppleDisplayProcessor;
pub use mp4_writer::AppleMp4WriterProcessor;
pub use screen_capture::AppleScreenCaptureProcessor;
