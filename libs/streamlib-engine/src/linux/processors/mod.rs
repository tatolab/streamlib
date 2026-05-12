// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub mod bgra_file_source;
pub mod mp4_writer;

// H.264 encoder/decoder live in `@tatolab/h264` (`streamlib-h264`), not
// in the engine (#675).
// H.265 encoder/decoder live in `@tatolab/h265` (`streamlib-h265`), not
// in the engine (#676).

// Display processor lives in `@tatolab/display` (#674).
