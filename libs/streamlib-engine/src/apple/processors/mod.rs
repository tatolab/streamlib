// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub mod mp4_writer;
pub mod screen_capture;

// Display processor lives in `@tatolab/display` (#674).

pub use mp4_writer::AppleMp4WriterProcessor;
pub use screen_capture::AppleScreenCaptureProcessor;
