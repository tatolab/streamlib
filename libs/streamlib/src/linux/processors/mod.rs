// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Audio capture / output processors live in `@tatolab/audio` (#672).

pub mod bgra_file_source;
pub mod camera;
pub mod display;
pub mod h264_decoder;
pub mod h264_encoder;
pub mod h265_decoder;
pub mod h265_encoder;
pub mod mp4_writer;

pub use camera::LinuxCameraProcessor;
pub use display::LinuxDisplayProcessor;
